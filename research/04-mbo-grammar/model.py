"""
GPT-style decoder-only transformer for MBO token sequences.

Small architecture: 4 layers, 128 dim, 4 heads, ~870K parameters.
Trained with autoregressive next-token prediction on the 126-token
MBO vocabulary. Weight-tied embedding/output projection.

Uses F.scaled_dot_product_attention(is_causal=True) directly to ensure
FlashAttention dispatch on PyTorch 2.5.1 (nn.MultiheadAttention blocks
FlashAttention during training when dropout > 0).
"""

import math
import torch
import torch.nn as nn
import torch.nn.functional as F


# Vocabulary constants (must match crates/mbo-tokenizer/src/lib.rs)
# Padded from 126 to 128 for tensor core alignment (8-tile boundary)
VOCAB_SIZE = 128
PAD = 0
BOS = 1
EOS = 2
COMMIT = 3

# Token class ranges for loss masking
SPECIAL_TOKENS = frozenset([PAD, BOS, EOS, COMMIT])


class CausalBlock(nn.Module):
    """Pre-LN transformer block with direct SDPA call for FlashAttention."""

    def __init__(self, d_model: int, nhead: int, dim_ff: int, dropout: float):
        super().__init__()
        assert d_model % nhead == 0
        self.nhead = nhead
        self.head_dim = d_model // nhead

        self.ln1 = nn.LayerNorm(d_model)
        self.ln2 = nn.LayerNorm(d_model)

        # Merged QKV projection (same layout as nn.MultiheadAttention.in_proj)
        self.in_proj = nn.Linear(d_model, 3 * d_model)
        self.out_proj = nn.Linear(d_model, d_model)

        self.ff = nn.Sequential(
            nn.Linear(d_model, dim_ff),
            nn.ReLU(),
            nn.Linear(dim_ff, d_model),
        )

        self.attn_dropout = dropout
        self.resid_drop = nn.Dropout(dropout)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        B, T, D = x.shape

        # Self-attention with pre-LN
        h = self.ln1(x)
        qkv = self.in_proj(h).reshape(B, T, 3, self.nhead, self.head_dim)
        q, k, v = qkv.unbind(2)            # each (B, T, H, D_h)
        q = q.transpose(1, 2)              # (B, H, T, D_h)
        k = k.transpose(1, 2)
        v = v.transpose(1, 2)

        # Direct SDPA call → FlashAttention dispatch (no mask tensor)
        drop_p = self.attn_dropout if self.training else 0.0
        attn_out = F.scaled_dot_product_attention(q, k, v, is_causal=True, dropout_p=drop_p)
        attn_out = attn_out.transpose(1, 2).reshape(B, T, D)
        attn_out = self.out_proj(attn_out)
        x = x + self.resid_drop(attn_out)

        # FFN with pre-LN
        h = self.ln2(x)
        x = x + self.resid_drop(self.ff(h))
        return x


class MBOTransformer(nn.Module):
    def __init__(
        self,
        vocab_size: int = VOCAB_SIZE,
        d_model: int = 128,
        nhead: int = 4,
        num_layers: int = 4,
        dim_ff: int = 512,
        max_seq_len: int = 512,
        dropout: float = 0.1,
    ):
        super().__init__()
        self.d_model = d_model
        self.max_seq_len = max_seq_len

        self.tok_emb = nn.Embedding(vocab_size, d_model)
        self.pos_emb = nn.Embedding(max_seq_len, d_model)
        self.drop = nn.Dropout(dropout)

        self.layers = nn.ModuleList([
            CausalBlock(d_model, nhead, dim_ff, dropout)
            for _ in range(num_layers)
        ])
        self.ln_f = nn.LayerNorm(d_model)
        self.head = nn.Linear(d_model, vocab_size, bias=False)

        # Weight tying: output projection shares weights with token embedding
        self.head.weight = self.tok_emb.weight

        self._init_weights()

    def _init_weights(self):
        nn.init.normal_(self.tok_emb.weight, std=0.02)
        nn.init.normal_(self.pos_emb.weight, std=0.02)
        for p in self.layers.parameters():
            if p.dim() > 1:
                nn.init.xavier_uniform_(p)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        """
        Args:
            x: (batch, seq_len) token IDs

        Returns:
            logits: (batch, seq_len, vocab_size)
        """
        B, T = x.shape
        assert T <= self.max_seq_len, f"Sequence length {T} > max {self.max_seq_len}"

        pos = torch.arange(T, device=x.device).unsqueeze(0)
        h = self.tok_emb(x) + self.pos_emb(pos)
        h = self.drop(h)

        for layer in self.layers:
            h = layer(h)
        h = self.ln_f(h)
        logits = self.head(h)
        return logits

    def forward_hidden(self, x: torch.Tensor) -> torch.Tensor:
        """Forward pass returning only hidden states (skips output projection).
        Uses dropout (respects train/eval mode).

        Returns:
            h: (batch, seq_len, d_model)
        """
        B, T = x.shape
        assert T <= self.max_seq_len

        pos = torch.arange(T, device=x.device).unsqueeze(0)
        h = self.tok_emb(x) + self.pos_emb(pos)
        h = self.drop(h)

        for layer in self.layers:
            h = layer(h)
        h = self.ln_f(h)
        return h

    def forward_with_hidden(self, x: torch.Tensor):
        """
        Forward pass returning both logits and pre-head hidden states.
        Uses dropout (respects train/eval mode).

        Returns:
            logits: (batch, seq_len, vocab_size)
            h:      (batch, seq_len, d_model)
        """
        h = self.forward_hidden(x)
        logits = self.head(h)
        return logits, h

    def extract_hidden(self, x: torch.Tensor) -> torch.Tensor:
        """
        Forward pass returning hidden states (before output projection).

        Args:
            x: (batch, seq_len) token IDs

        Returns:
            h: (batch, seq_len, d_model) hidden states
        """
        B, T = x.shape
        pos = torch.arange(T, device=x.device).unsqueeze(0)
        h = self.tok_emb(x) + self.pos_emb(pos)
        # No dropout during extraction
        for layer in self.layers:
            h = layer(h)
        h = self.ln_f(h)
        return h

    def count_parameters(self) -> int:
        return sum(p.numel() for p in self.parameters() if p.requires_grad)


class DirectionalHead(nn.Module):
    """Predicts direction of next BBO change from hidden states at COMMIT positions."""

    def __init__(self, d_model: int):
        super().__init__()
        self.head = nn.Sequential(
            nn.Linear(d_model, d_model // 2),
            nn.GELU(),
            nn.Linear(d_model // 2, 1),
        )

    def forward(self, h: torch.Tensor) -> torch.Tensor:
        """h: (N, d_model) → logits: (N,)"""
        return self.head(h).squeeze(-1)

    def count_parameters(self) -> int:
        return sum(p.numel() for p in self.parameters() if p.requires_grad)


class ReconHead(nn.Module):
    """
    Book state reconstruction head.

    Predicts book state from hidden states at COMMIT positions:
      - 10 size fields (5 bid + 5 ask levels) as 9-class classification (log2 buckets)
      - Spread as 11-class classification (0=crossed, 1-9=ticks, 10=wide)
      - Level-1 imbalance as regression in [0, 1]

    At d_model=256, ~100K parameters per head.
    """

    N_SIZE_FIELDS = 10
    N_SIZE_CLASSES = 9
    N_SPREAD_CLASSES = 11

    def __init__(self, d_model: int):
        super().__init__()
        mid = d_model // 2
        self.size_head = nn.Sequential(
            nn.Linear(d_model, mid),
            nn.GELU(),
            nn.Linear(mid, self.N_SIZE_FIELDS * self.N_SIZE_CLASSES),
        )
        self.spread_head = nn.Sequential(
            nn.Linear(d_model, mid),
            nn.GELU(),
            nn.Linear(mid, self.N_SPREAD_CLASSES),
        )
        self.imbalance_head = nn.Sequential(
            nn.Linear(d_model, mid),
            nn.GELU(),
            nn.Linear(mid, 1),
            nn.Sigmoid(),
        )

    def forward(self, h: torch.Tensor):
        """
        Args:
            h: (N, d_model) hidden states at COMMIT positions

        Returns:
            size_logits:    (N, 10, 9)  per-field size class logits
            spread_logits:  (N, 11)     spread class logits
            imbalance:      (N,)        predicted imbalance in [0, 1]
        """
        size_logits = self.size_head(h).view(
            -1, self.N_SIZE_FIELDS, self.N_SIZE_CLASSES
        )
        spread_logits = self.spread_head(h)
        imbalance = self.imbalance_head(h).squeeze(-1)
        return size_logits, spread_logits, imbalance

    def count_parameters(self) -> int:
        return sum(p.numel() for p in self.parameters() if p.requires_grad)
