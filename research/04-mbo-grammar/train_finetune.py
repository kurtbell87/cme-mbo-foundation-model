"""
Phase 2 (Gate 1.5): Train LM + dual book state reconstruction heads.

Loads Phase 1 checkpoint, adds pre-batch and post-batch reconstruction heads,
trains with combined loss: gamma*L_lm + alpha*L_pre_recon + beta*L_post_recon.

Pass criteria (Gate 1.5):
  - Level-1 size accuracy > unconditional mode + 10pp
  - Spread accuracy on non-modal samples (spread > 1 tick) > 40%
  - Imbalance MAE < 0.15

Usage:
    python train_finetune.py \
        --tokens /data/tokens.bin \
        --book-state /data/tokens.bin.book_state \
        --checkpoint /data/best_model.pt \
        --results-dir /results
"""

import argparse
import json
import math
import os
import time

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.utils.data import DataLoader

from model import MBOTransformer, ReconHead, VOCAB_SIZE, SPECIAL_TOKENS
from data import (
    load_tokens_mmap,
    scan_commit_positions,
    load_book_state_mmap,
    ReconDataset,
    PackedReconDataset,
    StreamingPackedDataset,
)


# ═══════════════════════════════════════════════════════════════
# Target discretization (GPU, vectorized)
# ═══════════════════════════════════════════════════════════════

_SIZE_BOUNDARIES = torch.tensor([1, 2, 4, 8, 16, 32, 64, 128], dtype=torch.long)
_SPECIAL_IDS = torch.tensor(sorted(SPECIAL_TOKENS), dtype=torch.long)

# Device-cached versions (set once in main() to avoid per-step H2D copies)
_SIZE_BOUNDARIES_GPU = None
_SPECIAL_IDS_GPU = None


def discretize_sizes(raw_sizes: torch.Tensor) -> torch.Tensor:
    """Discretize raw order sizes into 9 log2 buckets (single kernel).

    Buckets: [0, 1, 2-3, 4-7, 8-15, 16-31, 32-63, 64-127, 128+]
    """
    return torch.bucketize(raw_sizes.long(), _SIZE_BOUNDARIES_GPU)


def discretize_spread(bid_rel: torch.Tensor, ask_rel: torch.Tensor) -> torch.Tensor:
    """Discretize spread into 11 classes (0=crossed, 1-9=ticks, 10=wide).

    Args:
        bid_rel, ask_rel: (...,) tensors of relative BBO prices in ticks
    Returns:
        (...,) long tensor of spread classes 0-10
    """
    spread = ask_rel - bid_rel
    return spread.round().long().clamp(0, 10)


def compute_imbalance(bid_size_1: torch.Tensor, ask_size_1: torch.Tensor) -> torch.Tensor:
    """Compute level-1 bid/ask imbalance in [0, 1]."""
    total = bid_size_1 + ask_size_1
    return torch.where(total > 0, bid_size_1 / total, 0.5)


# ═══════════════════════════════════════════════════════════════
# Loss functions
# ═══════════════════════════════════════════════════════════════

def masked_lm_loss(logits: torch.Tensor, targets: torch.Tensor) -> torch.Tensor:
    """Cross-entropy loss with SPECIAL token masking (single fused kernel)."""
    loss_per_token = F.cross_entropy(
        logits.reshape(-1, VOCAB_SIZE), targets.reshape(-1), reduction="none"
    ).reshape(targets.shape)
    non_special = ~torch.isin(targets, _SPECIAL_IDS_GPU)
    n_valid = non_special.sum()
    if n_valid == 0:
        return torch.tensor(0.0, device=logits.device, requires_grad=True)
    return (loss_per_token * non_special).sum() / n_valid


def recon_loss(
    head: ReconHead,
    h_commits: torch.Tensor,
    book_state: torch.Tensor,
    mask: torch.Tensor,
) -> torch.Tensor:
    """Compute reconstruction loss for one head (pre or post).

    Fixed-shape computation: runs head on ALL (B*M) positions, masks loss.
    Avoids boolean indexing which creates dynamic shapes and breaks CUDA graphs.

    Args:
        head: ReconHead module
        h_commits: (B, M, D) hidden states gathered at COMMIT positions
        book_state: (B, M, 12) raw book state targets
        mask: (B, M) bool, valid positions (padding + crossed-book filtered)
    Returns:
        scalar loss (mean over valid positions)
    """
    B, M, D = h_commits.shape
    n_valid = mask.sum()
    if n_valid == 0:
        return torch.tensor(0.0, device=h_commits.device, requires_grad=True)

    # Run head on ALL B*M positions (fixed shape for CUDA graphs)
    flat_h = h_commits.reshape(B * M, D)
    size_logits, spread_logits, imb_pred = head(flat_h)

    # Targets (computed on all positions, masked below)
    flat_state = book_state.reshape(B * M, 12)
    flat_mask = mask.reshape(B * M).float()

    # Size: 10 fields, each 9-class CE (masked reduction)
    size_targets = discretize_sizes(flat_state[:, 2:12])  # (B*M, 10)
    size_loss_per = F.cross_entropy(
        size_logits.reshape(-1, ReconHead.N_SIZE_CLASSES),
        size_targets.reshape(-1),
        reduction="none",
    ).reshape(B * M, 10)
    size_loss = (size_loss_per * flat_mask.unsqueeze(1)).sum() / (n_valid * 10)

    # Spread: 11-class CE (masked reduction)
    spread_targets = discretize_spread(flat_state[:, 0], flat_state[:, 1])
    spread_loss_per = F.cross_entropy(spread_logits, spread_targets, reduction="none")
    spread_loss = (spread_loss_per * flat_mask).sum() / n_valid

    # Imbalance: MSE (masked reduction)
    imb_targets = compute_imbalance(flat_state[:, 2], flat_state[:, 7])
    imb_loss_per = (imb_pred - imb_targets).square()
    imb_loss = (imb_loss_per * flat_mask).sum() / n_valid

    return size_loss + spread_loss + imb_loss


# ═══════════════════════════════════════════════════════════════
# Gathering hidden states at COMMIT positions
# ═══════════════════════════════════════════════════════════════

def gather_commit_hidden(h, commit_pos, n_commits):
    """Gather hidden states at COMMIT positions from transformer output.

    Args:
        h: (B, T, D) hidden states
        commit_pos: (B, M) local positions of COMMITs (0-padded)
        n_commits: (B,) actual commit counts

    Returns:
        h_commits: (B, M, D) gathered hidden states
        valid_mask: (B, M) bool mask for non-padding positions
    """
    B, T, D = h.shape
    M = commit_pos.size(1)
    batch_idx = torch.arange(B, device=h.device).unsqueeze(1).expand(-1, M)
    h_commits = h[batch_idx, commit_pos]  # (B, M, D)

    commit_range = torch.arange(M, device=h.device).unsqueeze(0)
    valid_mask = commit_range < n_commits.unsqueeze(1)
    return h_commits, valid_mask


# ═══════════════════════════════════════════════════════════════
# Training
# ═══════════════════════════════════════════════════════════════

def get_device():
    if torch.cuda.is_available():
        return torch.device("cuda")
    if hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
        return torch.device("mps")
    return torch.device("cpu")


def train_epoch(model, pre_head, post_head, dataloader, optimizer, scheduler, device, args,
                amp_dtype=torch.float32):
    """Train one epoch with combined LM + reconstruction loss."""
    model.train()
    pre_head.train()
    post_head.train()

    use_amp = amp_dtype != torch.float32

    # Accumulate on GPU — one sync at epoch end instead of 3 per step
    total_lm = torch.tensor(0.0, device=device)
    total_pre = torch.tensor(0.0, device=device)
    total_post = torch.tensor(0.0, device=device)
    n_batches = 0

    # Build param list once (not every step)
    all_params = (
        list(model.parameters())
        + list(pre_head.parameters())
        + list(post_head.parameters())
    )

    for batch in dataloader:
        x, y, commit_pos, post_state, pre_state, pre_valid_mask, n_commits = [
            b.to(device, non_blocking=True) for b in batch
        ]

        with torch.amp.autocast(device_type=device.type, dtype=amp_dtype, enabled=use_amp):
            # Forward pass
            lm_logits, h = model.forward_with_hidden(x)
            lm_loss = masked_lm_loss(lm_logits, y)

            # Gather hidden states at COMMIT positions
            h_commits, valid_mask = gather_commit_hidden(h, commit_pos, n_commits)

            # Mask crossed books (spread < 0)
            post_spread = post_state[:, :, 1] - post_state[:, :, 0]
            pre_spread = pre_state[:, :, 1] - pre_state[:, :, 0]
            valid_post = valid_mask & (post_spread >= 0)
            valid_pre = valid_mask & pre_valid_mask & (pre_spread >= 0)

            # Reconstruction losses
            post_loss = recon_loss(post_head, h_commits, post_state, valid_post)
            pre_loss = recon_loss(pre_head, h_commits, pre_state, valid_pre)

            loss = args.gamma * lm_loss + args.alpha * pre_loss + args.beta * post_loss

        optimizer.zero_grad(set_to_none=True)
        loss.backward()
        nn.utils.clip_grad_norm_(all_params, 1.0)
        optimizer.step()
        if scheduler is not None:
            scheduler.step()

        total_lm += lm_loss.detach()
        total_pre += pre_loss.detach()
        total_post += post_loss.detach()
        n_batches += 1

    d = max(n_batches, 1)
    return (total_lm / d).item(), (total_pre / d).item(), (total_post / d).item()


# ═══════════════════════════════════════════════════════════════
# Evaluation
# ═══════════════════════════════════════════════════════════════

@torch.inference_mode()
def evaluate_reconstruction(model, head, dataloader, device, name="post",
                            amp_dtype=torch.float32):
    """Evaluate reconstruction accuracy for Gate 1.5.

    Returns dict with size accuracy, spread accuracy, imbalance MAE, etc.
    """
    model.eval()
    head.eval()

    use_amp = amp_dtype != torch.float32
    all_size_preds, all_size_targets = [], []
    all_spread_preds, all_spread_targets = [], []
    all_imb_preds, all_imb_targets = [], []

    for batch in dataloader:
        x, y, commit_pos, post_state, pre_state, pre_valid_mask, n_commits = [
            b.to(device, non_blocking=True) for b in batch
        ]

        with torch.amp.autocast(device_type=device.type, dtype=amp_dtype, enabled=use_amp):
            _, h = model.forward_with_hidden(x)
        h_commits, valid_mask = gather_commit_hidden(h, commit_pos, n_commits)

        if name == "post":
            state = post_state
            spread = state[:, :, 1] - state[:, :, 0]
            mask = valid_mask & (spread >= 0)
        else:
            state = pre_state
            spread = state[:, :, 1] - state[:, :, 0]
            mask = valid_mask & pre_valid_mask & (spread >= 0)

        flat_h = h_commits[mask]
        flat_state = state[mask]
        if flat_h.shape[0] == 0:
            continue

        size_logits, spread_logits, imb_pred = head(flat_h)

        size_targets = discretize_sizes(flat_state[:, 2:12])
        size_preds = size_logits.argmax(dim=-1)

        spread_targets = discretize_spread(flat_state[:, 0], flat_state[:, 1])
        spread_preds = spread_logits.argmax(dim=-1)

        imb_targets = compute_imbalance(flat_state[:, 2], flat_state[:, 7])

        all_size_preds.append(size_preds.cpu())
        all_size_targets.append(size_targets.cpu())
        all_spread_preds.append(spread_preds.cpu())
        all_spread_targets.append(spread_targets.cpu())
        all_imb_preds.append(imb_pred.cpu())
        all_imb_targets.append(imb_targets.cpu())

    if not all_size_preds:
        return {"n_samples": 0}

    size_preds = torch.cat(all_size_preds)
    size_targets = torch.cat(all_size_targets)
    spread_preds = torch.cat(all_spread_preds)
    spread_targets = torch.cat(all_spread_targets)
    imb_preds = torch.cat(all_imb_preds)
    imb_targets = torch.cat(all_imb_targets)

    # Size accuracy: overall and level-1 (fields 0,5 = bid_size_1, ask_size_1)
    size_correct = (size_preds == size_targets).float()
    overall_size_acc = size_correct.mean().item()
    per_field_acc = size_correct.mean(dim=0).tolist()
    level1_acc = size_correct[:, [0, 5]].mean().item()

    # Spread accuracy: overall and non-modal (spread > 1 tick)
    spread_acc = (spread_preds == spread_targets).float().mean().item()
    non_modal = spread_targets > 1
    if non_modal.sum() > 0:
        spread_nonmodal_acc = (
            (spread_preds[non_modal] == spread_targets[non_modal]).float().mean().item()
        )
    else:
        spread_nonmodal_acc = 0.0

    # Imbalance MAE
    imb_mae = (imb_preds - imb_targets).abs().mean().item()

    return {
        "size_acc_overall": overall_size_acc,
        "size_acc_level1": level1_acc,
        "size_acc_per_field": per_field_acc,
        "spread_acc": spread_acc,
        "spread_nonmodal_acc": spread_nonmodal_acc,
        "spread_nonmodal_count": int(non_modal.sum()),
        "imbalance_mae": imb_mae,
        "n_samples": len(size_preds),
    }


@torch.inference_mode()
def compute_unconditional_baselines(dataloader, device):
    """Compute unconditional (mode/mean) baselines from data for Gate 1.5 thresholds."""
    all_sizes, all_spreads, all_imbalances = [], [], []

    for batch in dataloader:
        _, _, commit_pos, post_state, _, _, n_commits = [b.to(device) for b in batch]

        M = commit_pos.size(1)
        commit_range = torch.arange(M, device=device).unsqueeze(0)
        valid = commit_range < n_commits.unsqueeze(1)
        spread = post_state[:, :, 1] - post_state[:, :, 0]
        valid = valid & (spread >= 0)

        flat_state = post_state[valid]
        if flat_state.shape[0] == 0:
            continue

        all_sizes.append(discretize_sizes(flat_state[:, 2:12]).cpu())
        all_spreads.append(discretize_spread(flat_state[:, 0], flat_state[:, 1]).cpu())
        all_imbalances.append(compute_imbalance(flat_state[:, 2], flat_state[:, 7]).cpu())

    sizes = torch.cat(all_sizes)       # (N, 10)
    spreads = torch.cat(all_spreads)   # (N,)
    imbalances = torch.cat(all_imbalances)  # (N,)

    # Mode accuracy per size field
    size_mode_acc = []
    for f in range(10):
        col = sizes[:, f]
        mode_val = col.mode().values.item()
        size_mode_acc.append((col == mode_val).float().mean().item())

    # Level-1 mode accuracy (fields 0 and 5)
    level1_accs = [size_mode_acc[0], size_mode_acc[5]]
    level1_baseline = float(np.mean(level1_accs))

    # Spread mode
    spread_mode = spreads.mode().values.item()
    spread_mode_acc = (spreads == spread_mode).float().mean().item()
    non_modal = spreads > 1
    spread_nonmodal_count = int(non_modal.sum())

    # Imbalance: predict 0.5 always
    imb_mae_half = (imbalances - 0.5).abs().mean().item()

    # Size class distributions (for reporting)
    size_distributions = {}
    for f in range(10):
        col = sizes[:, f]
        counts = torch.bincount(col, minlength=9)
        size_distributions[f"field_{f}"] = (counts.float() / counts.sum()).tolist()

    spread_counts = torch.bincount(spreads, minlength=11)
    spread_distribution = (spread_counts.float() / spread_counts.sum()).tolist()

    return {
        "size_mode_acc_per_field": size_mode_acc,
        "size_mode_acc_level1": level1_baseline,
        "size_distributions": size_distributions,
        "spread_mode_class": int(spread_mode),
        "spread_mode_acc": spread_mode_acc,
        "spread_distribution": spread_distribution,
        "spread_nonmodal_count": spread_nonmodal_count,
        "imbalance_mae_predict_half": imb_mae_half,
        "n_samples": len(sizes),
    }


# ═══════════════════════════════════════════════════════════════
# Checkpoint
# ═══════════════════════════════════════════════════════════════

def save_checkpoint(path, model, pre_head, post_head, optimizer, epoch, metrics, args):
    torch.save(
        {
            "epoch": epoch,
            "model_state_dict": model.state_dict(),
            "pre_head_state_dict": pre_head.state_dict(),
            "post_head_state_dict": post_head.state_dict(),
            "optimizer_state_dict": optimizer.state_dict(),
            "metrics": metrics,
            "args": vars(args),
        },
        path,
    )


# ═══════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════

def main():
    parser = argparse.ArgumentParser(description="Phase 2: Gate 1.5 — LM + reconstruction")
    parser.add_argument("--tokens", default=None, help="tokens.bin path (raw mode)")
    parser.add_argument("--book-state", default=None, help=".book_state sidecar path (raw mode)")
    parser.add_argument("--packed-train", default=None, help="Pre-packed train file (packed mode)")
    parser.add_argument("--packed-val", default=None, help="Pre-packed val file (packed mode)")
    parser.add_argument("--checkpoint", required=True, help="Phase 1 best_model.pt")
    parser.add_argument("--results-dir", default="results")
    parser.add_argument("--epochs", type=int, default=10)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--context-len", type=int, default=512)
    parser.add_argument("--stride", type=int, default=2048)
    parser.add_argument("--lr", type=float, default=3e-4)
    parser.add_argument("--alpha", type=float, default=0.1, help="Pre-recon loss weight")
    parser.add_argument("--beta", type=float, default=0.1, help="Post-recon loss weight")
    parser.add_argument("--gamma", type=float, default=1.0, help="LM loss weight")
    parser.add_argument("--train-frac", type=float, default=0.8)
    parser.add_argument("--max-commits", type=int, default=200)
    parser.add_argument("--num-workers", type=int, default=4)
    parser.add_argument("--no-amp", action="store_true", help="Disable automatic mixed precision")
    args = parser.parse_args()

    torch.set_float32_matmul_precision("high")
    device = get_device()
    print(f"Device: {device}")

    # Move constant tensors to device once (avoid per-step H2D copies + compile graph breaks)
    global _SIZE_BOUNDARIES_GPU, _SPECIAL_IDS_GPU
    _SIZE_BOUNDARIES_GPU = _SIZE_BOUNDARIES.to(device)
    _SPECIAL_IDS_GPU = _SPECIAL_IDS.to(device)

    # ── Load data ───────────────────────────────────────────────
    packed_mode = args.packed_train is not None and args.packed_val is not None
    raw_mode = args.tokens is not None and args.book_state is not None

    if not packed_mode and not raw_mode:
        parser.error("Provide either --packed-train/--packed-val or --tokens/--book-state")

    use_pin = device.type == "cuda"

    if packed_mode:
        print("Using streaming packed datasets (chunked sequential I/O)")
        train_ds = StreamingPackedDataset(args.packed_train, batch_size=args.batch_size,
                                          chunk_size=4096)
        val_ds = StreamingPackedDataset(args.packed_val, batch_size=args.batch_size,
                                        chunk_size=4096)
    else:
        tokens = load_tokens_mmap(args.tokens)
        commit_positions = scan_commit_positions(args.tokens)
        book_state = load_book_state_mmap(args.book_state)

        n_commits = len(commit_positions)
        n_book = len(book_state)
        if n_commits != n_book:
            delta = abs(n_commits - n_book)
            assert delta <= 2, (
                f"COMMIT positions ({n_commits}) and book_state rows ({n_book}) "
                f"differ by {delta} — expected <=2 (boundary one-sided books)"
            )
            n_min = min(n_commits, n_book)
            print(f"WARNING: trimming COMMITs {n_commits:,} -> {n_min:,} "
                  f"(book_state has {n_book:,} rows, delta={delta})")
            commit_positions = commit_positions[:n_min]
            book_state = book_state[:n_min]
        print(f"Sidecar alignment OK: {len(commit_positions):,} COMMITs")

        split_idx = int(len(tokens) * args.train_frac)
        print(f"Temporal split at token {split_idx:,} / {len(tokens):,} "
              f"({args.train_frac:.0%} train)")

        train_ds = ReconDataset(
            tokens, commit_positions, book_state,
            start=0, end=split_idx,
            context_len=args.context_len, stride=args.stride,
            max_commits=args.max_commits,
        )
        val_ds = ReconDataset(
            tokens, commit_positions, book_state,
            start=split_idx, end=len(tokens),
            context_len=args.context_len, stride=args.stride,
            max_commits=args.max_commits,
        )

    persist = args.num_workers > 0
    is_streaming = isinstance(train_ds, StreamingPackedDataset)

    def worker_init_fn(worker_id):
        np.random.seed(np.random.get_state()[1][0] + worker_id)

    if is_streaming:
        # Pre-batched: DataLoader just passes through yielded batch tuples
        train_dl = DataLoader(
            train_ds, batch_size=None,
            num_workers=args.num_workers, pin_memory=use_pin,
            persistent_workers=persist,
            worker_init_fn=worker_init_fn,
        )
        val_dl = DataLoader(
            val_ds, batch_size=None,
            num_workers=args.num_workers, pin_memory=use_pin,
            persistent_workers=persist,
            worker_init_fn=worker_init_fn,
        )
    else:
        train_dl = DataLoader(
            train_ds, batch_size=args.batch_size, shuffle=True,
            num_workers=args.num_workers, pin_memory=use_pin,
            persistent_workers=persist, drop_last=True,
            worker_init_fn=worker_init_fn,
        )
        val_dl = DataLoader(
            val_ds, batch_size=args.batch_size, shuffle=False,
            num_workers=args.num_workers, pin_memory=use_pin,
            persistent_workers=persist,
            worker_init_fn=worker_init_fn,
        )

    # ── Load Phase 1 model ─────────────────────────────────────
    ckpt = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
    ckpt_args = ckpt["args"]

    model = MBOTransformer(
        vocab_size=VOCAB_SIZE,
        d_model=ckpt_args["d_model"],
        nhead=ckpt_args["n_heads"],
        num_layers=ckpt_args["n_layers"],
        dim_ff=ckpt_args["dim_ff"],
        max_seq_len=args.context_len,
        dropout=0.1,
    ).to(device)

    # Remap Phase 1 checkpoint (nn.TransformerEncoder → CausalBlock), pad vocab 126→128
    state = ckpt["model_state_dict"]
    remapped = {}
    for k, v in state.items():
        if k.startswith("transformer.norm"):
            continue  # Drop TransformerEncoder's optional norm layer
        new_k = k
        # TransformerEncoder layers → ModuleList
        new_k = new_k.replace("transformer.layers.", "layers.")
        # nn.MultiheadAttention → CausalBlock attention
        new_k = new_k.replace(".self_attn.in_proj_weight", ".in_proj.weight")
        new_k = new_k.replace(".self_attn.in_proj_bias", ".in_proj.bias")
        new_k = new_k.replace(".self_attn.out_proj.", ".out_proj.")
        # FFN: linear1/2 → ff.0/2
        new_k = new_k.replace(".linear1.", ".ff.0.")
        new_k = new_k.replace(".linear2.", ".ff.2.")
        # LayerNorm: norm1/2 → ln1/2
        new_k = new_k.replace(".norm1.", ".ln1.")
        new_k = new_k.replace(".norm2.", ".ln2.")
        remapped[new_k] = v
    # Pad embedding from 126 to 128 if needed (tensor core alignment)
    if remapped["tok_emb.weight"].shape[0] < VOCAB_SIZE:
        old_emb = remapped["tok_emb.weight"]
        new_emb = torch.zeros(VOCAB_SIZE, old_emb.shape[1])
        new_emb[:old_emb.shape[0]] = old_emb
        remapped["tok_emb.weight"] = new_emb
        remapped.pop("head.weight", None)  # Tied, set by weight tying
    model.load_state_dict(remapped, strict=False)

    n_backbone = model.count_parameters()
    print(f"Loaded Phase 1 checkpoint: epoch {ckpt['epoch']}, "
          f"ppl {ckpt['val_ppl']:.4f}, {n_backbone:,} params")

    # ── Reconstruction heads ───────────────────────────────────
    d_model = ckpt_args["d_model"]
    pre_head = ReconHead(d_model).to(device)
    post_head = ReconHead(d_model).to(device)

    n_pre = pre_head.count_parameters()
    n_post = post_head.count_parameters()
    print(f"Reconstruction heads: pre={n_pre:,} + post={n_post:,} = {n_pre + n_post:,} params")
    print(f"Total trainable: {n_backbone + n_pre + n_post:,} params")

    # ── AMP setup ─────────────────────────────────────────────
    use_amp = (not args.no_amp) and device.type == "cuda"
    amp_dtype = torch.bfloat16 if use_amp else torch.float32
    if use_amp:
        print(f"AMP enabled: {amp_dtype}")
    else:
        print("AMP disabled (fp32)")

    # ── Optimizer ──────────────────────────────────────────────
    all_params = (
        list(model.parameters())
        + list(pre_head.parameters())
        + list(post_head.parameters())
    )
    use_fused = device.type == "cuda"
    optimizer = torch.optim.AdamW(all_params, lr=args.lr, weight_decay=0.01,
                                  fused=use_fused)
    total_steps = len(train_dl) * args.epochs
    print(f"Scheduler: {len(train_dl)} steps/epoch x {args.epochs} epochs = {total_steps} total steps")
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, total_steps)

    os.makedirs(args.results_dir, exist_ok=True)

    # ── Unconditional baselines (Gate 1.5 Step 0) ─────────────
    print("\nComputing unconditional baselines on val split...")
    baselines = compute_unconditional_baselines(val_dl, device)
    print(f"  Size mode accuracy (level 1):  {baselines['size_mode_acc_level1']:.1%}")
    print(f"  Spread mode class:             {baselines['spread_mode_class']} "
          f"(accuracy: {baselines['spread_mode_acc']:.1%})")
    print(f"  Spread non-modal count:        {baselines['spread_nonmodal_count']:,}")
    print(f"  Imbalance MAE (predict 0.5):   {baselines['imbalance_mae_predict_half']:.4f}")

    # Save baselines
    with open(os.path.join(args.results_dir, "baselines.json"), "w") as f:
        json.dump(baselines, f, indent=2)

    # ── Training loop ──────────────────────────────────────────
    best_val_level1 = 0.0
    best_ckpt_path = os.path.join(args.results_dir, "best_model.pt")

    print(f"\n{'ep':>3} {'lm':>7} {'pre':>7} {'post':>7} "
          f"{'sz1_acc':>8} {'spr_acc':>8} {'spr_nm':>7} {'imb_mae':>8} {'time':>6}")
    print("-" * 72)

    for epoch in range(1, args.epochs + 1):
        t0 = time.time()

        lm_l, pre_l, post_l = train_epoch(
            model, pre_head, post_head, train_dl, optimizer, scheduler, device, args,
            amp_dtype=amp_dtype,
        )

        post_m = evaluate_reconstruction(model, post_head, val_dl, device, "post",
                                         amp_dtype=amp_dtype)
        # Pre-head eval only at final epoch (saves ~15% wall time)
        is_final = epoch == args.epochs
        if is_final:
            pre_m = evaluate_reconstruction(model, pre_head, val_dl, device, "pre",
                                            amp_dtype=amp_dtype)
        else:
            pre_m = None

        dt = time.time() - t0

        print(
            f"{epoch:>3} {lm_l:>7.4f} {pre_l:>7.4f} {post_l:>7.4f} "
            f"{post_m['size_acc_level1']:>8.1%} {post_m['spread_acc']:>8.1%} "
            f"{post_m['spread_nonmodal_acc']:>7.1%} {post_m['imbalance_mae']:>8.4f} "
            f"{dt:>5.0f}s"
        )

        if post_m["size_acc_level1"] > best_val_level1:
            best_val_level1 = post_m["size_acc_level1"]
            save_checkpoint(
                best_ckpt_path, model, pre_head, post_head, optimizer, epoch,
                {"post": post_m, "pre": pre_m}, args,
            )

    # ── Gate 1.5 verdict ───────────────────────────────────────
    print(f"\n{'=' * 50}")
    print(f"GATE 1.5 EVALUATION")
    print(f"{'=' * 50}")

    # Reload best
    ckpt = torch.load(best_ckpt_path, map_location=device, weights_only=False)
    model.load_state_dict(ckpt["model_state_dict"])
    pre_head.load_state_dict(ckpt["pre_head_state_dict"])
    post_head.load_state_dict(ckpt["post_head_state_dict"])

    post_m = evaluate_reconstruction(model, post_head, val_dl, device, "post",
                                     amp_dtype=amp_dtype)
    pre_m = evaluate_reconstruction(model, pre_head, val_dl, device, "pre",
                                    amp_dtype=amp_dtype)

    size_delta = post_m["size_acc_level1"] - baselines["size_mode_acc_level1"]
    spread_nm = post_m["spread_nonmodal_acc"]
    imb_mae = post_m["imbalance_mae"]

    print(f"\nPost-batch reconstruction (best epoch {ckpt['epoch']}):")
    print(f"  Size accuracy (level 1): {post_m['size_acc_level1']:.1%} "
          f"(baseline: {baselines['size_mode_acc_level1']:.1%}, "
          f"delta: {size_delta:+.1%})")
    print(f"  Spread non-modal acc:    {spread_nm:.1%} "
          f"(threshold: >40%)")
    print(f"  Imbalance MAE:           {imb_mae:.4f} "
          f"(baseline: {baselines['imbalance_mae_predict_half']:.4f}, "
          f"threshold: <0.15)")

    print(f"\nPre-batch reconstruction:")
    print(f"  Size accuracy (level 1): {pre_m['size_acc_level1']:.1%}")
    print(f"  Spread non-modal acc:    {pre_m['spread_nonmodal_acc']:.1%}")
    print(f"  Imbalance MAE:           {pre_m['imbalance_mae']:.4f}")

    size_pass = size_delta >= 0.10
    spread_pass = spread_nm >= 0.40
    imb_pass = imb_mae < 0.15
    gate_passed = size_pass and spread_pass and imb_pass

    print(f"\n{'=' * 50}")
    if gate_passed:
        print("GATE 1.5: PASS")
        print("Model recovers book state from event context.")
        print("Proceed to Phase 3 (directional signal check).")
    else:
        failures = []
        if not size_pass:
            failures.append(f"size delta {size_delta:+.1%} < +10%")
        if not spread_pass:
            failures.append(f"spread non-modal {spread_nm:.1%} < 40%")
        if not imb_pass:
            failures.append(f"imbalance MAE {imb_mae:.4f} >= 0.15")
        print(f"GATE 1.5: FAIL — {', '.join(failures)}")
        print("Model cannot recover book state. Directional experiment is pointless.")
    print(f"{'=' * 50}")

    # ── Save results ───────────────────────────────────────────
    results = {
        "gate": "gate1.5",
        "passed": gate_passed,
        "best_epoch": ckpt["epoch"],
        "post_metrics": post_m,
        "pre_metrics": pre_m,
        "baselines": baselines,
        "checks": {
            "size_delta_pp": round(size_delta * 100, 1),
            "size_pass": size_pass,
            "spread_nonmodal_pct": round(spread_nm * 100, 1),
            "spread_pass": spread_pass,
            "imbalance_mae": round(imb_mae, 4),
            "imbalance_pass": imb_pass,
        },
        "loss_weights": {"alpha": args.alpha, "beta": args.beta, "gamma": args.gamma},
        "args": vars(args),
    }
    results_path = os.path.join(args.results_dir, "gate1_5_results.json")
    with open(results_path, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nResults saved to {results_path}")


if __name__ == "__main__":
    main()
