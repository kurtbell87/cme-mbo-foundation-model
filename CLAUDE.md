# Project Instructions — Orchestration-Kit (Greenfield)

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

## Current State (updated YYYY-MM-DD)

- **Build:** _update after first TDD cycle_
- **Experiments completed:** _0_
- **Constructions completed:** _0_
- **Next task:** _Pick a kit and start your first cycle._
