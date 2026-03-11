"""
Phase 3: Signal Check — Minimum viable directional test.

Conditions:
  B: 8M pretrained (Phase 2 checkpoint) + dual recon heads + directional head
  C: 8M random init + directional head only

Target: Next BBO change direction (binary)
Validation: 15-fold CPCV (6 groups, 2 test)

Kill gate: B beats majority class (F) by >= 2pp in >= 60% of folds.
If not, MBO sequences do not predict direction on MES. Stop.

Prerequisite: run precompute_phase3.py to create commit_positions.bin and
direction_targets.bin from tokens.bin + book_state. Direction is based on
tradeable BBO prices (bid_rel + ask_rel), not mid-price.

Usage (condition B):
    python phase3_signal_check.py --condition B \
        --tokens /data/tokens.bin \
        --commit-positions /data/commit_positions.bin \
        --direction-targets /data/direction_targets.bin \
        --book-state /data/tokens.bin.book_state \
        --checkpoint /data/best_model.pt \
        --results-dir /results

Usage (condition C — no book_state needed):
    python phase3_signal_check.py --condition C \
        --tokens /data/tokens.bin \
        --commit-positions /data/commit_positions.bin \
        --direction-targets /data/direction_targets.bin \
        --results-dir /results
"""

import argparse
import json
import os
import time
from itertools import combinations

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.utils.data import Dataset, DataLoader, ConcatDataset

from model import (
    MBOTransformer, ReconHead, DirectionalHead,
    VOCAB_SIZE, SPECIAL_TOKENS, COMMIT,
)
from data import load_tokens_mmap, load_book_state_mmap

# ═══════════════════════════════════════════════════════════════
# Constants
# ═══════════════════════════════════════════════════════════════

N_GROUPS = 6
N_TEST_GROUPS = 2
COMMIT_TOKEN = 3
BUFFER_TOKENS = 30000  # purge + embargo zone near test boundaries

# Copied from train_finetune.py (avoid import of mutable globals)
_SIZE_BOUNDARIES = torch.tensor([1, 2, 4, 8, 16, 32, 64, 128], dtype=torch.long)
_SPECIAL_IDS = torch.tensor(sorted(SPECIAL_TOKENS), dtype=torch.long)
_SIZE_BOUNDARIES_GPU = None
_SPECIAL_IDS_GPU = None


# ═══════════════════════════════════════════════════════════════
# Dataset
# ═══════════════════════════════════════════════════════════════

class DirectionalReconDataset(Dataset):
    """Sliding window dataset with direction + book state targets at COMMIT positions."""

    def __init__(self, tokens_mmap, commit_positions, book_state_mmap, direction_targets,
                 start, end, context_len=512, stride=8192, max_commits=200):
        self.tokens = tokens_mmap
        self.commit_positions = commit_positions
        self.book_state = book_state_mmap
        self.direction_targets = direction_targets
        self.context_len = context_len
        self.max_commits = max_commits

        max_start = min(end, len(tokens_mmap)) - context_len - 1
        if max_start < start:
            self.starts = np.array([], dtype=np.int64)
        else:
            self.starts = np.arange(start, max_start + 1, stride, dtype=np.int64)

    def __len__(self):
        return len(self.starts)

    def __getitem__(self, idx):
        start = self.starts[idx]
        chunk = np.array(self.tokens[start:start + self.context_len + 1], dtype=np.int64)
        x = torch.from_numpy(chunk[:-1])
        y = torch.from_numpy(chunk[1:])

        local_commits = np.where(chunk[:-1] == COMMIT_TOKEN)[0]

        n = 0
        sidecar_rows = np.empty(0, dtype=np.int64)
        if len(local_commits) > 0:
            global_pos = (start + local_commits).astype(np.int64)
            sidecar_rows = np.searchsorted(self.commit_positions, global_pos)
            valid = sidecar_rows < len(self.commit_positions)
            valid[valid] &= self.commit_positions[sidecar_rows[valid]] == global_pos[valid]
            local_commits = local_commits[valid]
            sidecar_rows = sidecar_rows[valid]
            n = min(len(local_commits), self.max_commits)

        cp = np.zeros(self.max_commits, dtype=np.int64)
        post = np.zeros((self.max_commits, 12), dtype=np.float32)
        pre = np.zeros((self.max_commits, 12), dtype=np.float32)
        pre_valid = np.zeros(self.max_commits, dtype=np.bool_)
        dir_tgt = np.full(self.max_commits, -1, dtype=np.int64)

        if n > 0:
            rows = sidecar_rows[:n]
            cp[:n] = local_commits[:n]

            if self.book_state is not None:
                post[:n] = np.array(self.book_state[rows])
                pre_rows = rows - 1
                has_pre = pre_rows >= 0
                pre_valid[:n] = has_pre
                safe_rows = np.where(has_pre, pre_rows, 0)
                pre[:n] = np.array(self.book_state[safe_rows])
                pre[:n][~has_pre] = 0.0

            dir_tgt[:n] = self.direction_targets[rows].astype(np.int64)

        return (
            x, y,
            torch.from_numpy(cp),
            torch.from_numpy(post),
            torch.from_numpy(pre),
            torch.from_numpy(pre_valid),
            torch.tensor(n, dtype=torch.long),
            torch.from_numpy(dir_tgt),
        )


# ═══════════════════════════════════════════════════════════════
# CPCV fold generation
# ═══════════════════════════════════════════════════════════════

def generate_folds(n_tokens, n_groups=N_GROUPS, n_test_groups=N_TEST_GROUPS, buffer=BUFFER_TOKENS):
    """Generate CPCV fold token ranges.

    Returns list of (train_ranges, test_ranges) where each is a list of (start, end) tuples.
    """
    tpg = n_tokens // n_groups
    group_bounds = [(g * tpg, min((g + 1) * tpg, n_tokens)) for g in range(n_groups)]

    folds = []
    for test_groups in combinations(range(n_groups), n_test_groups):
        test_set = set(test_groups)

        test_ranges = [group_bounds[g] for g in test_groups]

        train_ranges = []
        for g in range(n_groups):
            if g in test_set:
                continue
            s, e = group_bounds[g]

            # Trim edges adjacent to test groups (purge + embargo zone)
            for tg in test_groups:
                ts, te = group_bounds[tg]
                if te == s:  # test group directly before this train group
                    s = min(s + buffer, e)
                if ts == e:  # test group directly after this train group
                    e = max(e - buffer, s)

            if s < e:
                train_ranges.append((s, e))

        folds.append((train_ranges, test_ranges))

    return folds


def make_datasets(tokens, commit_positions, book_state, direction_targets,
                  ranges, context_len, stride, max_commits):
    """Create a ConcatDataset from multiple token ranges."""
    datasets = []
    total_windows = 0
    for s, e in ranges:
        ds = DirectionalReconDataset(
            tokens, commit_positions, book_state, direction_targets,
            start=s, end=e, context_len=context_len, stride=stride,
            max_commits=max_commits,
        )
        total_windows += len(ds)
        datasets.append(ds)
    if not datasets:
        return None, 0
    return ConcatDataset(datasets), total_windows


# ═══════════════════════════════════════════════════════════════
# Loss functions (duplicated from train_finetune.py to avoid global state issues)
# ═══════════════════════════════════════════════════════════════

def discretize_sizes(raw_sizes):
    return torch.bucketize(raw_sizes.long(), _SIZE_BOUNDARIES_GPU)

def discretize_spread(bid_rel, ask_rel):
    return (ask_rel - bid_rel).round().long().clamp(0, 10)

def compute_imbalance(bid_size_1, ask_size_1):
    total = bid_size_1 + ask_size_1
    return torch.where(total > 0, bid_size_1 / total, 0.5)

def masked_lm_loss(logits, targets):
    loss_per_token = F.cross_entropy(
        logits.reshape(-1, VOCAB_SIZE), targets.reshape(-1), reduction="none"
    ).reshape(targets.shape)
    non_special = ~torch.isin(targets, _SPECIAL_IDS_GPU)
    n_valid = non_special.sum()
    if n_valid == 0:
        return torch.tensor(0.0, device=logits.device, requires_grad=True)
    return (loss_per_token * non_special).sum() / n_valid

def recon_loss(head, h_commits, book_state, mask):
    B, M, D = h_commits.shape
    n_valid = mask.sum()
    if n_valid == 0:
        return torch.tensor(0.0, device=h_commits.device, requires_grad=True)

    flat_h = h_commits.reshape(B * M, D)
    size_logits, spread_logits, imb_pred = head(flat_h)

    flat_state = book_state.reshape(B * M, 12)
    flat_mask = mask.reshape(B * M).float()

    size_targets = discretize_sizes(flat_state[:, 2:12])
    size_loss_per = F.cross_entropy(
        size_logits.reshape(-1, ReconHead.N_SIZE_CLASSES),
        size_targets.reshape(-1), reduction="none",
    ).reshape(B * M, 10)
    size_loss = (size_loss_per * flat_mask.unsqueeze(1)).sum() / (n_valid * 10)

    spread_targets = discretize_spread(flat_state[:, 0], flat_state[:, 1])
    spread_loss_per = F.cross_entropy(spread_logits, spread_targets, reduction="none")
    spread_loss = (spread_loss_per * flat_mask).sum() / n_valid

    imb_targets = compute_imbalance(flat_state[:, 2], flat_state[:, 7])
    imb_loss_per = (imb_pred - imb_targets).square()
    imb_loss = (imb_loss_per * flat_mask).sum() / n_valid

    return size_loss + spread_loss + imb_loss


def gather_commit_hidden(h, commit_pos, n_commits):
    B, T, D = h.shape
    M = commit_pos.size(1)
    batch_idx = torch.arange(B, device=h.device).unsqueeze(1).expand(-1, M)
    h_commits = h[batch_idx, commit_pos]
    commit_range = torch.arange(M, device=h.device).unsqueeze(0)
    valid_mask = commit_range < n_commits.unsqueeze(1)
    return h_commits, valid_mask


# ═══════════════════════════════════════════════════════════════
# Model creation + checkpoint loading
# ═══════════════════════════════════════════════════════════════

def create_model_B(ckpt_path, device, d_model):
    """Create condition B model from Phase 2 checkpoint."""
    ckpt = torch.load(ckpt_path, map_location="cpu", weights_only=False)
    ckpt_args = ckpt.get("args", {})

    model = MBOTransformer(
        vocab_size=VOCAB_SIZE,
        d_model=ckpt_args.get("d_model", d_model),
        nhead=ckpt_args.get("n_heads", 8),
        num_layers=ckpt_args.get("n_layers", 8),
        dim_ff=ckpt_args.get("dim_ff", 1024),
        max_seq_len=512,
        dropout=0.1,
    ).to(device)
    model.load_state_dict(ckpt["model_state_dict"])

    pre_head = ReconHead(d_model).to(device)
    post_head = ReconHead(d_model).to(device)
    pre_head.load_state_dict(ckpt["pre_head_state_dict"])
    post_head.load_state_dict(ckpt["post_head_state_dict"])

    dir_head = DirectionalHead(d_model).to(device)

    return model, pre_head, post_head, dir_head


def create_model_C(device, d_model):
    """Create condition C model (random init, no recon heads)."""
    model = MBOTransformer(
        vocab_size=VOCAB_SIZE,
        d_model=d_model,
        nhead=8,
        num_layers=8,
        dim_ff=1024,
        max_seq_len=512,
        dropout=0.1,
    ).to(device)

    dir_head = DirectionalHead(d_model).to(device)

    return model, None, None, dir_head


# ═══════════════════════════════════════════════════════════════
# Training loop (one fold, one condition)
# ═══════════════════════════════════════════════════════════════

def train_fold(model, dir_head, pre_head, post_head, train_dl, device,
               condition, epochs, lr, amp_dtype):
    """Train one condition for one fold. Returns final train direction loss."""
    model.train()
    dir_head.train()

    params = list(model.parameters()) + list(dir_head.parameters())
    if condition == "B" and pre_head is not None:
        pre_head.train()
        post_head.train()
        params += list(pre_head.parameters()) + list(post_head.parameters())

    use_fused = device.type == "cuda"
    optimizer = torch.optim.AdamW(params, lr=lr, weight_decay=0.01, fused=use_fused)
    total_steps = len(train_dl) * epochs
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, total_steps)

    use_amp = amp_dtype != torch.float32

    total_batches_per_epoch = len(train_dl)
    t_train_start = time.time()

    for epoch in range(1, epochs + 1):
        model.train()
        dir_head.train()
        if condition == "B":
            pre_head.train()
            post_head.train()

        epoch_dir_loss = torch.tensor(0.0, device=device)
        n_batches = 0
        t_epoch = time.time()

        for batch in train_dl:
            x, y, commit_pos, post_state, pre_state, pre_valid_mask, n_commits, dir_targets = [
                b.to(device, non_blocking=True) for b in batch
            ]

            with torch.amp.autocast(device_type=device.type, dtype=amp_dtype, enabled=use_amp):
                if condition == "B":
                    lm_logits, h = model.forward_with_hidden(x)
                else:
                    h = model.forward_hidden(x)

                h_commits, valid_mask = gather_commit_hidden(h, commit_pos, n_commits)

                # Direction loss (fixed-shape computation)
                B, M, D = h_commits.shape
                flat_h = h_commits.reshape(B * M, D)
                flat_dir = dir_targets.reshape(B * M)
                flat_valid = valid_mask.reshape(B * M)

                dir_logits = dir_head(flat_h)
                dir_valid = flat_valid & (flat_dir >= 0)
                n_dir = dir_valid.sum()

                if n_dir > 0:
                    dir_loss_per = F.binary_cross_entropy_with_logits(
                        dir_logits, flat_dir.float().clamp(0, 1), reduction='none'
                    )
                    dir_loss = (dir_loss_per * dir_valid.float()).sum() / n_dir
                else:
                    dir_loss = torch.tensor(0.0, device=device, requires_grad=True)

                loss = dir_loss

                if condition == "B":
                    lm_loss = masked_lm_loss(lm_logits, y)

                    post_spread = post_state[:, :, 1] - post_state[:, :, 0]
                    pre_spread = pre_state[:, :, 1] - pre_state[:, :, 0]
                    valid_post = valid_mask & (post_spread >= 0)
                    valid_pre = valid_mask & pre_valid_mask & (pre_spread >= 0)

                    post_l = recon_loss(post_head, h_commits, post_state, valid_post)
                    pre_l = recon_loss(pre_head, h_commits, pre_state, valid_pre)

                    loss = dir_loss + 0.1 * pre_l + 0.1 * post_l + 0.01 * lm_loss

            optimizer.zero_grad(set_to_none=True)
            loss.backward()
            nn.utils.clip_grad_norm_(params, 1.0)
            optimizer.step()
            scheduler.step()

            epoch_dir_loss += dir_loss.detach()
            n_batches += 1

            if n_batches <= 3 or n_batches % 50 == 0:
                elapsed = time.time() - t_epoch
                batch_rate = n_batches / elapsed if elapsed > 0 else 0
                print(f"    ep{epoch} batch {n_batches}/{total_batches_per_epoch} "
                      f"dir_loss={dir_loss.item():.4f} "
                      f"rate={batch_rate:.1f} b/s", flush=True)

        epoch_elapsed = time.time() - t_epoch
        print(f"    ep{epoch} done: {n_batches} batches in {epoch_elapsed:.0f}s "
              f"({n_batches/epoch_elapsed:.1f} b/s)", flush=True)

    total_elapsed = time.time() - t_train_start
    print(f"    training done: {total_elapsed:.0f}s total", flush=True)
    return (epoch_dir_loss / max(n_batches, 1)).item()


# ═══════════════════════════════════════════════════════════════
# Evaluation
# ═══════════════════════════════════════════════════════════════

@torch.inference_mode()
def evaluate_direction(model, dir_head, dataloader, device, amp_dtype):
    """Evaluate directional accuracy on test set."""
    model.eval()
    dir_head.eval()
    use_amp = amp_dtype != torch.float32

    all_preds = []
    all_targets = []

    for batch in dataloader:
        x, y, commit_pos, post_state, pre_state, pre_valid_mask, n_commits, dir_targets = [
            b.to(device, non_blocking=True) for b in batch
        ]

        with torch.amp.autocast(device_type=device.type, dtype=amp_dtype, enabled=use_amp):
            h = model.forward_hidden(x)

        h_commits, valid_mask = gather_commit_hidden(h, commit_pos, n_commits)

        B, M, D = h_commits.shape
        flat_h = h_commits.reshape(B * M, D)
        flat_dir = dir_targets.reshape(B * M)
        flat_valid = valid_mask.reshape(B * M)

        dir_logits = dir_head(flat_h)
        dir_preds = (dir_logits > 0).long()

        mask = flat_valid & (flat_dir >= 0)
        all_preds.append(dir_preds[mask].cpu())
        all_targets.append(flat_dir[mask].cpu())

    if not all_preds:
        return {"accuracy": 0.0, "majority_class_acc": 0.5, "n_samples": 0,
                "beats_majority_pp": 0.0}

    preds = torch.cat(all_preds)
    targets = torch.cat(all_targets)

    accuracy = (preds == targets).float().mean().item()
    n_up = (targets == 1).sum().item()
    n_down = (targets == 0).sum().item()
    n_total = n_up + n_down
    majority_pct = max(n_up, n_down) / n_total if n_total > 0 else 0.5

    return {
        "accuracy": accuracy,
        "majority_class_acc": majority_pct,
        "n_samples": n_total,
        "n_up": n_up,
        "n_down": n_down,
        "beats_majority_pp": (accuracy - majority_pct) * 100,
    }


# ═══════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════

def main():
    parser = argparse.ArgumentParser(description="Phase 3: Signal Check")
    parser.add_argument("--tokens", required=True, help="tokens.bin path")
    parser.add_argument("--commit-positions", required=True, help="precomputed commit_positions.bin (uint64)")
    parser.add_argument("--direction-targets", required=True, help="precomputed direction_targets.bin (int8)")
    parser.add_argument("--book-state", default=None, help=".book_state sidecar path (required for condition B)")
    parser.add_argument("--checkpoint", default=None, help="Phase 2 best_model.pt (required for condition B)")
    parser.add_argument("--condition", required=True, choices=["B", "C"], help="Which condition to run")
    parser.add_argument("--results-dir", default="results")
    parser.add_argument("--epochs", type=int, default=5)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--context-len", type=int, default=512)
    parser.add_argument("--stride", type=int, default=8192)
    parser.add_argument("--lr-b", type=float, default=5e-5, help="LR for condition B (fine-tuning)")
    parser.add_argument("--lr-c", type=float, default=3e-4, help="LR for condition C (from scratch)")
    parser.add_argument("--max-commits", type=int, default=200)
    parser.add_argument("--num-workers", type=int, default=4)
    parser.add_argument("--no-amp", action="store_true")
    parser.add_argument("--d-model", type=int, default=256)
    args = parser.parse_args()

    if args.condition == "B" and not args.checkpoint:
        parser.error("--checkpoint is required for condition B")
    if args.condition == "B" and not args.book_state:
        parser.error("--book-state is required for condition B")

    torch.set_float32_matmul_precision("high")
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    print(f"Device: {device}, Condition: {args.condition}")

    global _SIZE_BOUNDARIES_GPU, _SPECIAL_IDS_GPU
    _SIZE_BOUNDARIES_GPU = _SIZE_BOUNDARIES.to(device)
    _SPECIAL_IDS_GPU = _SPECIAL_IDS.to(device)

    amp_dtype = torch.bfloat16 if device.type == "cuda" and not args.no_amp else torch.float32
    print(f"AMP: {amp_dtype}")

    # ── Load data ───────────────────────────────────────────────
    # Memory strategy: ALL data is memory-mapped. Zero RAM arrays.
    # commit_positions + direction_targets precomputed to disk by precompute_phase3.py.
    # For condition C, book_state is not loaded (saves 33.4 GB mmap pressure).
    print("\n=== Loading data (all mmap) ===")
    tokens = load_tokens_mmap(args.tokens)
    n_tokens = len(tokens)

    commit_positions = np.memmap(args.commit_positions, dtype=np.uint64, mode="r")
    print(f"Memory-mapped {len(commit_positions):,} COMMIT positions from {args.commit_positions}")

    direction_targets = np.memmap(args.direction_targets, dtype=np.int8, mode="r")
    print(f"Memory-mapped {len(direction_targets):,} direction targets from {args.direction_targets}")

    book_state = None
    if args.book_state:
        book_state = load_book_state_mmap(args.book_state)
        n_book = len(book_state)
        # Trim if needed (same logic as train_finetune.py)
        n_commits = len(commit_positions)
        if n_commits != n_book:
            n_min = min(n_commits, n_book)
            print(f"WARNING: commit/book_state mismatch {n_commits:,} vs {n_book:,}, using min")
            commit_positions = commit_positions[:n_min]
    else:
        print("Condition C: book_state not loaded (not needed)")

    # Trim direction targets to match commit_positions
    if len(direction_targets) > len(commit_positions):
        direction_targets = direction_targets[:len(commit_positions)]

    # ── Generate CPCV folds ────────────────────────────────────
    print(f"\n=== Generating {N_GROUPS}-group CPCV folds ===")
    folds = generate_folds(n_tokens, N_GROUPS, N_TEST_GROUPS, BUFFER_TOKENS)
    print(f"  {len(folds)} folds generated")

    os.makedirs(args.results_dir, exist_ok=True)

    # ── Run folds ──────────────────────────────────────────────
    condition = args.condition
    all_results = []
    use_pin = device.type == "cuda"
    persist = args.num_workers > 0

    print(f"\n{'fold':>4} {'train_win':>10} {'test_win':>9} "
          f"{'acc':>7} {'maj':>7} {'delta':>7} {'time':>6}")
    print("-" * 58)

    for fold_idx, (train_ranges, test_ranges) in enumerate(folds):
        train_ds, n_train = make_datasets(
            tokens, commit_positions, book_state, direction_targets,
            train_ranges, args.context_len, args.stride, args.max_commits,
        )
        test_ds, n_test = make_datasets(
            tokens, commit_positions, book_state, direction_targets,
            test_ranges, args.context_len, args.stride, args.max_commits,
        )

        if train_ds is None or test_ds is None or n_train == 0 or n_test == 0:
            print(f"  Fold {fold_idx}: skipped (empty dataset)")
            continue

        train_dl = DataLoader(
            train_ds, batch_size=args.batch_size, shuffle=False,
            num_workers=args.num_workers, pin_memory=use_pin,
            persistent_workers=persist, drop_last=True,
        )
        test_dl = DataLoader(
            test_ds, batch_size=args.batch_size, shuffle=False,
            num_workers=args.num_workers, pin_memory=use_pin,
            persistent_workers=persist,
        )

        t0 = time.time()
        print(f"\n  Fold {fold_idx}: {n_train:,} train / {n_test:,} test windows", flush=True)

        if condition == "B":
            model, pre_head, post_head, dir_head = create_model_B(
                args.checkpoint, device, args.d_model
            )
            lr = args.lr_b
        else:
            model, pre_head, post_head, dir_head = create_model_C(device, args.d_model)
            lr = args.lr_c

        print(f"  Model loaded, starting training (lr={lr})...", flush=True)
        train_fold(
            model, dir_head, pre_head, post_head, train_dl, device,
            condition, args.epochs, lr, amp_dtype,
        )

        metrics = evaluate_direction(model, dir_head, test_dl, device, amp_dtype)
        dt = time.time() - t0

        print(f"  {fold_idx:>2}   {n_train:>10,} {n_test:>9,} "
              f"{metrics['accuracy']:>7.2%} {metrics['majority_class_acc']:>7.2%} "
              f"{metrics['beats_majority_pp']:>+6.1f}pp {dt:>5.0f}s")

        fold_result = {
            "fold": fold_idx,
            "condition": condition,
            "n_train_windows": n_train,
            "n_test_windows": n_test,
            "metrics": metrics,
        }
        all_results.append(fold_result)

        # Free GPU memory
        del model, pre_head, post_head, dir_head

        # Persist after each fold so partial results survive crashes
        partial_path = os.path.join(args.results_dir, f"phase3_{condition}_partial.json")
        with open(partial_path, "w") as f:
            json.dump({"condition": condition, "completed_folds": len(all_results),
                        "results": all_results}, f, indent=2)

    # ── Summary ────────────────────────────────────────────────
    print(f"\n{'=' * 58}")
    print(f"CONDITION {condition} — {len(all_results)} FOLDS COMPLETE")
    print(f"{'=' * 58}")

    metrics_list = [r["metrics"] for r in all_results]
    mean_acc = np.mean([m["accuracy"] for m in metrics_list])
    mean_delta = np.mean([m["beats_majority_pp"] for m in metrics_list])
    n_beating = sum(1 for m in metrics_list if m["beats_majority_pp"] >= 2.0)

    print(f"  Mean accuracy:     {mean_acc:.2%}")
    print(f"  Mean delta vs F:   {mean_delta:+.1f}pp")
    print(f"  Folds >= +2pp:     {n_beating}/{len(all_results)}")

    if condition == "B":
        gate_threshold = int(np.ceil(len(all_results) * 0.60))
        gate_passed = n_beating >= gate_threshold
        print(f"\n  Gate check: {n_beating}/{len(all_results)} >= {gate_threshold} → "
              f"{'PASS' if gate_passed else 'FAIL'}")
        print(f"  (Final verdict requires merging B + C results)")

    # ── Save results ───────────────────────────────────────────
    results = {
        "condition": condition,
        "n_folds": len(all_results),
        "mean_accuracy": mean_acc,
        "mean_delta_pp": mean_delta,
        "folds_beating_majority": n_beating,
        "per_fold": all_results,
        "args": vars(args),
    }
    results_path = os.path.join(args.results_dir, f"phase3_{condition}_results.json")
    with open(results_path, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nResults saved to {results_path}")


if __name__ == "__main__":
    main()
