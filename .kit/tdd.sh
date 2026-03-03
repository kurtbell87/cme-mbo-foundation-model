#!/usr/bin/env bash
# tdd.sh -- TDD Workflow Orchestrator for Claude Code and Codex CLI
#
# Usage:
#   ./tdd.sh red   <spec-file>     # Write tests from spec
#   ./tdd.sh green                 # Implement to pass tests
#   ./tdd.sh refactor              # Refactor while keeping tests green
#   ./tdd.sh breadcrumbs <spec-file> # Update docs (LAST_TOUCH.md, CLAUDE.md, AGENTS.md, READMEs)
#   ./tdd.sh ship  <spec-file>     # Commit, create PR, archive spec
#   ./tdd.sh full  <spec-file>     # Run all phases sequentially (ship includes breadcrumbs)
#   ./tdd.sh watch [phase] [--resolve]  # Live-tail or summarize a phase log
#
# Configure via environment variables or edit the defaults below.

set -euo pipefail

# Allow nested Claude Code sessions (sub-agents spawned by orchestrator).
unset CLAUDECODE 2>/dev/null || true

# ──────────────────────────────────────────────────────────────
# Configuration -- edit these to match your project
# ──────────────────────────────────────────────────────────────
TEST_DIRS="${TEST_DIRS:-tests}"                  # Space-separated test directories
SRC_DIR="${SRC_DIR:-src}"                        # Source/implementation directory
PROMPT_DIR="${PROMPT_DIR:-.claude/prompts}"      # Phase-specific prompt files
HOOK_DIR="${HOOK_DIR:-.claude/hooks}"            # Hook scripts

# Kit state directory — greenfield sets KIT_STATE_DIR=".kit", monorepo leaves unset.
_SD="${KIT_STATE_DIR:-.}"

# Agent backend
# Supported values: claude, codex
TDD_AGENT_BIN="${TDD_AGENT_BIN:-claude}"
TDD_AGENT_EXTRA_ARGS="${TDD_AGENT_EXTRA_ARGS:-}"
# Codex network-isolation signal (injected by some sandboxed shells). Defaults to 0.
CODEX_SANDBOX_NETWORK_DISABLED="${CODEX_SANDBOX_NETWORK_DISABLED:-0}"

# Optional codex-specific prompt pack. If PROMPT_DIR was not overridden and
# codex prompts exist, prefer them when TDD_AGENT_BIN=codex.
if [[ "$TDD_AGENT_BIN" == "codex" && "$PROMPT_DIR" == ".claude/prompts" && -d ".codex/prompts" ]]; then
  PROMPT_DIR=".codex/prompts"
fi

# Test file patterns (find-compatible)
TEST_FILE_PATTERNS=(
  -name "test_*.cpp"
  -o -name "test_*.py"
  -o -name "test_*.ts"
  -o -name "test_*.js"
  -o -name "*_test.cpp"
  -o -name "*_test.py"
  -o -name "*_test.ts"
  -o -name "*_test.js"
  -o -name "*.test.ts"
  -o -name "*.test.js"
  -o -name "*.spec.ts"
  -o -name "*.spec.js"
)

# Build & test commands -- override these for your project
# These are injected into the agent's context so it knows how to build/test.
BUILD_CMD="${BUILD_CMD:-echo 'Set BUILD_CMD for your project'}"
TEST_CMD="${TEST_CMD:-echo 'Set TEST_CMD for your project'}"

# Log directory -- per-project isolation under /tmp
# Uses repo basename for per-project isolation
# Override with TDD_LOG_DIR if you need a different path.
_project_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
_project_name="$(basename "$_project_root")"
TDD_LOG_DIR="${TDD_LOG_DIR:-/tmp/tdd-${_project_name}}"
export TDD_LOG_DIR
mkdir -p "$TDD_LOG_DIR"

# Post-cycle PR settings
# Set TDD_AUTO_MERGE=true to auto-merge the PR after creation
# Set TDD_DELETE_BRANCH=true to delete the feature branch after merge
TDD_AUTO_MERGE="${TDD_AUTO_MERGE:-false}"
TDD_DELETE_BRANCH="${TDD_DELETE_BRANCH:-false}"
TDD_BASE_BRANCH="${TDD_BASE_BRANCH:-main}"

# ──────────────────────────────────────────────────────────────
# Colors
# ──────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

# ──────────────────────────────────────────────────────────────
# Helpers
# ──────────────────────────────────────────────────────────────

find_test_files() {
  # Standard pattern search across the project
  find . -type f \( "${TEST_FILE_PATTERNS[@]}" \) \
    ! -path "*/build/*" \
    ! -path "*/_deps/*" \
    ! -path "*/.git/*" \
    ! -path "*/__pycache__/*" \
    ! -path "*/.venv/*" \
    ! -path "*/venv/*" \
    ! -path "*/node_modules/*" \
    ! -path "*/dist/*" \
    ! -path "*/target/*"
  # Rust workspace: also find .rs files in */tests/ directories
  find . -type f -name "*.rs" -path "*/tests/*" \
    ! -path "*/target/*" \
    ! -path "*/.git/*" 2>/dev/null || true
}

lock_tests() {
  echo -e "${YELLOW}Locking test files...${NC}"
  local count=0
  while IFS= read -r f; do
    chmod 444 "$f"
    echo -e "   ${YELLOW}locked:${NC} $f"
    ((count++))
  done < <(find_test_files)

  # Lock test directories to prevent new file creation
  for d in $TEST_DIRS; do
    if [[ -d "$d" ]]; then
      find "$d" -type d | while IFS= read -r dir; do
        chmod 555 "$dir"
      done
    fi
  done

  echo -e "   ${YELLOW}$count file(s) locked${NC}"
}

unlock_tests() {
  echo -e "${BLUE}Unlocking test files...${NC}"
  # Unlock directories first
  for d in $TEST_DIRS; do
    if [[ -d "$d" ]]; then
      find "$d" -type d -exec chmod 755 {} \; 2>/dev/null || true
    fi
  done

  local count=0
  while IFS= read -r f; do
    chmod 644 "$f"
    ((count++))
  done < <(find_test_files)
  echo -e "   ${BLUE}$count file(s) unlocked${NC}"
}

ensure_hooks_executable() {
  if [[ -f "$HOOK_DIR/pre-tool-use.sh" ]]; then
    chmod +x "$HOOK_DIR/pre-tool-use.sh"
  fi
}

ensure_dashboard_watchdog() {
  case "${ORCHESTRATION_KIT_DASHBOARD_AUTOSTART:-1}" in
    0|false|FALSE|no|NO|off|OFF) return 0 ;;
  esac
  local mk_root="${ORCHESTRATION_KIT_ROOT:-}"
  if [[ -z "$mk_root" ]]; then
    return 0
  fi
  local dashboard="$mk_root/tools/dashboard"
  if [[ ! -x "$dashboard" ]]; then
    return 0
  fi

  local project_root="${PROJECT_ROOT:-$_project_root}"
  local label
  label="$(basename "$project_root")"

  "$dashboard" register --orchestration-kit-root "$mk_root" --project-root "$project_root" --label "$label" >/dev/null 2>&1 || true
  "$dashboard" ensure-service --wait-seconds "${ORCHESTRATION_KIT_DASHBOARD_ENSURE_WAIT_SECONDS:-1}" >/dev/null 2>&1 || true
}

_run_codex_agent() {
  local phase="$1"
  local system_prompt="$2"
  local user_prompt="$3"
  local allowed_tools="$4"
  local log_file="$5"
  local phase_upper
  phase_upper="$(printf '%s' "$phase" | tr '[:lower:]' '[:upper:]')"

  if ! command -v codex >/dev/null 2>&1; then
    echo -e "${RED}Error: codex CLI not found in PATH.${NC}" >&2
    return 127
  fi

  case "$CODEX_SANDBOX_NETWORK_DISABLED" in
    1|true|TRUE|yes|YES|on|ON)
      cat > "$log_file" <<LOG
[codex-network-isolation]
CODEX_SANDBOX_NETWORK_DISABLED=${CODEX_SANDBOX_NETWORK_DISABLED}
Codex CLI calls need outbound network access and were skipped.
Run this phase from a non-isolated shell, or switch backend:
  TDD_AGENT_BIN=claude ./tdd.sh $phase
LOG
      echo -e "${RED}Error: Codex backend blocked by shell network isolation (CODEX_SANDBOX_NETWORK_DISABLED=${CODEX_SANDBOX_NETWORK_DISABLED}).${NC}" >&2
      echo -e "${YELLOW}Hint:${NC} Run outside this sandbox or switch backend: TDD_AGENT_BIN=claude ./tdd.sh $phase" >&2
      return 69
      ;;
  esac

  local help_out
  help_out="$(codex --help 2>&1 || true)"

  local prompt_file
  prompt_file="$(mktemp)"
  cat > "$prompt_file" <<PROMPT
You are running the ${phase_upper} phase of a strict TDD workflow.
Follow all instructions below exactly.

## System Instructions
$system_prompt

## Compatibility Notes
- Allowed tools in the original workflow: $allowed_tools
- Keep output concise and action-oriented.

## Task
$user_prompt
PROMPT

  local rc=0
  if echo "$help_out" | grep -Eq -- '(^|[[:space:]])exec([[:space:]]|$)'; then
    if [[ -n "$TDD_AGENT_EXTRA_ARGS" ]]; then
      # shellcheck disable=SC2086
      codex $TDD_AGENT_EXTRA_ARGS exec "$(cat "$prompt_file")" > "$log_file" 2>&1 || rc=$?
    else
      codex exec "$(cat "$prompt_file")" > "$log_file" 2>&1 || rc=$?
    fi
  elif echo "$help_out" | grep -Eq -- '(^|[[:space:]])-p([[:space:]]|,|$)'; then
    if [[ -n "$TDD_AGENT_EXTRA_ARGS" ]]; then
      # shellcheck disable=SC2086
      codex $TDD_AGENT_EXTRA_ARGS -p "$(cat "$prompt_file")" > "$log_file" 2>&1 || rc=$?
    else
      codex -p "$(cat "$prompt_file")" > "$log_file" 2>&1 || rc=$?
    fi
  else
    if [[ -n "$TDD_AGENT_EXTRA_ARGS" ]]; then
      # shellcheck disable=SC2086
      codex $TDD_AGENT_EXTRA_ARGS "$(cat "$prompt_file")" > "$log_file" 2>&1 || rc=$?
    else
      codex "$(cat "$prompt_file")" > "$log_file" 2>&1 || rc=$?
    fi
  fi

  rm -f "$prompt_file"
  return "$rc"
}

run_agent() {
  local phase="$1"
  local system_prompt="$2"
  local user_prompt="$3"
  local allowed_tools="$4"
  local log_file="$TDD_LOG_DIR/${phase}.log"
  local exit_code=0

  case "$TDD_AGENT_BIN" in
    claude)
      if ! command -v claude >/dev/null 2>&1; then
        echo -e "${RED}Error: claude CLI not found in PATH.${NC}" >&2
        return 127
      fi
      if [[ -n "$TDD_AGENT_EXTRA_ARGS" ]]; then
        # shellcheck disable=SC2086
        claude \
          --output-format stream-json \
          --append-system-prompt "$system_prompt" \
          --allowed-tools "$allowed_tools" \
          $TDD_AGENT_EXTRA_ARGS \
          -p "$user_prompt" \
          > "$log_file" 2>&1 || exit_code=$?
      else
        claude \
          --output-format stream-json \
          --append-system-prompt "$system_prompt" \
          --allowed-tools "$allowed_tools" \
          -p "$user_prompt" \
          > "$log_file" 2>&1 || exit_code=$?
      fi
      ;;
    codex)
      _run_codex_agent "$phase" "$system_prompt" "$user_prompt" "$allowed_tools" "$log_file" || exit_code=$?
      ;;
    *)
      echo -e "${RED}Error: Unsupported TDD_AGENT_BIN '$TDD_AGENT_BIN'. Use 'claude' or 'codex'.${NC}" >&2
      return 2
      ;;
  esac

  return "$exit_code"
}

_phase_summary() {
  # Extract the final agent message from logs and print a compact summary.
  # For Claude stream-json logs, parse assistant text.
  # For plain text logs (e.g., codex), fall back to the last non-empty line.
  local phase="$1"
  local exit_code="$2"
  local log="$TDD_LOG_DIR/${phase}.log"

  local summary
  summary="$(tail -20 "$log" 2>/dev/null | python3 -c "
import json, sys
texts = []
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        if d.get('type') == 'assistant':
            for c in d.get('message', {}).get('content', []):
                if c.get('type') == 'text':
                    texts.append(c['text'])
    except Exception:
        pass
if texts:
    print(texts[-1][:500])
" 2>/dev/null)"

  if [[ -z "$summary" ]]; then
    summary="$(tail -40 "$log" 2>/dev/null | sed '/^[[:space:]]*$/d' | tail -1 | cut -c1-500)"
  fi

  if [[ -n "$summary" ]]; then
    printf '%b%s%b\n' "${YELLOW}[${phase}]${NC} " "$summary" ""
  fi
  printf '%b\n' "${YELLOW}[${phase}]${NC} Phase complete (exit: $exit_code). Log: $log"
  return "$exit_code"
}

# ──────────────────────────────────────────────────────────────
# Phase Runners
# ──────────────────────────────────────────────────────────────

run_red() {
  local spec_file="${1:?Usage: tdd.sh red <spec-file>}"

  if [[ ! -f "$spec_file" ]]; then
    echo -e "${RED}Error: Spec file not found: $spec_file${NC}" >&2
    exit 1
  fi

  echo ""
  echo -e "${RED}======================================================${NC}"
  echo -e "${RED}  TDD RED PHASE -- Writing Failing Tests${NC}"
  echo -e "${RED}======================================================${NC}"
  echo -e "  Spec:  $spec_file"
  echo -e "  Tests: $TEST_DIRS"
  echo ""

  # Ensure tests are writable for the test author
  unlock_tests 2>/dev/null || true

  export TDD_PHASE="red"

  local system_prompt
  system_prompt="$(cat "$PROMPT_DIR/tdd-red.md")

## Context
- Design spec path: $spec_file
- Test directories: $TEST_DIRS
- Build command: $BUILD_CMD
- Test command: $TEST_CMD
- Existing test files: $(find_test_files | wc -l | tr -d ' ') file(s) in $TEST_DIRS (use Glob to discover)

Read the spec file first, then write your tests."

  local user_prompt
  user_prompt="Read the spec file first, then write your tests."

  local exit_code=0
  run_agent "red" "$system_prompt" "$user_prompt" "Read,Write,Edit,Bash" || exit_code=$?

  _phase_summary "red" "$exit_code"
}

run_green() {
  # Verify tests exist
  local test_count
  test_count=$(find_test_files | wc -l | tr -d ' ')
  if [[ "$test_count" -eq 0 ]]; then
    echo -e "${RED}Error: No test files found. Run 'tdd.sh red <spec>' first.${NC}" >&2
    exit 1
  fi

  echo ""
  echo -e "${GREEN}======================================================${NC}"
  echo -e "${GREEN}  TDD GREEN PHASE -- Implementing to Pass${NC}"
  echo -e "${GREEN}======================================================${NC}"
  echo -e "  Source: $SRC_DIR"
  echo -e "  Tests:  $test_count file(s)"
  echo ""

  # OS-level enforcement
  lock_tests
  ensure_hooks_executable

  export TDD_PHASE="green"

  # Unlock on exit regardless of success/failure
  trap unlock_tests EXIT

  local system_prompt
  system_prompt="$(cat "$PROMPT_DIR/tdd-green.md")

## Context
- Source directory: $SRC_DIR
- Test directories: $TEST_DIRS (READ-ONLY -- do not attempt to modify)
- Build command: $BUILD_CMD
- Test command: ./$_SD/scripts/test-summary.sh $TEST_CMD
- Full test log: $TDD_LOG_DIR/test-output.log (Read this file for detailed failure tracebacks)
- Test files: $(find_test_files | wc -l | tr -d ' ') file(s) in $TEST_DIRS (use Glob to discover)

Start by reading the test files to understand what's expected, then implement iteratively. Always use the test command above — it prints a compact summary. If you need full tracebacks, Read $TDD_LOG_DIR/test-output.log."

  local user_prompt
  user_prompt="Read the test files to understand what's expected, then implement iteratively."

  local exit_code=0
  run_agent "green" "$system_prompt" "$user_prompt" "Read,Write,Edit,Bash" || exit_code=$?

  _phase_summary "green" "$exit_code"
}

run_refactor() {
  echo ""
  echo -e "${BLUE}======================================================${NC}"
  echo -e "${BLUE}  TDD REFACTOR PHASE -- Improving Quality${NC}"
  echo -e "${BLUE}======================================================${NC}"
  echo -e "  Tests: $TEST_DIRS"
  echo ""

  # Ensure tests are unlocked (refactor may touch test readability)
  unlock_tests 2>/dev/null || true

  export TDD_PHASE="refactor"

  local system_prompt
  system_prompt="$(cat "$PROMPT_DIR/tdd-refactor.md")

## Context
- Source directory: $SRC_DIR
- Test directories: $TEST_DIRS
- Build command: $BUILD_CMD
- Test command: ./$_SD/scripts/test-summary.sh $TEST_CMD
- Full test log: $TDD_LOG_DIR/test-output.log (Read this file for detailed failure tracebacks)

Start by running the full test suite to confirm your green baseline, then refactor. Always use the test command above — it prints a compact summary."

  local user_prompt
  user_prompt="Run the full test suite to confirm your green baseline, then refactor."

  local exit_code=0
  run_agent "refactor" "$system_prompt" "$user_prompt" "Read,Write,Edit,Bash" || exit_code=$?

  _phase_summary "refactor" "$exit_code"
}

run_breadcrumbs() {
  # Update navigation docs before shipping commits.
  local spec_file="${1:?Usage: run_breadcrumbs <spec-file>}"
  local breadcrumbs_prompt="$PROMPT_DIR/tdd-breadcrumbs.md"

  if [[ ! -f "$spec_file" ]]; then
    echo -e "${RED}Error: Spec file not found: $spec_file${NC}" >&2
    exit 1
  fi

  if [[ ! -f "$breadcrumbs_prompt" ]]; then
    echo -e "${RED}Error: Breadcrumb prompt not found: $breadcrumbs_prompt${NC}" >&2
    exit 1
  fi

  echo ""
  echo -e "${BLUE}======================================================${NC}"
  echo -e "${BLUE}  TDD BREADCRUMBS PHASE -- Updating navigation docs${NC}"
  echo -e "${BLUE}======================================================${NC}"
  echo -e "  Spec: $spec_file"
  echo ""

  export TDD_PHASE="breadcrumbs"
  local exit_code=0

  local changed_files
  changed_files=$(git diff --name-only HEAD 2>/dev/null || echo 'unknown')

  local system_prompt
  system_prompt="$(cat "$breadcrumbs_prompt")

## Context
- Spec file: $spec_file
- Source directory: $SRC_DIR
- Test directories: $TEST_DIRS
- Build command: $BUILD_CMD
- Test command: ./$_SD/scripts/test-summary.sh $TEST_CMD
- Full test log: $TDD_LOG_DIR/test-output.log (Read this file for detailed failure tracebacks)
- Files changed in this cycle:
$changed_files

Read the spec and update breadcrumb files with accurate status/counts."

  local user_prompt
  user_prompt="Read the spec and update CLAUDE.md, AGENTS.md, $_SD/LAST_TOUCH.md, and affected directory README.md files before shipping."

  run_agent "breadcrumbs" "$system_prompt" "$user_prompt" "Read,Write,Edit,Bash,Glob,Grep" || exit_code=$?

  _phase_summary "breadcrumbs" "$exit_code"
}

run_ship() {
  # Ship the results of a TDD cycle: breadcrumbs, commit, PR, optionally merge.
  # Called with the spec file path so we can derive the branch name.
  local spec_file="${1:?Usage: run_ship <spec-file>}"
  local feature_name
  feature_name="$(basename "$spec_file" .md)"
  local branch="tdd/${feature_name}"

  echo ""
  echo -e "${YELLOW}======================================================${NC}"
  echo -e "${YELLOW}  SHIPPING -- breadcrumbs, commit, PR, archive spec${NC}"
  echo -e "${YELLOW}======================================================${NC}"
  echo ""

  # Breadcrumbs are mandatory before commit.
  run_breadcrumbs "$spec_file"

  # Create feature branch
  git checkout -b "$branch" 2>/dev/null || git checkout "$branch"

  # Stage all changes and commit
  git add -A
  git commit -m "feat(${feature_name}): implement via TDD cycle

Spec: ${spec_file}
Red-green-refactor complete. All tests passing."

  # Push and create PR (skip if no remote)
  if git remote get-url origin &>/dev/null; then
    git push -u origin "$branch"

    local pr_url
    pr_url=$(gh pr create \
      --base "$TDD_BASE_BRANCH" \
      --title "feat(${feature_name}): TDD cycle complete" \
      --body "## TDD Cycle: ${feature_name}

**Spec:** \`${spec_file}\`

### Phases completed
- [x] RED — failing tests written
- [x] GREEN — implementation passes all tests
- [x] REFACTOR — code quality improved

### Spec archived
The spec file has been deleted from the working tree. It is preserved in this branch's git history.

---
*Generated by [tdd-kit](https://github.com/kurtbell87/tdd-kit)*")

    echo -e "  ${GREEN}PR created:${NC} $pr_url"

    # Auto-merge if configured
    if [[ "$TDD_AUTO_MERGE" == "true" ]]; then
      echo -e "  ${YELLOW}Auto-merging...${NC}"
      gh pr merge "$pr_url" --merge
      echo -e "  ${GREEN}Merged.${NC}"

      # Return to base branch
      git checkout "$TDD_BASE_BRANCH"
      git pull

      # Delete branch if configured
      if [[ "$TDD_DELETE_BRANCH" == "true" ]]; then
        git branch -d "$branch" 2>/dev/null || true
        gh api -X DELETE "repos/{owner}/{repo}/git/refs/heads/${branch}" 2>/dev/null || true
        echo -e "  ${GREEN}Branch deleted.${NC}"
      fi
    fi

    # Archive: delete the spec file (it's preserved in git history)
    if [[ -f "$spec_file" ]]; then
      rm "$spec_file"
      git add "$spec_file"
      git commit -m "chore: archive spec ${spec_file}"
      git push
      echo -e "  ${GREEN}Spec archived:${NC} $spec_file removed (preserved in git history)"
    fi

    echo ""
    echo -e "${GREEN}======================================================${NC}"
    echo -e "${GREEN}  Shipped! PR: $pr_url${NC}"
    echo -e "${GREEN}======================================================${NC}"
  else
    echo -e "  ${YELLOW}No git remote 'origin' configured — skipping push and PR.${NC}"
    echo -e "  ${GREEN}Changes committed locally on branch:${NC} $branch"

    # Archive spec locally even without remote
    if [[ -f "$spec_file" ]]; then
      rm "$spec_file"
      git add "$spec_file"
      git commit -m "chore: archive spec ${spec_file}"
      echo -e "  ${GREEN}Spec archived:${NC} $spec_file removed (preserved in git history)"
    fi
  fi
}

run_full() {
  local spec_file="${1:?Usage: tdd.sh full <spec-file>}"

  echo -e "${YELLOW}Running full TDD cycle: RED -> GREEN -> REFACTOR -> SHIP${NC}"
  echo ""

  run_red "$spec_file"

  echo ""
  echo -e "${YELLOW}--- Red phase complete. Starting green phase... ---${NC}"
  echo ""

  run_green

  echo ""
  echo -e "${YELLOW}--- Green phase complete. Starting refactor phase... ---${NC}"
  echo ""

  run_refactor

  echo ""
  echo -e "${YELLOW}--- Refactor phase complete. Shipping... ---${NC}"
  echo ""

  run_ship "$spec_file"

  echo ""
  echo -e "${YELLOW}======================================================${NC}"
  echo -e "${YELLOW}  Full TDD cycle complete!${NC}"
  echo -e "${YELLOW}======================================================${NC}"
}

# ──────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────

ensure_dashboard_watchdog

case "${1:-help}" in
  red)      shift; run_red "$@" ;;
  green)    run_green ;;
  refactor) run_refactor ;;
  breadcrumbs) shift; run_breadcrumbs "$@" ;;
  ship)     shift; run_ship "$@" ;;
  full)     shift; run_full "$@" ;;
  watch)    shift; python3 "$_SD/scripts/tdd-watch.py" "$@" ;;
  help|*)
    echo "Usage: tdd.sh <phase> [args]"
    echo ""
    echo "Phases:"
    echo "  red   <spec-file>   Write failing tests from design spec"
    echo "  green               Implement minimum code to pass tests"
    echo "  refactor            Refactor while keeping tests green"
    echo "  breadcrumbs <spec-file> Update docs before shipping commits"
    echo "  ship  <spec-file>   Commit, create PR, archive spec"
    echo "  full  <spec-file>   Run all phases (red -> green -> refactor -> ship, with breadcrumbs)"
    echo "  watch [phase]       Live-tail a running phase (--resolve for summary)"
    echo ""
    echo "Environment:"
    echo "  TEST_DIRS='tests'                   Test directories (space-separated)"
    echo "  SRC_DIR='src'                       Source directory"
    echo "  BUILD_CMD='make'                    Build command"
    echo "  TEST_CMD='make test'                Test runner command"
    echo "  TDD_AGENT_BIN='claude'              Agent backend: claude|codex"
    echo "  TDD_AGENT_EXTRA_ARGS=''             Extra args passed to the selected agent CLI"
    echo "  TDD_LOG_DIR='/tmp/tdd-<project>'  Log directory"
    echo "  TDD_AUTO_MERGE='false'              Auto-merge PR after creation"
    echo "  TDD_DELETE_BRANCH='false'           Delete feature branch after merge"
    echo "  TDD_BASE_BRANCH='main'              Base branch for PRs"
    ;;
esac
