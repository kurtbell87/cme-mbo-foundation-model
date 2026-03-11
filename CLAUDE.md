# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# MBO-DL — CME Microstructure Research & Trading Pipeline

## What This Is

Rust workspace for CME futures microstructure research and live trading. Ingests L3 (MBO) order book data from Databento (historical) and Rithmic (live), reconstructs full limit order books, extracts features, and evaluates trading strategies via rigorous CPCV backtesting.

**Current focus:** MBO grammar foundation model. Six research threads (01-04, 06) conclusively showed that hand-engineered LOB features + XGBoost cannot predict short-term price direction. Thread 05 pivoted to transformer pretraining on tokenized MBO event sequences (126-token vocabulary). Phase 1 (language model) and Phase 2 (book state reconstruction) gates passed. Phase 3 (directional signal check) is next. See `.kit/RESEARCH_LOG.md` for full findings.

## Read Order

1. **This file** — project structure, build, conventions
2. **`.kit/RESEARCH_LOG.md`** — what we learned, what doesn't work, path forward
3. **`.kit/QUESTIONS.md`** — prioritized open research questions

## Build

```bash
~/.cargo/bin/cargo build --release          # full workspace
~/.cargo/bin/cargo test -p book-builder     # run tests for a crate
~/.cargo/bin/cargo run --release -p rithmic-live -- --help
```

`cargo` is NOT on PATH — always use `~/.cargo/bin/cargo`.
Use `-p <package>` for workspace builds, not `--bin <name>`.

## Workspace Layout

### Core Crates

| Crate | Purpose |
|-------|---------|
| `crates/book-builder` | L2 order book from MBO events. Sorted-Vec + FxHashMap. Verified 99.91% vs Databento MBP-10. |
| `crates/flow-features` | 48 event-count EMA flow features (OFI, trade flow, cancel rates, etc.) |
| `crates/event-features` | 42 instantaneous LOB features from CommittedState |
| `crates/event-labels` | Tick-level triple-barrier simulation (multi-geometry) |
| `crates/common` | Shared types |
| `crates/backtest` | Triple-barrier backtest engine |
| `crates/rithmic-client` | Rithmic protobuf WebSocket client (live market data) |
| `crates/databento-ingest` | Databento .dbn.zst ingestion |
| `crates/seq-features` | 22 inter-episode BBO dynamics features (order flow pressure, BBO transition patterns) |
| `crates/xgboost-ffi` | Pure Rust XGBoost JSON inference |

### Tools

| Tool | Purpose |
|------|---------|
| `tools/rithmic-live` | Live multi-instrument pipeline (Rithmic → BookBuilder → features) |
| `tools/event-export` | Export LOB features + labels to Parquet from .dbn.zst |
| `tools/event-backtest` | CPCV + serial PnL backtest with distributed fold sharding |
| `tools/book-verify` | Validate BookBuilder against Databento MBP-10 ground truth |
| `tools/seq-diag` | Dump seq-features distributions from .dbn.zst to CSV for validation |
| `tools/cloud-run` | Multi-backend (EC2/RunPod) cloud compute orchestration with TTL enforcement |
| `tools/lead-lag` | Cross-instrument lead/lag analysis |
| `tools/session-strat` | Session-based strategy analysis |

### Research

| Directory | Status |
|-----------|--------|
| `research/01-bar-level-cpcv` | DEAD — bar aggregation destroys signal |
| `research/02-tick-level-serial` | DEAD — confirms null hypothesis at execution resolution |
| `research/03-event-lob-probability` | DEAD — 0/45 CPCV folds positive with static LOB features |
| `research/04-mbo-grammar` | ACTIVE — transformer pretraining on tokenized MBO events. Phase 2 passed, Phase 3 next. |
| `research/RESEARCH_INDEX.md` | Summary + pointers to evidence |

Threads 04 (seq-features CPCV) and 06 (cooldown sweep) had no separate research directories — results on S3 only. See `.kit/RESEARCH_LOG.md`.

### Data

- **Local raw:** `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/` — 312 .dbn.zst files, 49.2 GB (full year 2022 MES MBO)
- **S3 raw:** `s3://kenoma-labs-research/data/MES-MBO-2022/` — same files, for cloud convenience
- **S3 tokenized:** `s3://kenoma-labs-research/cloud-runs/mbo-grammar/` — tokens.bin, .mids, .book_state, .meta.json

## Git Workflow

Work directly on `main` or on a feature branch. **Do NOT create git worktrees** unless explicitly asked.

## Cloud Infrastructure — MANDATORY RULES

**NEVER launch raw EC2 instances, RunPod pods, write ad-hoc user-data scripts, or create one-off launch scripts. ALL cloud compute MUST go through the `cloud-run` CLI (`tools/cloud-run`). NO EXCEPTIONS.**

The `cloud-run` CLI provides heartbeat monitoring, idle detection, TTL enforcement, and automatic termination. Bypassing it creates zombie instances/pods that burn money indefinitely.

**Rules:**
1. **Every workload uses `cloud-run launch`.** No `aws ec2 run-instances` or `runpodctl create pod` in scripts.
2. **Every workload needs a `cloud-run.toml`.** With `max_runtime_minutes`, `gpu` (true/false), and `spot` (default true).
3. **Docker images that can't cross-compile** (e.g., xgboost-sys) must be built inside a container managed by cloud-run — NOT on a raw EC2 instance.
4. **Never modify the default VPC security group.** If SSH access is needed, cloud-run creates a dedicated SG with `key_name`.
5. **Run `cloud-run cleanup` after failures.** Kill TTL-expired instances, orphaned SGs, and exited RunPod pods.
6. **Run `cloud-run list` before launching.** Verify no forgotten instances/pods are still running.

**If you find yourself writing `aws ec2 run-instances`, `runpodctl create pod`, or a bash bootstrap script, STOP. You are doing it wrong. Use cloud-run.**

### Backend Selection

- **AWS EC2** (default): Omit `[runpod]` section. Uses ECR for images, spot-first with on-demand fallback.
- **RunPod**: Add `[runpod]` section with `gpu_type`. Uses Docker Hub for images, GraphQL API for pod lifecycle.

RunPod is preferred for heavy GPU training (H200, A100) — significantly cheaper than AWS GPU instances.

```toml
# Example: RunPod H200 training
[experiment]
name = "mbo-transformer"

[container]
dockerfile = "Dockerfile"
dockerhub_repo = "kurtbell87/mbo-dl"    # Required for RunPod builds

[instance]
max_runtime_minutes = 480
gpu = true

[runpod]
gpu_type = "NVIDIA H200 SXM"            # RunPod GPU type
container_disk_gb = 20
volume_gb = 0

[run]
command = "python train.py --batch-size 32"
env = { CUDA_VISIBLE_DEVICES = "0" }

[results]
s3_prefix = "s3://kenoma-labs-research/runs"
```

**RunPod prerequisites:**
- `RUNPOD_API_KEY` env var or `~/.runpod/config.toml`
- `docker login` for Docker Hub push
- AWS credentials available (for S3 results upload from pod)

## Key Conventions

- **No mid-price.** All features and labels use tradeable prices (bid/ask for entry, trade price for close).
- **Serial execution only.** Backtests enforce one position at a time. Overlapping metrics are meaningless.
- **Event-level, not time-aggregated.** 5s bars are information-destroying. All analysis at committed-state resolution.
- **Validate locally first.** No EC2 spend until local experiments on sample data show signal.

## Live Pipeline

Multi-instrument Rithmic client. Each instrument gets its own tokio task with independent BookBuilder, BBO ring buffer, and recovery state.

```bash
LOG=~/logs/rithmic-$(date +%Y%m%d-%H%M).jsonl && mkdir -p ~/logs

RITHMIC_URI=wss://rprotocol-mobile.rithmic.com:443 \
RITHMIC_USER=kurtbell87Paper@amp.com \
RITHMIC_PASSWORD=Sim145615 \
RITHMIC_CERT_PATH=/Users/brandonbell/Downloads/0.89.0.0/etc/rithmic_ssl_cert_auth_params \
RITHMIC_SYSTEM="Rithmic Paper Trading" \
~/.cargo/bin/cargo run --release -p rithmic-live -- \
  --instrument MESH6:CME:0.25 --instrument MNQH6:CME:0.25 --dev-mode \
  --log-file "$LOG"
```

### Rithmic Notes

- **Correct URI:** `wss://rprotocol-mobile.rithmic.com:443` (Paper Trading)
- **Wrong URI:** `wss://rituz00100.rithmic.com:443` — test gateway, no paper trading system
- AMP paper account: max 1 market data session (CME non-pro rule). Multiple symbols on one connection is fine.

## Orchestration Kit

Available kits for structured research and development:

| Kit | Script | Use For |
|-----|--------|---------|
| **Research** | `.kit/experiment.sh` | Hypothesis testing, experiment cycles |
| **TDD** | `.kit/tdd.sh` | Red/green/refactor development |
| **Math** | `.kit/math.sh` | Formal specifications |

```bash
source .orchestration-kit.env
# Run from project root, never cd into orchestration-kit/
```

### State Files

| File | Purpose |
|------|---------|
| `.kit/RESEARCH_LOG.md` | Master research findings — read first for any research task |
| `.kit/QUESTIONS.md` | Prioritized open questions with decision gates |
| `.kit/LAST_TOUCH.md` | TDD current state |

### Orchestrator Discipline

When using kit phases as orchestrator:

1. Run phases in background, check exit code only. Don't read TaskOutput content.
2. Don't re-verify sub-agent work. Exit 0 = done.
3. Chain phases by exit code. Exit 1 → read capsule, decide retry/stop.
4. Read state files, not implementation files or logs.

## Breadcrumb Maintenance

After every session that changes the codebase, update:

1. `.kit/RESEARCH_LOG.md` — append findings
2. `.kit/QUESTIONS.md` — update question status
3. This file's Current State section

## Current State (2026-03-11)

- **Build:** GREEN — compiles clean, 1 warning (unused `resolve_datacenter` in cloud-run)
- **Branch:** `main`
- **Research:** Six threads completed (01-04, 06 negative; 05 Gate 1 passed / Gate 2 negative). Phase 1 PASSED (ppl 1.864). **Phase 2 (Gate 1.5) PASSED** — dual-head book state reconstruction: size acc 67.4% (vs 23.6% baseline), spread NM 84.4%, imb MAE 0.0665. LOBS5 pre-batch risk did NOT materialize.
- **Literature:** Competitive landscape assessed (13 papers, 3 deep dives). CME futures MBO whitespace confirmed. TradeFM (JPMorgan, 524M params) is primary threat but equities-only. Three extractable components queued (continuous-time RoPE, interarrival time encoding, MarS-style additive conditioning). See `.kit/lit-review-lob-dl.md` and `.kit/lit-deep-dives.md`.
- **S3 canonical data:** `s3://kenoma-labs-research/cloud-runs/mbo-grammar/` — tokens.bin (7.3 GiB), .book_state (31.1 GiB, 695M rows), .mids (10.4 GiB), .meta.json.
- **S3 Phase 2 results:** `s3://kenoma-labs-research/runs/mbo-grammar-phase2-20260310T020658Z/results/` — best_model.pt, gate1_5_results.json.
- **cloud-run:** RunPod backend fully functional. Self-stop still broken (pods auto-restart).
- **Architecture:** Custom CausalBlock with `F.scaled_dot_product_attention(is_causal=True)` for FlashAttention. VOCAB_SIZE 128 (padded from 126). Checkpoint key remapping for CausalBlock.
- **Next:** Phase 3 — directional signal check. B (pretrained+recon) vs C (random) on "next BBO change direction", 15-fold CPCV. Kill gate: B >= majority+2pp in >=60% of folds. Code: `phase3_signal_check.py`, `cloud-run-phase3.toml` in `../mbo-tokenization/research/04-mbo-grammar/`.
