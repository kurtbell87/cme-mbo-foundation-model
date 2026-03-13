"""
Dataset utilities for MBO token sequences.

Loads binary u16 token files produced by mbo-tokenize and creates
sliding-window datasets for autoregressive training.
"""

import struct

import numpy as np
import torch
from torch.utils.data import Dataset, IterableDataset

COMMIT_TOKEN = 3  # Must match vocabulary in model.py
PACKED_MAGIC = b"PKDW"
PACKED_HEADER_SIZE = 64


# ═══════════════════════════════════════════════════════════════
# Loading (eager — used by Gate 1 train.py)
# ═══════════════════════════════════════════════════════════════

def load_tokens(path: str) -> np.ndarray:
    """Load binary u16 token file into numpy array."""
    tokens = np.fromfile(path, dtype=np.uint16)
    print(f"Loaded {len(tokens):,} tokens from {path}")
    return tokens


def load_mids(path: str):
    """
    Load mids sidecar file.

    Returns:
        positions: np.ndarray of uint64 token positions
        mids: np.ndarray of int64 mid prices (fixed-point x1e9)
    """
    dt = np.dtype([("pos", "<u8"), ("mid", "<i8")])
    data = np.fromfile(path, dtype=dt)
    print(f"Loaded {len(data):,} mid-price entries from {path}")
    return data["pos"].copy(), data["mid"].copy()


# ═══════════════════════════════════════════════════════════════
# Loading (memory-mapped — used by Phase 2+ train_finetune.py)
# ═══════════════════════════════════════════════════════════════

def load_tokens_mmap(path: str) -> np.memmap:
    """Memory-mapped token loading. Returns read-only u16 memmap."""
    tokens = np.memmap(path, dtype=np.uint16, mode="r")
    print(f"Memory-mapped {len(tokens):,} tokens from {path}")
    return tokens


def load_commit_positions(mids_path: str) -> np.ndarray:
    """Load COMMIT global positions from mids sidecar into RAM (for binary search).

    Returns a sorted uint64 array of global token positions for every COMMIT.
    """
    dt = np.dtype([("pos", "<u8"), ("mid", "<i8")])
    mids = np.memmap(mids_path, dtype=dt, mode="r")
    positions = np.array(mids["pos"])  # copy to RAM
    assert len(positions) > 0, f"No entries in {mids_path}"
    assert np.all(positions[1:] > positions[:-1]), "Commit positions must be strictly increasing"
    print(f"Loaded {len(positions):,} COMMIT positions from {mids_path} "
          f"({len(positions) * 8 / 1e9:.1f} GB RAM)")
    return positions


def scan_commit_positions(tokens_path: str, chunk_size: int = 100_000_000) -> np.ndarray:
    """Scan tokens.bin for COMMIT token positions, eliminating .mids dependency.

    Processes in chunks to avoid materializing a 3.93B-element boolean array.

    Returns a sorted uint64 array of global token positions for every COMMIT.
    """
    tokens = np.memmap(tokens_path, dtype=np.uint16, mode="r")
    n = len(tokens)
    parts = []
    for offset in range(0, n, chunk_size):
        end = min(offset + chunk_size, n)
        chunk = np.array(tokens[offset:end])
        hits = np.where(chunk == COMMIT_TOKEN)[0].astype(np.uint64) + offset
        parts.append(hits)
    positions = np.concatenate(parts)
    assert len(positions) > 0, f"No COMMIT tokens found in {tokens_path}"
    assert np.all(positions[1:] > positions[:-1]), "COMMIT positions must be strictly increasing"
    print(f"Scanned {len(positions):,} COMMIT positions from {tokens_path} "
          f"({len(positions) * 8 / 1e9:.1f} GB RAM)")
    return positions


def load_book_state_mmap(path: str) -> np.memmap:
    """Memory-mapped book state sidecar. Returns (N, 12) float32 read-only memmap."""
    flat = np.memmap(path, dtype=np.float32, mode="r")
    n_rows = len(flat) // 12
    assert len(flat) == n_rows * 12, f"Book state size {len(flat)} not divisible by 12"
    data = flat.reshape(n_rows, 12)
    print(f"Memory-mapped {n_rows:,} book state entries from {path}")
    return data


# ═══════════════════════════════════════════════════════════════
# Datasets
# ═══════════════════════════════════════════════════════════════

class SlidingWindowDataset(Dataset):
    """
    Sliding window dataset for autoregressive training.

    Each sample is a window of `context_len + 1` tokens.
    Input: tokens[:-1], Target: tokens[1:].
    """

    def __init__(self, tokens: np.ndarray, context_len: int = 512, stride: int = 256):
        self.tokens = tokens
        self.context_len = context_len
        # Window start indices
        max_start = len(tokens) - context_len - 1
        if max_start < 0:
            self.starts = np.array([], dtype=np.int64)
        else:
            self.starts = np.arange(0, max_start + 1, stride, dtype=np.int64)
        print(f"SlidingWindowDataset: {len(self.starts):,} windows "
              f"(context={context_len}, stride={stride})")

    def __len__(self):
        return len(self.starts)

    def __getitem__(self, idx):
        start = self.starts[idx]
        chunk = self.tokens[start : start + self.context_len + 1]
        x = torch.from_numpy(chunk[:-1].astype(np.int64))
        y = torch.from_numpy(chunk[1:].astype(np.int64))
        return x, y


class ReconDataset(Dataset):
    """
    Sliding window dataset with book state reconstruction targets at COMMIT positions.

    Each sample yields:
        x:          (context_len,)      input token IDs
        y:          (context_len,)      LM target token IDs (shifted by 1)
        commit_pos: (max_commits,)      local positions of COMMITs in x (0-padded)
        post_state: (max_commits, 12)   raw book state at each COMMIT (0-padded)
        pre_state:  (max_commits, 12)   raw book state at previous COMMIT (0-padded)
        pre_valid:  (max_commits,)      bool mask for valid pre-batch targets
        n_commits:  scalar              actual number of COMMITs in this window

    All memmaps are shared across DataLoader workers (OS page cache).
    commit_positions (in RAM) is CoW-shared on fork.
    """

    def __init__(
        self,
        tokens_mmap: np.memmap,
        commit_positions: np.ndarray,
        book_state_mmap: np.memmap,
        start: int = 0,
        end: int | None = None,
        context_len: int = 512,
        stride: int = 2048,
        max_commits: int = 200,
    ):
        self.tokens = tokens_mmap
        self.commit_positions = commit_positions
        self.book_state = book_state_mmap
        self.context_len = context_len
        self.max_commits = max_commits

        if end is None:
            end = len(tokens_mmap)
        max_start = min(end, len(tokens_mmap)) - context_len - 1
        if max_start < start:
            self.starts = np.array([], dtype=np.int64)
        else:
            self.starts = np.arange(start, max_start + 1, stride, dtype=np.int64)

        print(f"ReconDataset: {len(self.starts):,} windows "
              f"(context={context_len}, stride={stride}, "
              f"range=[{start:,}, {end:,}))")

    def __len__(self):
        return len(self.starts)

    def __getitem__(self, idx):
        start = self.starts[idx]
        chunk = np.array(
            self.tokens[start : start + self.context_len + 1], dtype=np.int64
        )
        x = torch.from_numpy(chunk[:-1])
        y = torch.from_numpy(chunk[1:])

        # Find COMMIT positions in input x (local indices 0..context_len-1)
        local_commits = np.where(chunk[:-1] == COMMIT_TOKEN)[0]

        # Map local positions → sidecar rows via binary search
        n = 0
        sidecar_rows = np.empty(0, dtype=np.int64)
        if len(local_commits) > 0:
            global_pos = (start + local_commits).astype(np.int64)
            sidecar_rows = np.searchsorted(self.commit_positions, global_pos)
            # Validate: sidecar position must exactly match global position
            valid = sidecar_rows < len(self.commit_positions)
            valid[valid] &= self.commit_positions[sidecar_rows[valid]] == global_pos[valid]
            local_commits = local_commits[valid]
            sidecar_rows = sidecar_rows[valid]
            n = min(len(local_commits), self.max_commits)

        # Allocate padded outputs
        cp = np.zeros(self.max_commits, dtype=np.int64)
        post = np.zeros((self.max_commits, 12), dtype=np.float32)
        pre = np.zeros((self.max_commits, 12), dtype=np.float32)
        pre_valid = np.zeros(self.max_commits, dtype=np.bool_)

        if n > 0:
            rows = sidecar_rows[:n]
            cp[:n] = local_commits[:n]
            post[:n] = np.array(self.book_state[rows])

            # Pre-batch target = book state at previous COMMIT
            pre_rows = rows - 1
            has_pre = pre_rows >= 0
            pre_valid[:n] = has_pre
            safe_rows = np.where(has_pre, pre_rows, 0)
            pre[:n] = np.array(self.book_state[safe_rows])
            pre[:n][~has_pre] = 0.0

        return (
            x,
            y,
            torch.from_numpy(cp),
            torch.from_numpy(post),
            torch.from_numpy(pre),
            torch.from_numpy(pre_valid),
            torch.tensor(n, dtype=torch.long),
        )


class PackedReconDataset(Dataset):
    """
    Memory-mapped pre-packed windows. Zero per-sample CPU work.

    Reads binary files produced by prepack_windows.py. Each record is a
    fixed-size numpy structured array element, accessed by index via memmap.
    """

    def __init__(self, path: str):
        with open(path, "rb") as f:
            raw = f.read(PACKED_HEADER_SIZE)
        magic, version, n_windows, context_len, max_commits, record_size = (
            struct.unpack("<4sIQIII", raw[:28])
        )
        assert magic == PACKED_MAGIC, f"Bad magic: {magic}"
        assert version == 1, f"Bad version: {version}"

        dtype = np.dtype([
            ("x", np.int16, (context_len,)),
            ("y", np.int16, (context_len,)),
            ("commit_pos", np.int16, (max_commits,)),
            ("post_state", np.float32, (max_commits, 12)),
            ("pre_state", np.float32, (max_commits, 12)),
            ("pre_valid", np.uint8, (max_commits,)),
            ("n_commits", np.int32),
        ])
        assert dtype.itemsize == record_size, (
            f"dtype size {dtype.itemsize} != header record_size {record_size}"
        )

        self.data = np.memmap(
            path, dtype=dtype, mode="r", offset=PACKED_HEADER_SIZE
        )
        assert len(self.data) == n_windows, (
            f"Expected {n_windows} records, got {len(self.data)}"
        )
        self.context_len = context_len
        self.max_commits = max_commits

        print(f"PackedReconDataset: {n_windows:,} windows from {path} "
              f"(context={context_len}, max_commits={max_commits})")

    def __len__(self):
        return len(self.data)

    def __getitem__(self, idx):
        rec = self.data[idx]
        return (
            torch.from_numpy(np.array(rec["x"], dtype=np.int64)),
            torch.from_numpy(np.array(rec["y"], dtype=np.int64)),
            torch.from_numpy(np.array(rec["commit_pos"], dtype=np.int64)),
            torch.from_numpy(np.array(rec["post_state"]).copy()),
            torch.from_numpy(np.array(rec["pre_state"]).copy()),
            torch.from_numpy(np.array(rec["pre_valid"], dtype=np.bool_)),
            torch.tensor(int(rec["n_commits"]), dtype=torch.long),
        )


class StreamingPackedDataset(IterableDataset):
    """
    Chunked-sequential packed dataset. Scales to any file size.

    Instead of random 22 KB reads across the entire file (page fault per sample),
    reads large contiguous chunks (~80-160 MB) sequentially. Shuffles chunk order
    across the file and sample order within each chunk.

    Yields pre-formed batch tensors (bulk numpy→torch conversion per chunk,
    not per sample). Use with DataLoader(batch_size=None) to skip collation.

    Multi-worker safe: each worker handles a disjoint subset of chunks.
    """

    def __init__(self, path: str, batch_size: int = 256, chunk_size: int = 4096):
        with open(path, "rb") as f:
            raw = f.read(PACKED_HEADER_SIZE)
        magic, version, n_windows, context_len, max_commits, record_size = (
            struct.unpack("<4sIQIII", raw[:28])
        )
        assert magic == PACKED_MAGIC, f"Bad magic: {magic}"
        assert version == 1, f"Bad version: {version}"

        self.path = path
        self.n_windows = n_windows
        self.context_len = context_len
        self.max_commits = max_commits
        self.record_size = record_size
        self.batch_size = batch_size
        self.chunk_size = chunk_size

        self.dtype = np.dtype([
            ("x", np.int16, (context_len,)),
            ("y", np.int16, (context_len,)),
            ("commit_pos", np.int16, (max_commits,)),
            ("post_state", np.float32, (max_commits, 12)),
            ("pre_state", np.float32, (max_commits, 12)),
            ("pre_valid", np.uint8, (max_commits,)),
            ("n_commits", np.int32),
        ])

        print(f"StreamingPackedDataset: {n_windows:,} windows from {path} "
              f"(context={context_len}, max_commits={max_commits}, "
              f"batch_size={batch_size}, chunk_size={chunk_size})")

    def __len__(self):
        """Number of complete batches per epoch (not raw windows).

        Used by DataLoader.__len__ and scheduler step computation.
        """
        return self.n_windows // self.batch_size

    def _open_memmap(self):
        return np.memmap(
            self.path, dtype=self.dtype, mode="r", offset=PACKED_HEADER_SIZE
        )

    def __iter__(self):
        data = self._open_memmap()
        n = self.n_windows
        cs = self.chunk_size
        bs = self.batch_size
        n_chunks = (n + cs - 1) // cs

        # Multi-worker: deterministic disjoint partition, then shuffle order
        worker_info = torch.utils.data.get_worker_info()
        if worker_info is not None:
            # Stride on natural order → guaranteed disjoint across workers
            worker_chunks = np.arange(worker_info.id, n_chunks, worker_info.num_workers)
            np.random.shuffle(worker_chunks)  # randomize chunk processing order
        else:
            worker_chunks = np.random.permutation(n_chunks)

        for ci in worker_chunks:
            start = ci * cs
            end = min(start + cs, n)
            # One large sequential read (~89 MB per chunk)
            chunk_data = np.array(data[start:end])
            # Shuffle within chunk
            perm = np.random.permutation(len(chunk_data))
            chunk_data = chunk_data[perm]

            # Bulk convert entire chunk to tensors (7 ops, not len*7)
            x_all = torch.from_numpy(chunk_data["x"].astype(np.int64))
            y_all = torch.from_numpy(chunk_data["y"].astype(np.int64))
            cp_all = torch.from_numpy(chunk_data["commit_pos"].astype(np.int64))
            post_all = torch.from_numpy(chunk_data["post_state"].copy())
            pre_all = torch.from_numpy(chunk_data["pre_state"].copy())
            pv_all = torch.from_numpy(chunk_data["pre_valid"].astype(np.bool_))
            nc_all = torch.from_numpy(chunk_data["n_commits"].astype(np.int64))

            # Yield pre-formed batches (skip DataLoader collation)
            for b_start in range(0, len(chunk_data), bs):
                b_end = b_start + bs
                if b_end > len(chunk_data):
                    break  # drop incomplete final batch
                yield (
                    x_all[b_start:b_end],
                    y_all[b_start:b_end],
                    cp_all[b_start:b_end],
                    post_all[b_start:b_end],
                    pre_all[b_start:b_end],
                    pv_all[b_start:b_end],
                    nc_all[b_start:b_end],
                )


# ═══════════════════════════════════════════════════════════════
# Splitting
# ═══════════════════════════════════════════════════════════════

def temporal_split(tokens: np.ndarray, train_frac: float = 0.8):
    """Split tokens temporally (no shuffling)."""
    split = int(len(tokens) * train_frac)
    return tokens[:split], tokens[split:]
