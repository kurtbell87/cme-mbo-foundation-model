# Project Instructions — Orchestration-Kit (Greenfield)

## MANDATORY: Work in a Worktree

**All work MUST begin on a new git worktree.** Never commit directly to this branch or to main.

```bash
git worktree add .worktrees/<descriptive-name> -b <branch-name> HEAD
cd .worktrees/<descriptive-name>
```

When done, push the branch and open a PR to merge back. This keeps the main working tree clean and avoids dirty-state conflicts between sessions.

## Path Convention

Kit state files, working directories, and utility scripts live in `.kit/`. Project source code (`src/`, `tests/`, etc.) stays at the project root. The kit prompts reference bare filenames (e.g., `LAST_TOUCH.md`) — the `KIT_STATE_DIR` environment variable tells the scripts to resolve these inside `.kit/`.

Files at project root: `CLAUDE.md`, `.claude/`, `.orchestration-kit.env`, `.gitignore`
Everything else kit-related: `.kit/`

## Available Kits

| Kit | Script | Phases |
|-----|--------|--------|
| **TDD** | `.kit/tdd.sh` | red, green, refactor, ship, full, watch |
| **Research** | `.kit/experiment.sh` | survey, frame, run, read, log, cycle, full, program, status |
| **Math** | `.kit/math.sh` | survey, specify, construct, formalize, prove, polish, audit, log, full, program, status |

## Orchestrator (Advanced)

For cross-kit runs and interop, use the orchestrator:

```bash
source .orchestration-kit.env
orchestration-kit/tools/kit --json <kit> <phase> [args...]
orchestration-kit/tools/kit --json research status
```

Run artifacts land in `orchestration-kit/runs/<run_id>/` — capsules, manifests, logs, events.

## Cross-Kit Interop (Advanced)

```bash
orchestration-kit/tools/kit request --from research --from-phase status --to math --action math.status \
  --run-id <parent_run_id> --json
orchestration-kit/tools/pump --once --request <request_id> --json
```

`--from-phase` is optional; if omitted, `orchestration-kit/tools/pump` infers it from the parent run metadata/events.

## Global Dashboard (Optional)

```bash
orchestration-kit/tools/dashboard register --orchestration-kit-root ./orchestration-kit --project-root "$(pwd)"
orchestration-kit/tools/dashboard index
orchestration-kit/tools/dashboard serve --host 127.0.0.1 --port 7340
```

Open `http://127.0.0.1:7340` to explore runs across projects and filter by project.

## State Files (in `.kit/`)

| Kit | Read first |
|-----|-----------|
| TDD | `CLAUDE.md` → `.kit/LAST_TOUCH.md` → `.kit/PRD.md` |
| Research | `CLAUDE.md` → `.kit/RESEARCH_LOG.md` → `.kit/QUESTIONS.md` |
| Math | `CLAUDE.md` → `.kit/CONSTRUCTION_LOG.md` → `.kit/CONSTRUCTIONS.md` |

## Working Directories

- `.kit/docs/` — TDD specs
- `.kit/experiments/` — Research experiment specs
- `.kit/results/` — Research + Math results
- `.kit/specs/` — Math specification documents
- `.kit/handoffs/completed/` — Resolved research handoffs
- `.kit/scripts/` — Utility scripts (symlinked from orchestration-kit)

## Git Worktree Setup

When working in a git worktree, `orchestration-kit/` will be empty. Use `tools/worktree-init`:

```bash
git worktree add ../project-slug -b feat/my-feature main
cd ../project-slug
orchestration-kit/tools/worktree-init
source .orchestration-kit.env
```

## Process Visibility (MCP)

- **`kit.active`** — List all background processes launched by the MCP server (run_id, pid, status, exit_code).
- **`kit.kill`** — Terminate a background process by run_id (SIGTERM/SIGKILL).
- **`kit.runs`** — Now shows runs immediately at launch (not just after completion). Includes `is_orphaned` flag for dead processes.

## Don't

- Don't `cd` into `orchestration-kit/` and run kit scripts from there — run from project root.
- Don't `cat` full log files — use `orchestration-kit/tools/query-log`.
- Don't explore the codebase to "understand" it — read state files first.
- **Don't independently verify kit sub-agent work.** Each phase spawns a dedicated sub-agent that does its own verification. Trust the exit code and capsule. Do NOT re-run tests, re-read logs, re-check build output, or otherwise duplicate work the sub-agent already did. Exit 0 + capsule = done. Exit 1 = read the capsule for the failure, don't grep the log.
- Don't read phase log files after a successful phase. Logs are for debugging failures only.

## Orchestrator Discipline (MANDATORY)

You are the orchestrator. Sub-agents do the work. Your job is to sequence phases and react to exit codes. Protect your context window.

1. **Run phases in background, check only the exit code.** Do not read the TaskOutput content — the JSON blob wastes context. Check `status: completed/failed` and `exit_code` only.
2. **Never run Bash for verification.** No `pytest`, `lake build`, `ls`, `cat`, `grep` to check what a sub-agent produced. If the phase exited 0, it worked.
3. **Never read implementation files** the sub-agents wrote (source code, test files, .lean files, experiment scripts). That is their domain. You read only state files (CLAUDE.md, `.kit/LAST_TOUCH.md`, `.kit/RESEARCH_LOG.md`, etc.).
4. **Chain phases by exit code only.** Exit 0 → next phase. Exit 1 → read the capsule (not the log), decide whether to retry or stop.
5. **Never read capsules after success.** Capsules exist for failure diagnosis and interop handoffs. A successful phase needs no capsule read.
6. **Minimize tool calls.** Each Bash call, Read, or Glob adds to your context. If the information isn't needed to decide the next action, don't fetch it.

## Breadcrumb Maintenance (MANDATORY)

After every session that changes the codebase, update:

1. **`.kit/LAST_TOUCH.md`** — Current state and what to do next (TDD).
2. **`.kit/RESEARCH_LOG.md`** — Append experiment results (Research).
3. **`.kit/CONSTRUCTION_LOG.md`** — Progress notes (Math).
4. **This file's "Current State" section** — Keep it current.

## Current State (updated 2026-03-06)

- **Build:** GREEN — compiles clean, 0 warnings
- **Branch:** `main` (all features merged)
- **Flow features:** Event-count EMA decay (halflives: 50/500/5000 events). 48 flow features (27 raw + 18 derived + 3 cross-scale). Inter-event time and event rate are explicit features.
- **Rithmic:** Live open-market test at 8:30am CT 2026-03-04 (see run command below)
- **Research/03:** Distributed imbalance CPCV implemented (`--fold-range`, `--mode aggregate`). First run on 4× c7a.8xlarge OOMed (94M rows/geometry exceeds 64 GB). Awaiting `--holdout-pct` (80/20 day-level split) + spot vCPU quota increase (128 → 512) to relaunch on 8× c7a.16xlarge.

### Live Test Run Command

```bash
# Run from the worktree root (wherever you cd'd after git worktree add)
LOG=~/logs/rithmic-health-$(date +%Y%m%d-%H%M).jsonl && mkdir -p ~/logs

RITHMIC_URI=wss://rituz00100.rithmic.com:443 \
RITHMIC_USER=kurtbell87Paper@amp.com \
RITHMIC_PASSWORD=Sim145615 \
RITHMIC_CERT_PATH=/Users/brandonbell/Downloads/0.89.0.0/etc/rithmic_ssl_cert_auth_params \
RITHMIC_SYSTEM="Rithmic Paper Trading" \
~/.cargo/bin/cargo run --release -p rithmic-live -- \
  --symbol MESH6 --exchange CME --tick-size 0.25 --dev-mode \
  --log-file "$LOG"
```

Run for at least 30 minutes through the open. Ctrl+C to stop. Log lands in `~/logs/`.

### Pass/Fail Analysis (run after stopping)

```bash
python3 -c "
import json, sys
events = [json.loads(l) for l in open('$LOG')]
sd = next((e for e in events if e['event'] == 'shutdown'), {})
recoveries = sum(1 for e in events if e['event'] == 'recovery_triggered')
skips = sum(1 for e in events if e['event'] == 'validation_skipped')
validations = sd.get('validations', 0)
gaps = sd.get('gaps', 0)
degraded = sd.get('degraded', False)
skip_pct = (skips / validations * 100) if validations > 0 else 0
print(f'exit_reason : {sd.get(\"exit_reason\", \"unknown\")}')
print(f'degraded    : {degraded}')
print(f'recoveries  : {recoveries}  (pass = 0)')
print(f'skipped     : {skips}  ({skip_pct:.1f}%)  (pass = <30%)')
print(f'gaps        : {gaps}  (pass = 0)')
print()
print('PASS' if (recoveries == 0 and skip_pct < 30 and gaps == 0 and not degraded) else 'FAIL')
"
```

### BBO Validation Design (Phase 3 — DONE)

Two guards before triggering snapshot recovery:
1. **Max-age:** skip if adjusted BBO age (raw − 150ms clock offset) > 400ms — stale BBO means book is MORE current
2. **Directional-consistency:** trigger only after 5 consecutive same-direction fresh divergences

On FAIL: `grep '"divergence"' $LOG | python3 -c "import json,sys; [print(json.loads(l)['ts_ms'], json.loads(l)['direction'], 'streak='+str(json.loads(l)['consecutive_consistent']), 'adj_age='+str(json.loads(l)['adjusted_age_ms'])+'ms') for l in sys.stdin]"`

### C++ Pipeline: RETIRED

The C++ MBO-DL pipeline is **deprecated and will not be revisited.** Rust pipeline is sole ground truth.
`tools/parity-test/` and `FEATURE_PARITY_SPEC.md` are historical artifacts — do not invest further effort.

### Feature Design Direction

Mid-price features are **out**. All features grounded in tradeable prices:
- Last **trade price** for close/open/returns
- **Bid/ask** for position and range metrics
- **VWAP**, spread, and order book imbalance remain valid

### Phase Status

| Phase | Status |
|-------|--------|
| 0–0c (Parity infrastructure) | DONE — retired with C++ |
| 1 (XGBoost-ffi) | DONE |
| 2 (Rithmic protobuf + msg-161) | DONE |
| 3 (Rithmic WebSocket Client) | **DONE** — live test pending 2026-03-04 open |
| 4 (Streaming Live Pipeline) | **NEXT** after live test passes |
| 5 (Trading Engine) | Blocked on 4 |
