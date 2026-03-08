# MBO-DL — CME Microstructure Research & Trading Pipeline

## What This Is

Rust workspace for CME futures microstructure research and live trading. Ingests L3 (MBO) order book data from Databento (historical) and Rithmic (live), reconstructs full limit order books, extracts features, and evaluates trading strategies via rigorous CPCV backtesting.

**Current focus:** Finding actionable alpha in event-level MBO data. Three research threads have conclusively shown that static LOB snapshot features do not predict short-term price direction. See `.kit/RESEARCH_LOG.md` for full findings and the path forward.

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
| `crates/xgboost-ffi` | Pure Rust XGBoost JSON inference |

### Tools

| Tool | Purpose |
|------|---------|
| `tools/rithmic-live` | Live multi-instrument pipeline (Rithmic → BookBuilder → features) |
| `tools/event-export` | Export LOB features + labels to Parquet from .dbn.zst |
| `tools/event-backtest` | CPCV + serial PnL backtest with distributed fold sharding |
| `tools/book-verify` | Validate BookBuilder against Databento MBP-10 ground truth |

### Research

| Directory | Status |
|-----------|--------|
| `research/01-bar-level-cpcv` | DEAD — bar aggregation destroys signal |
| `research/02-tick-level-serial` | DEAD — confirms null hypothesis at execution resolution |
| `research/03-event-lob-probability` | DEAD — 0/45 CPCV folds positive with static LOB features |
| `research/RESEARCH_INDEX.md` | Summary + pointers to evidence |

### Data

- **S3:** `s3://kenoma-labs-research/data/MES-MBO-2022/` — 312 .dbn.zst files, 49.2 GB (full year 2022 MES MBO)
- **Local:** No derived data. Re-export from .dbn.zst as needed.

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

## Current State (2026-03-06)

- **Build:** GREEN — compiles clean, 0 warnings
- **Branch:** `main`
- **Research:** Three threads completed, all negative. Static LOB features have zero predictive power for short-term price direction on MES. See `.kit/RESEARCH_LOG.md` for details and path forward.
- **Infrastructure:** Production-grade. BookBuilder verified, multi-instrument live pipeline tested, CPCV distributed backtest framework ready. Event export can re-derive any feature set from raw .dbn.zst.
- **Next:** Local experiments testing event sequences, cross-instrument signal, regime conditioning, and alternative target formulations. Zero EC2 cost until local results show promise.
