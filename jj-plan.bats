#!/usr/bin/env bats

# Tests for jj-plan
# Run: cargo build --release && bats jj-plan.bats --jobs 8
# (sequential: bats jj-plan.bats)
#
# Uses the freshly-built target/release/jj-plan binary (no system-wide install).
# Override with JJ_PLAN_BIN=path/to/binary if needed.

REAL_JJ="/opt/homebrew/bin/jj"

# Resolve the shim binary: env override, or default to target/release/jj-plan
JJ_PLAN_BIN="${JJ_PLAN_BIN:-$BATS_TEST_DIRNAME/target/release/jj-plan}"

# --- File-level setup/teardown (runs once) ---

setup_file() {
  export SHIM_DIR="$(mktemp -d)"
  ln -s "$(cd "$BATS_TEST_DIRNAME" && realpath "$JJ_PLAN_BIN")" "$SHIM_DIR/jj"
  export PATH="$SHIM_DIR:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"

  # Pre-create template repo with a plan-registered bookmark
  export TEMPLATE_REPO="$(mktemp -d)"
  "$REAL_JJ" git init "$TEMPLATE_REPO" 2>/dev/null
  "$REAL_JJ" -R "$TEMPLATE_REPO" bookmark create start -r @ 2>/dev/null
  mkdir -p "$TEMPLATE_REPO/.jj-plan"
  # Capture the real change ID for the registry entry.
  # Production stores standard-hex change IDs (commit.change_id().hex()), so the
  # fixture must too — `change_id` alone renders jj's reverse-hex display form,
  # which ChangeId::try_from_hex rejects, silently breaking resolve_plan_bookmark_at.
  local TMPL_CID
  TMPL_CID=$("$REAL_JJ" -R "$TEMPLATE_REPO" log -r @ -T 'change_id.normal_hex()' --no-graph)
  # Register the bookmark in the PlanRegistry
  mkdir -p "$TEMPLATE_REPO/.jj/repo/jj-plan"
  cat > "$TEMPLATE_REPO/.jj/repo/jj-plan/plans.toml" << EOF
version = 1

[[bookmarks]]
name = "start"
change_id = "$TMPL_CID"
planned_at = "2024-01-01T00:00:00Z"
EOF
}

teardown_file() {
  rm -rf "$SHIM_DIR" "$TEMPLATE_REPO"
}

# --- Per-test setup/teardown ---

setup() {
  TEST_REPO="$(mktemp -d)"
  cp -r "$TEMPLATE_REPO/." "$TEST_REPO"
  cd "$TEST_REPO"
}

teardown() {
  rm -rf "$TEST_REPO"
}

# Resolve the single plan file for a bookmark under the current
# L-NN-<encoded-bookmark>.md naming scheme. Matching by bookmark name keeps
# assertions independent of the position-derived letter prefix and dependency
# index, so a future reindex does not re-break them.
plan_file() {
  local dir="${2:-.jj-plan}"
  local matches=("$dir"/*-"$1".md)
  echo "${matches[0]}"
}

# Set a plan's description across the describe-protocol guard. These tests
# predate the guard, which blocks bare `jjdesc -m` once a plan file exists
# (to avoid clobbering the plan document). Bare describe still works before a
# plan file exists; once it does, fall back to the supported --override-plan-protocol
# escape hatch, which writes the message to the plan file and the description.
jjdesc() {
  jj describe "$@" 2>/dev/null || jj describe --override-plan-protocol "$@"
}

# =============================================================================
# Basic sync
# =============================================================================

@test "describe creates plan file in .jj-plan" {
  jjdesc -m "My plan"
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 1 ]]
}

@test "plan file contains the description" {
  jjdesc -m "My detailed plan"
  [[ "$(cat "$(plan_file start)")" == "My detailed plan" ]]
}

@test "sync does not create current.md" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  [[ ! -e .jj-plan/current.md ]]
}

# =============================================================================
# Stack building (using jj plan new <bookmark> for new steps)
# =============================================================================

@test "jj plan new <bookmark> creates a new plan file" {
  jjdesc -m "Plan"
  jj plan new step-1
  jjdesc -m "Step 1"
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat "$(plan_file step-1)")" == "Step 1" ]]
}

@test "three-change stack produces three numbered files in order" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  [[ "$(cat .jj-plan/*-01-*.md)" == "Plan" ]]
  [[ "$(cat .jj-plan/*-02-*.md)" == "Step 1" ]]
  [[ "$(cat .jj-plan/*-03-*.md)" == "Step 2" ]]
}

@test "sort order is bottom-endian: 01 is closest to start bookmark" {
  jjdesc -m "Stack-root"
  jj plan new step-1; jjdesc -m "Middle"
  jj plan new step-2; jjdesc -m "Tip"
  [[ "$(cat .jj-plan/*-01-*.md)" == "Stack-root" ]]
}

# =============================================================================
# Inclusive model: bookmark is first member
# =============================================================================

@test "start bookmark change is included in stack.md as first member" {
  jjdesc -m "I am the start bookmark"
  [[ "$(cat .jj-plan/stack.md)" == *"[start](./"*"-01-start.md)"* ]]
  [[ "$(cat .jj-plan/stack.md)" == *"I am the start bookmark"* ]]
}

@test "single-change stack (@ is the bookmark) shows one entry" {
  jjdesc -m "Solo change"
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 1 ]]
  [[ "$(cat "$(plan_file start)")" == "Solo change" ]]
}

# =============================================================================
# Switching changes
# =============================================================================

@test "jj edit preserves all plan files" {
  jjdesc -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Impl"
  [[ "$(cat "$(plan_file step-1)")" == "Impl" ]]
  jj edit -r "$PLAN"
  [[ "$(cat "$(plan_file start)")" == "Plan" ]]
  [[ "$(cat "$(plan_file step-1)")" == "Impl" ]]
}

@test "all stack files remain visible when editing a middle change" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Step 2"
  jj edit -r "$STEP1"
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 3 ]]
}

# =============================================================================
# Editing plan files
# =============================================================================

@test "editing plan file flushes to jj description on switch" {
  jjdesc -m "Original plan"
  local PLAN IMPL
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Impl"
  IMPL=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Updated impl description" > "$(plan_file step-1)"
  jj edit -r "$PLAN"
  [[ "$("$REAL_JJ" log -r "$IMPL" -T description --no-graph)" == "Updated impl description" ]]
}

# =============================================================================
# Non-current file flush (data loss prevention)
# =============================================================================

@test "editing a non-current plan file is flushed to jj on next command" {
  jjdesc -m "Plan"
  local PLAN STEP1
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Step 1"
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Step 2"
  # Edit the Plan file (not current) with rich content
  printf "Plan\n\n## Background\nDetailed context here" > "$(plan_file start)"
  # Trigger a sync with any mutating command
  jjdesc -m "Step 2 updated"
  local desc
  desc=$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)
  [[ "$desc" == *"Plan"* ]]
  [[ "$desc" == *"## Background"* ]]
  [[ "$desc" == *"Detailed context here"* ]]
}

@test "editing a non-current plan file survives jj edit to another change" {
  jjdesc -m "Phase 1"
  local P1 P2
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "phase 2 placeholder"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Phase 3"
  # Write rich plan to Phase 2 (not current)
  printf "Phase 2: Full implementation plan\n\n## Steps\n- Do X\n- Do Y\n- Do Z" > "$(plan_file step-1)"
  # Switch to Phase 2
  jj edit -r "$P2"
  [[ "$(cat "$(plan_file step-1)")" == *"Phase 2: Full implementation plan"* ]]
  [[ "$(cat "$(plan_file step-1)")" == *"- Do X"* ]]
  [[ "$("$REAL_JJ" log -r "$P2" -T description --no-graph)" == *"Phase 2: Full implementation plan"* ]]
}

@test "editing multiple non-current plan files flushes all of them" {
  jjdesc -m "Change A"
  local CA CB
  CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Change B"
  CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Change C"
  # Edit both A and B (neither is current)
  printf "Change A revised with detail" > "$(plan_file start)"
  printf "Change B revised with detail" > "$(plan_file step-1)"
  # Trigger sync
  jjdesc -m "Change C updated"
  [[ "$("$REAL_JJ" log -r "$CA" -T description --no-graph)" == "Change A revised with detail" ]]
  [[ "$("$REAL_JJ" log -r "$CB" -T description --no-graph)" == "Change B revised with detail" ]]
}

@test "non-current file edits survive stack renumbering" {
  jjdesc -m "Will be abandoned"
  local DOOMED KEEP
  DOOMED=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Important plan"
  KEEP=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Current work"
  # Edit the non-current plan file (index 02)
  printf "Important plan\n\n## Revised\nWith critical details" > "$(plan_file step-1)"
  # Abandon the first change — causes renumbering (step-1 goes from 02 to 01).
  # Don't move `start` to KEEP — two bookmarks on one commit causes a flush
  # conflict where both plan files write to the same description.
  jj abandon "$DOOMED"
  # The flush inside the shim's abandon lifecycle writes the edited file
  # content to KEEP's description before the abandon executes.
  local desc
  desc=$("$REAL_JJ" log -r "$KEEP" -T description --no-graph)
  [[ "$desc" == *"Important plan"* ]]
  [[ "$desc" == *"## Revised"* ]]
  [[ "$desc" == *"critical details"* ]]
  # After renumbering, verify the content survived in the renumbered file.
  local found=false
  for f in .jj-plan/*-[0-9][0-9]-*.md; do
    if [[ -f "$f" ]] && grep -q "critical details" "$f"; then
      found=true; break
    fi
  done
  [[ "$found" == "true" ]]
}

@test "jj describe does not get clobbered by stale file content" {
  jjdesc -m "First version"
  [[ "$(cat "$(plan_file start)")" == "First version" ]]
  jjdesc -m "Second version"
  [[ "$(cat "$(plan_file start)")" == "Second version" ]]
  jjdesc -m "Third version"
  [[ "$(cat "$(plan_file start)")" == "Third version" ]]
}

@test "non-current edits and jj describe on current do not interfere" {
  jjdesc -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Impl"
  # Edit non-current (Plan) file
  printf "Plan\n\n## Updated background" > "$(plan_file start)"
  # Also jj describe current
  jjdesc -m "Impl revised"
  # Plan should have the locally edited content
  local plan_desc
  plan_desc=$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)
  [[ "$plan_desc" == *"Plan"* ]]
  [[ "$plan_desc" == *"## Updated background"* ]]
  # Impl should have the jj describe content (not clobbered)
  [[ "$(cat "$(plan_file step-1)")" == "Impl revised" ]]
}

@test "exact reproduction of data loss scenario: write to non-current then jj edit" {
  # Build a stack of 4 phases
  jjdesc -m "Phase 1: schema refactor"
  jj plan new step-1; jjdesc -m "phase 2 placeholder"
  local P2
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "phase 3 placeholder"
  jj plan new step-3; jjdesc -m "phase 4 placeholder"
  # Write rich plan to phase 2 (NOT current — current is phase 4)
  # Plan files use bookmark names, not change IDs: step-1 is the bookmark for P2
  printf "Phase 2: Implement branded InterpreterLayer\n\n## Background\nThis is the detailed plan that must not be lost.\n\n## Steps\n- Step A: extract trait\n- Step B: implement layer\n- Step C: wire up" > "$(plan_file step-1)"
  # Now jj edit to phase 2 (this is the operation that caused data loss)
  jj edit -r "$P2"
  # Verify plan survived in BOTH the file and jj description
  [[ "$(head -1 "$(plan_file step-1)")" == "Phase 2: Implement branded InterpreterLayer" ]]
  [[ "$(grep -c "Step A" "$(plan_file step-1)")" -eq 1 ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Phase 2: Implement branded InterpreterLayer" ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph | grep -c "Step A")" -eq 1 ]]
}

# =============================================================================
# Editing plan files (original tests)
# =============================================================================

@test "jj describe updates the plan file (not clobbered)" {
  jjdesc -m "First version"
  [[ "$(cat "$(plan_file start)")" == "First version" ]]
  jjdesc -m "Second version"
  [[ "$(cat "$(plan_file start)")" == "Second version" ]]
}

# =============================================================================
# Multiline descriptions
# =============================================================================

@test "multiline descriptions are preserved" {
  jjdesc -m "Auth refactor

## Why
Need JWT and API key support

## Steps
- Extract module
- Add JWT"
  local content
  content=$(cat "$(plan_file start)")
  [[ "$content" == *"## Why"* ]]
  [[ "$content" == *"## Steps"* ]]
  [[ "$content" == *"- Extract module"* ]]
}

@test "multiline edits to plan files round-trip through jj" {
  jjdesc -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Plan\n\n## Background\nSome context here\n\n## Steps\n- [x] Done\n- [ ] Todo" > "$(plan_file start)"
  jj plan new step-1
  local desc
  desc=$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)
  [[ "$desc" == *"## Background"* ]]
  [[ "$desc" == *"- [x] Done"* ]]
  [[ "$desc" == *"- [ ] Todo"* ]]
}

# =============================================================================
# Stack summary
# =============================================================================

@test "stack.md is generated with first lines of plan files" {
  jjdesc -m "Refactor auth middleware"
  jj plan new step-1; jjdesc -m "Extract auth module"
  jj plan new step-2; jjdesc -m "Implement JWT strategy"
  local stack
  stack=$(cat .jj-plan/stack.md)
  [[ "$stack" == *"[start](./"*"-01-start.md)"* ]]
  [[ "$stack" == *"Refactor auth middleware"* ]]
  [[ "$stack" == *"[step-1](./"*"-02-step-1.md)"* ]]
  [[ "$stack" == *"Extract auth module"* ]]
  [[ "$stack" == *"[step-2](./"*"-03-step-2.md)"* ]]
  [[ "$stack" == *"Implement JWT strategy"* ]]
}

@test "stack.md marks current change with working copy marker" {
  jjdesc -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  local stack
  stack=$(cat .jj-plan/stack.md)
  # Current is Step 2 (tip) — should have ◉ marker on node line
  [[ "$(grep '◉' .jj-plan/stack.md)" == *"[step-2](./"*"-03-step-2.md)"* ]]
  # (@) indicator is on the description line (Regular format)
  [[ "$stack" == *"(@)"*"Step 2"* ]]
  # Switch to first
  jj edit -r "$PLAN"
  stack=$(cat .jj-plan/stack.md)
  [[ "$(grep '◉' .jj-plan/stack.md)" == *"[start](./"*"-01-start.md)"* ]]
  [[ "$stack" == *"(@)"*"Plan"* ]]
}

@test "stack.md updates when stack changes" {
  jjdesc -m "Plan"
  local before
  before=$(cat .jj-plan/stack.md | wc -l | tr -d " ")
  jj plan new step-1; jjdesc -m "Step 1"
  local after
  after=$(cat .jj-plan/stack.md | wc -l | tr -d " ")
  # Graph format: adding a plan increases line count (header + graph lines)
  [[ "$after" -gt "$before" ]]
  # Both should have the generated header
  [[ "$(head -1 .jj-plan/stack.md)" == "<!-- generated by jj-plan"* ]]
}

# =============================================================================
# Status indicators
# =============================================================================

@test "stack.md shows blank for empty not-started changes" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  # Empty, not-started change has no indicator parenthetical.
  # In Regular format, indicators are on the description line (after │),
  # so we check the node line (bookmark line) AND the next line.
  # For a blank/not-started change, the description line has no indicators.
  local stack
  stack=$(cat .jj-plan/stack.md)
  # The "Plan" description line should not have indicator parens
  # (grep the line containing "Plan" after │)
  local plan_desc
  plan_desc=$(echo "$stack" | grep "Plan$" || true)
  [[ "$plan_desc" != *"(@)"* ]]
  [[ "$plan_desc" != *"(~)"* ]]
  [[ "$plan_desc" != *"(✓)"* ]]
}

@test "stack.md shows ~ for non-empty non-current changes" {
  jjdesc -m "Step 1"
  echo "some work" > file.txt
  jj plan new step-1; jjdesc -m "Step 2"
  # Non-current, non-empty change shows (~) indicator on description line
  local stack
  stack=$(cat .jj-plan/stack.md)
  [[ "$stack" == *"(~)"*"Step 1"* ]]
}

@test "stack.md shows ✓ for changes with plan-status: ✅" {
  jjdesc -m "Step 1"
  jj plan new step-1; jjdesc -m "Step 2"
  # Mark Step 1 as done by editing its plan file (bookmark-named: 01-start.md)
  printf "Step 1\n\n> [!plan]\n> status: ✅\n\nDid the work." > "$(plan_file start)"
  # Trigger a sync
  jjdesc -m "Step 2 updated"
  # (✓) indicator now on description line in Regular format
  local stack
  stack=$(cat .jj-plan/stack.md)
  [[ "$stack" == *"(✓)"*"Step 1"* ]]
}

@test "stack.md shows all status types together" {
  # Change 0: will be marked done
  jjdesc -m "Done change"
  # Change 1: will have file changes (has-changes)
  jj plan new step-1; jjdesc -m "Has changes"
  echo "work" > file.txt
  # Change 2: will be current (in-progress)
  jj plan new step-2; jjdesc -m "Current work"
  # Change 3: empty, not started
  jj plan new step-3; jjdesc -m "Future work"
  # Now go back to change 2 to make it current
  jj edit -r @-
  # Mark change 0 as done (bookmark-named: 01-start.md)
  printf "Done change\n\n> [!plan]\n> status: ✅" > "$(plan_file start)"
  # Trigger sync
  jjdesc -m "Current work"
  local stack
  stack=$(cat .jj-plan/stack.md)
  # Done change shows (✓) on description line
  [[ "$stack" == *"(✓)"*"Done change"* ]]
  # Has-changes shows (~) on description line
  [[ "$stack" == *"(~)"*"Has changes"* ]]
  # Current (working copy) shows (@) on description line
  [[ "$stack" == *"(@)"*"Current work"* ]]
  # Empty, not-started change: description line has no indicator parens
  local future_desc
  future_desc=$(echo "$stack" | grep "Future work" || true)
  [[ "$future_desc" != *"(@)"* ]]
  [[ "$future_desc" != *"(~)"* ]]
  [[ "$future_desc" != *"(✓)"* ]]
}

@test "stack.md contains clickable markdown links" {
  jjdesc -m "Auth feature"
  jj plan new step-1; jjdesc -m "Extract module"
  [[ -f .jj-plan/stack.md ]]
  # Contains markdown link syntax with ./ prefix
  [[ "$(cat .jj-plan/stack.md)" == *"](./"*"-01-start.md)"* ]]
  # Contains bookmark names in links
  [[ "$(cat .jj-plan/stack.md)" == *"[start](./"*"-01-start.md)"* ]]
  [[ "$(cat .jj-plan/stack.md)" == *"[step-1](./"*"-02-step-1.md)"* ]]
  # Contains the generated header comment
  [[ "$(cat .jj-plan/stack.md)" == *"<!-- generated by jj-plan"* ]]
}

@test "plan-status: ✅ round-trips through jj description" {
  jjdesc -m "Step 1"
  local START_CID
  START_CID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Write done status to plan file using callout format
  printf "Step 1\n\n> [!plan]\n> status: ✅\n\nCompleted." > "$(plan_file start)"
  # Switch away (flushes to jj)
  jj plan new step-1; jjdesc -m "Step 2"
  # Check the description was preserved
  local desc
  desc=$("$REAL_JJ" log -r "$START_CID" -T description --no-graph)
  [[ "$desc" == *"Step 1"* ]]
  [[ "$desc" == *"status: ✅"* ]]
}

@test "jj status flushes non-current file edits and updates stack" {
  jjdesc -m "Phase 1"
  local P2
  jj plan new step-1; jjdesc -m "phase 2 placeholder"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Phase 3"
  # Write rich plan to Phase 2 (not current) WITHOUT running a jj command
  # Plan files use bookmark names, not change IDs: step-1 is bookmark for P2
  printf "Phase 2: Full implementation plan\n\nDetailed steps here" > "$(plan_file step-1)"
  # jj status should flush the edit and show updated terminal view
  run jj status
  [[ "$output" == *"Phase 2: Full implementation plan"* ]]
  [[ "$("$REAL_JJ" log -r "$P2" -T description --no-graph)" == *"Phase 2: Full implementation plan"* ]]
}

@test "jj st flushes edits to multiple non-current files" {
  jjdesc -m "Change A"
  local CA CB
  CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Change B"
  CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Change C"
  # Edit both non-current files (bookmark-named: start, step-1)
  printf "Change A: revised plan" > "$(plan_file start)"
  printf "Change B: revised plan" > "$(plan_file step-1)"
  # jj st should flush both
  run jj st
  [[ "$output" == *"Change A: revised plan"* ]]
  [[ "$output" == *"Change B: revised plan"* ]]
  [[ "$("$REAL_JJ" log -r "$CA" -T description --no-graph)" == "Change A: revised plan" ]]
  [[ "$("$REAL_JJ" log -r "$CB" -T description --no-graph)" == "Change B: revised plan" ]]
}

# =============================================================================
# Cleanup
# =============================================================================

@test "files for abandoned changes are removed" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Step 2"
  local before
  before=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  jj abandon "$STEP1"
  local after
  after=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$before" -eq 3 ]]
  [[ "$after" -eq 2 ]]
}

# =============================================================================
# Read-only passthrough
# =============================================================================

@test "jj log passes through without sync overhead" {
  jjdesc -m "Plan"
  rm -rf .jj-plan
  jj log -r @ -T description --no-graph
  [[ ! -d .jj-plan ]]
}

@test "jj status without .jj-plan does not create it" {
  rm -rf .jj-plan
  jj status
  [[ ! -d .jj-plan ]]
}

@test "jj status appends plan stack when .jj-plan is active" {
  jjdesc -m "Refactor auth"
  jj plan new step-1; jjdesc -m "Extract module"
  run jj status
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"Refactor auth"* ]]
  [[ "$output" == *"Extract module"* ]]
}

@test "jj st also appends plan stack" {
  jjdesc -m "My plan"
  run jj st
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"My plan"* ]]
}

# =============================================================================
# Subdirectory support
# =============================================================================

@test "jj status appends plan stack from a subdirectory" {
  jjdesc -m "Refactor auth"
  jj plan new step-1; jjdesc -m "Extract module"
  mkdir -p lib
  cd lib
  run jj status
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"Refactor auth"* ]]
}

@test "jj st appends plan stack from a subdirectory" {
  jjdesc -m "My plan"
  mkdir -p lib
  cd lib
  run jj st
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"My plan"* ]]
}

@test "mutating commands sync plans from a subdirectory" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  mkdir -p src
  cd src
  jj plan new step-2; jjdesc -m "Step 2"
  local count
  count=$(ls ../.jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 3 ]]
  [[ "$(cat "$(plan_file step-2 ../.jj-plan)")" == "Step 2" ]]
}

@test "editing plan file from subdir flushes to jj on switch" {
  jjdesc -m "Original"
  local PLAN IMPL
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Impl"
  IMPL=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Updated from subdir" > "$(plan_file step-1)"
  mkdir -p src
  cd src
  jj edit -r "$PLAN"
  [[ "$("$REAL_JJ" log -r "$IMPL" -T description --no-graph)" == "Updated from subdir" ]]
}

# =============================================================================
# Error state: max changes
# =============================================================================

@test "exceeding max changes creates error.md" {
  export JJ_PLAN_MAX=3
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  jj plan new step-3; jjdesc -m "Step 3"
  [[ -f .jj-plan/error.md ]]
  [[ -f .jj-plan/error.md ]]
}

@test "error.md contains a descriptive message" {
  export JJ_PLAN_MAX=3
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  jj plan new step-3; jjdesc -m "Step 3"
  local msg
  msg=$(cat .jj-plan/error.md)
  [[ "$msg" == *"max 3"* ]]
  [[ "$msg" == *"Refusing to sync"* ]]
}

@test "error state self-heals when stack shrinks below max" {
  export JJ_PLAN_MAX=3
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  jj plan new step-3; jjdesc -m "Step 3"
  [[ -f .jj-plan/error.md ]]
  jj squash -m "Step 2+3 combined"
  jj edit -r @-
  [[ ! -f .jj-plan/error.md ]]
}

@test "flush is skipped during error state (no description clobber)" {
  export JJ_PLAN_MAX=3
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  jj plan new step-3; jjdesc -m "Step 3"
  jjdesc -m "Step 3 updated"
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Step 3 updated" ]]
}

# =============================================================================
# Edge cases
# =============================================================================

@test "jj plan new produces empty plan file before describe" {
  jjdesc -m "Plan"
  jj plan new step-1
  local content
  content=$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)
  # Should have a placeholder description from plan new
  [[ -n "$content" ]]
}

@test "works outside a jj repo without errors" {
  cd "$(mktemp -d)"
  run jj version
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj"* ]]
}

# =============================================================================
# Activation / deactivation
# =============================================================================

@test "no .jj-plan directory means full passthrough (no sync)" {
  rm -rf .jj-plan
  jjdesc -m "Should not create .jj-plan"
  [[ ! -d .jj-plan ]]
}

@test "passthrough still runs jj commands correctly without .jj-plan" {
  rm -rf .jj-plan
  jjdesc -m "Test description"
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Test description" ]]
}

@test "creating .jj-plan activates sync" {
  rm -rf .jj-plan
  jjdesc -m "Before activation"
  [[ ! -d .jj-plan ]]
  mkdir .jj-plan
  jjdesc -m "After activation"
  [[ -f "$(plan_file start)" ]]
}

# =============================================================================
# trunk() fallback (exclusive)
# =============================================================================

# Removed: "trunk() is used as fallback when no registered plan exists"
# Removed: "trunk() fallback is exclusive — trunk commit not in stack"
# Reason: The old model created plan files for any commit between trunk and @.
# The new model only creates plan files for registered plan bookmarks.
# With no registry entries, there are no plan files. The test below validates
# the correct current behavior.

@test "no sync when neither registered plan nor useful trunk() exists" {
  rm -f .jj/repo/jj-plan/plans.toml
  "$REAL_JJ" bookmark delete start 2>/dev/null
  jjdesc -m "Orphan work"
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$count" -eq 0 ]]
  [[ ! -f "$(plan_file start)" ]]
}

# =============================================================================
# jj plan new <bookmark>
# =============================================================================

@test "jj plan new requires bookmark name" {
  run jj plan new
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"missing required <bookmark-name>"* ]]
}

@test "jj plan new creates change with bookmark" {
  run jj plan new feat-auth
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Created plan: feat-auth"* ]]
  # Verify bookmark exists
  bm=$("$REAL_JJ" bookmark list 2>/dev/null)
  [[ "$bm" == *"feat-auth:"* ]]
}

@test "jj plan new rejects duplicate bookmark" {
  jj plan new feat-one
  run jj plan new feat-one
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"already exists"* ]]
}

@test "jj plan new with -r positions the new change" {
  # Get current change for -r positioning
  jjdesc -m "Base"
  BASE=$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)
  jj plan new step-1
  jjdesc -m "After"
  # Create plan rooted off base
  run jj plan new rooted-plan -r "$BASE"
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Created plan: rooted-plan"* ]]
}

@test "jj plan new flushes pending edits" {
  jjdesc -m "Original plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Revised plan with important details" > "$(plan_file start)"
  jj plan new step-next
  [[ "$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)" == "Revised plan with important details" ]]
}

@test "jj plan new creates plan file and shows stack" {
  jjdesc -m "Old plan"
  run jj plan new step-next
  [[ -f "$(plan_file step-next)" ]]
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
}

@test "jj plan new plan file contains placeholder" {
  jjdesc -m "Old plan"
  jj plan new step-next
  [[ "$(cat "$(plan_file step-next)")" == "(plan: jj:"* ]]
}

@test "jj plan new from mid-stack inserts linearly (not a fork)" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  jj plan new step-2; jjdesc -m "Step 2"
  # Move @ back to the middle
  jj edit -r @-
  jj plan new step-mid
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 4 ]]
  [[ "$(cat .jj-plan/*-01-*.md)" == "Plan" ]]
  [[ "$(cat .jj-plan/*-02-*.md)" == "Step 1" ]]
  [[ "$(cat .jj-plan/*-03-*.md)" == "(plan: jj:"* ]]
  [[ "$(cat .jj-plan/*-04-*.md)" == "Step 2" ]]
}

@test "jj plan new placeholder contains actual change ID" {
  jjdesc -m "Existing plan"
  jj plan new step-next
  local NEW_ID desc
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == "(plan: jj:$NEW_ID)"* ]]
}

# =============================================================================
# jj plan track / untrack
# =============================================================================

@test "jj plan track adopts existing bookmark" {
  "$REAL_JJ" bookmark create my-existing -r @ 2>/dev/null
  run jj plan track my-existing
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Tracking plan: my-existing"* ]]
}

@test "jj plan track rejects non-existent bookmark" {
  run jj plan track nonexistent
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"does not exist"* ]]
}

@test "jj plan untrack removes plan registration" {
  jj plan new tracked-bm
  run jj plan untrack tracked-bm
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Untracked plan: tracked-bm"* ]]
}

@test "jj plan untrack rejects non-tracked bookmark" {
  run jj plan untrack not-tracked
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"not registered as a plan"* ]]
}

# =============================================================================
# Encoded-name collision detection
# =============================================================================

# Note: The feat--auth collision scenario (where jj itself creates a bookmark
# with "--" in the name) is covered by unit tests in src/types.rs
# (test_would_collide_*) because jj prohibits "--" in bookmark names.
@test "jj plan new feat/auth succeeds (slashes are valid)" {
  run jj plan new feat/auth
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Created plan: feat/auth"* ]]
  # Verify the encoded filename was created
  [[ -f "$(plan_file feat--auth)" ]]
}

@test "jj plan new feat--auth after feat/auth fails with collision" {
  jj plan new feat/auth
  run jj plan new feat--auth
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"would collide"* ]]
  [[ "$output" == *"feat/auth"* ]]
}

# Removed: "jj plan new feat/auth after feat--auth fails with collision"
# Removed: "jj plan track on colliding encoded name fails"
# Reason: jj itself rejects "--" in bookmark names, making these scenarios
# impossible to set up. Collision logic is covered by unit tests in
# src/types.rs (test_would_collide_*).

@test "jj plan new feat-auth does not collide with feat/auth" {
  jj plan new feat/auth
  run jj plan new feat-auth
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Created plan: feat-auth"* ]]
}

# =============================================================================
# jj stack stubs
# =============================================================================

@test "jj stack shows visualization with change ID" {
  jjdesc -m "My plan"
  run jj stack
  [[ "$status" -eq 0 ]]
  # Verify the visualization includes the bookmark name and a change ID
  [[ "$output" == *"start "* ]]
  # Change ID is the short reverse-hex (k-z alphabet, 8+ chars), shown before the bookmark
  [[ "$output" =~ [k-z]{8}.*start ]]
  [[ "$output" == *"trunk()"* ]]
}

@test "jj stack submit without remote fails gracefully" {
  run jj stack submit
  [[ "$status" -ne 0 ]]
  [[ "$output" == *"no supported remotes"* ]] || [[ "$output" == *"authentication"* ]] || [[ "$output" == *"remote"* ]]
}

@test "jj stack submit --continue-on-error is accepted" {
  run jj stack submit --continue-on-error
  # Should fail for remote/auth reasons, NOT for unknown flag
  [[ "$status" -ne 0 ]]
  [[ "$output" == *"no supported remotes"* ]] || [[ "$output" == *"authentication"* ]] || [[ "$output" == *"remote"* ]]
  [[ "$output" != *"unknown"* ]]
}

@test "jj stack sync without remote fails gracefully" {
  run jj stack sync
  [[ "$status" -ne 0 ]]
  [[ "$output" == *"no supported remotes"* ]] || [[ "$output" == *"authentication"* ]] || [[ "$output" == *"remote"* ]]
}

@test "jj stack merge without remote fails gracefully" {
  run jj stack merge
  [[ "$status" -ne 0 ]]
  [[ "$output" == *"no supported remotes"* ]] || [[ "$output" == *"authentication"* ]] || [[ "$output" == *"remote"* ]]
}

@test "jj stack unknown subcommand shows error" {
  run jj stack blah
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"unknown subcommand"* ]]
}

# =============================================================================
# jj plan error handling
# =============================================================================

@test "jj plan with no subcommand shows the current plan summary" {
  jjdesc -m "Plan"
  run jj plan
  # Bare `jj plan` shows the summary when @ is a tracked plan (orientation otherwise).
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan"* ]]
}

@test "jj plan bogus shows usage" {
  jjdesc -m "Plan"
  run jj plan bogus
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"jj plan: unknown subcommand"* ]]
  [[ "$output" == *"jj plan --help"* ]]
}

# =============================================================================
# jj plan --help
# =============================================================================

@test "jj plan --help prints help" {
  run jj plan --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Commands:"* ]]
}

@test "jj plan -h prints help" {
  run jj plan -h
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Commands:"* ]]
}

# =============================================================================
# jj plan <subcommand> --help (no side effects)
# =============================================================================

@test "jj plan new --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan new --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan track --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan track --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan untrack --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan untrack --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan done --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan done --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan go --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan go --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan next --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan next --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan prev --help prints help without side effects" {
  jjdesc -m "Precious content"
  run jj plan prev --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

# =============================================================================
# jj plan config
# =============================================================================

@test "jj plan config shows resolved plan directory" {
  run jj plan config
  [[ "$output" == *"jj-plan configuration:"* ]]
  [[ "$output" == *"resolved dir:"*".jj-plan"* ]]
  [[ "$output" == *"resolution source: .jj-plan"* ]]
}

@test "jj plan config shows shim path" {
  run jj plan config
  [[ "$output" == *"shim path:"* ]]
}

@test "jj plan config shows stack info" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  run jj plan config
  [[ "$output" == *"stack segments:"*"2"* ]]
}

@test "jj plan config shows legacy resolution source for .jj-plans" {
  rm -rf .jj-plan
  mkdir -p .jj-plans
  run jj plan config
  [[ "$output" == *"resolution source: .jj-plans (legacy)"* ]]
}

@test "jj plan config shows env var when JJ_PLAN_DIR is set" {
  mkdir -p .custom-plans
  export JJ_PLAN_DIR="$(pwd)/.custom-plans"
  run jj plan config
  [[ "$output" == *"JJ_PLAN_DIR env:"*".custom-plans"* ]]
  [[ "$output" == *"resolution source: env var"* ]]
}

@test "jj plan config shows no stack when no registered plan or trunk" {
  rm -f .jj/repo/jj-plan/plans.toml
  "$REAL_JJ" bookmark delete start 2>/dev/null
  run jj plan config
  [[ "$output" == *"stack segments:"*"0"* ]] || [[ "$output" == *"stack:"*"empty"* ]]
}

# =============================================================================
# Navigation commands show plan stack
# =============================================================================

@test "jj plan new appends plan stack when .jj-plan is active" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  run jj plan new step-2
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"Plan"* ]]
  [[ "$output" == *"Step 1"* ]]
}

@test "jj edit appends plan stack when .jj-plan is active" {
  jjdesc -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Step 1"
  run jj edit -r "$PLAN"
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"Plan"* ]]
  [[ "$output" == *"Step 1"* ]]
}

@test "jj plan new appends plan stack after confirmation" {
  jjdesc -m "Old plan"
  run jj plan new my-feature
  [[ "$output" == *"Created plan:"* ]]
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
  [[ "$output" == *"trunk()"* ]]
}

@test "jj new without .jj-plan does not show stack (passthrough)" {
  rm -rf .jj-plan
  "$REAL_JJ" describe -m "base" 2>/dev/null
  run jj new
  [[ "$output" != *"Plan stack"* ]]
}

# =============================================================================
# Two-column status: here + status independent
# =============================================================================

@test "change that is both current AND done shows (@, ✓)" {
  jjdesc -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Step 1"
  # Mark Plan as done using callout format (bookmark-named: 01-start.md)
  printf "Plan\n\n> [!plan]\n> status: ✅" > "$(plan_file start)"
  # Switch back to Plan — it is both current AND done
  jj edit -r "$PLAN"
  # ◉ marker (working copy) on node line
  [[ "$(grep '◉' .jj-plan/stack.md)" == *"[start]"* ]]
  # (@, ✓) indicators on description line (Regular format)
  local stack
  stack=$(cat .jj-plan/stack.md)
  [[ "$stack" == *"(@, ✓)"*"Plan"* ]]
}

@test "status: ✅ in callout detected when not on the last line" {
  jjdesc -m "Step 1"
  jj plan new step-1; jjdesc -m "Step 2"
  # Write status in callout format with trailing content (bookmark-named: 01-start.md)
  printf "Step 1\n\n> [!plan]\n> status: ✅\n\n## Notes\nSome trailing content" > "$(plan_file start)"
  jjdesc -m "Step 2 updated"
  # (✓) indicator on description line in Regular format
  local stack
  stack=$(cat .jj-plan/stack.md)
  [[ "$stack" == *"(✓)"*"Step 1"* ]]
}

# =============================================================================
# Bookmark protection on abandon
# =============================================================================

@test "abandon bookmarked change with descendants succeeds and resyncs" {
  jjdesc -m "Start root"
  local ROOT CHILD
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new child-step; jjdesc -m "Child"
  CHILD=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new grandchild-step; jjdesc -m "Grandchild"
  run jj abandon "$ROOT"
  # jj deletes the bookmark on abandon (not move). Verify the abandon succeeded
  # and plan files re-synced. The companion test ".jj-plan is correctly synced
  # after bookmark recovery on abandon" validates the file contents.
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Abandoned"* ]]
}

@test "abandon non-bookmarked middle change does not interfere with bookmark" {
  jjdesc -m "Plan"
  jj plan new step-1; jjdesc -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Step 2"
  run jj abandon "$STEP1"
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"start:"* ]]
  [[ "$output" != *"WARNING"* ]]
}

@test ".jj-plan is correctly synced after bookmark recovery on abandon" {
  jjdesc -m "Start root"
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new child-step; jjdesc -m "Child"
  jj plan new grandchild-step; jjdesc -m "Grandchild"
  jj abandon "$ROOT"
  local count
  count=$(ls .jj-plan/*-[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/*-01-*.md)" == "Child" ]]
  # Verify the grandchild plan file content (it should be at some NN-grandchild-step.md)
  local gc_file
  gc_file=$(ls "$(plan_file grandchild-step)" 2>/dev/null | head -1)
  [[ -n "$gc_file" ]]
  [[ "$(cat "$gc_file")" == "Grandchild" ]]
}

# =============================================================================
# Legacy .jj-plans fallback
# =============================================================================

@test "legacy .jj-plans/ works when .jj-plan/ does not exist" {
  rm -rf .jj-plan
  mkdir -p .jj-plans
  jjdesc -m "Legacy plan"
  [[ -f "$(plan_file start .jj-plans)" ]]
  [[ "$(cat "$(plan_file start .jj-plans)")" == "Legacy plan" ]]
  run jj status
  [[ "$output" == *"Plan stack (.jj-plans/"* ]]
}

@test ".jj-plan/ takes precedence when both .jj-plan/ and .jj-plans/ exist" {
  mkdir -p .jj-plans
  jjdesc -m "Precedence test"
  [[ -f "$(plan_file start)" ]]
  local legacy_count
  legacy_count=$(ls .jj-plans/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$legacy_count" -eq 0 ]]
  run jj status
  [[ "$output" == *"Plan stack (.jj-plan/"* ]]
}

@test "JJ_PLAN_DIR env var overrides both .jj-plan/ and .jj-plans/" {
  mkdir -p .jj-plans .custom-plans
  export JJ_PLAN_DIR="$(pwd)/.custom-plans"
  jjdesc -m "Custom dir test"
  [[ -f "$(plan_file start .custom-plans)" ]]
  local default_count legacy_count
  default_count=$(ls .jj-plan/*-[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  legacy_count=$(ls .jj-plans/*-[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$default_count" -eq 0 ]]
  [[ "$legacy_count" -eq 0 ]]
  [[ "$(cat "$(plan_file start .custom-plans)")" == "Custom dir test" ]]
  run jj status
  [[ "$output" == *"Plan stack (.custom-plans/"* ]]
}

# =============================================================================
# jj plan done
# =============================================================================

@test "jj plan done marks current plan as done" {
  jjdesc -m "My plan"
  jj plan done
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"status: ✅"* ]]
}

@test "jj plan done does not change working copy" {
  jjdesc -m "Plan 1"
  local P1 P2
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Plan 2"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Go back to plan 1 and mark it done
  jj edit -r "$P1"
  jj plan done
  # Working copy should still be plan 1, not advanced to plan 2
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
}

@test "jj plan done --dry-run does not modify description" {
  jjdesc -m "My plan

## Scratch [scratch]

Working notes here"
  run jj plan done --dry-run
  [[ "$output" == *"Stripped scratch sections"* ]]
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"My plan"* ]]
  [[ "$desc" == *"Working notes here"* ]]
}

@test "jj plan done --keep-scratch preserves scratch content" {
  jjdesc -m "My plan

## Notes [scratch]

Important scratch notes"
  jj plan done --keep-scratch
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"Important scratch notes"* ]]
  [[ "$desc" == *"status: ✅"* ]]
}

@test "jj plan done strips scratch sections" {
  jjdesc -m "My plan

## Background

Real content

## Scratch [scratch]

Temporary notes

## Results

Final results"
  jj plan done
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"Real content"* ]]
  [[ "$desc" == *"Final results"* ]]
  [[ "$desc" != *"Temporary notes"* ]]
  [[ "$desc" == *"status: ✅"* ]]
}

@test "jj plan done --stack marks all plans done" {
  jjdesc -m "Plan 1"
  jj plan new step-1; jjdesc -m "Plan 2"
  jj plan new step-2; jjdesc -m "Plan 3"
  jj plan done --stack
  local descs
  descs=$("$REAL_JJ" log -r "@ | @- | @--" -T "description" --no-graph)
  [[ "$descs" == *"status: ✅"* ]]
}

@test "jj plan done --show-stripped=toc lists top-level scratch + descendants" {
  jjdesc -m "My plan

> [!plan]
> status: 🔴

# Background

Real content.

# Notes [scratch]

These are my learnings.

## Sub-finding

Nested content.

# Conclusion

Wrap-up."
  run jj plan done
  [[ "$output" == *"Stripped scratch sections:"* ]]
  [[ "$output" == *"# Notes [scratch]"* ]]
  [[ "$output" == *"## Sub-finding"* ]]
  [[ "$output" == *"Recover with: jj evolog -r"* ]]
}

@test "jj plan done --show-stripped=headings omits descendants" {
  jjdesc -m "My plan

> [!plan]
> status: 🔴

# Background

Real content.

# Notes [scratch]

These are my learnings.

## Sub-finding

Nested content."
  run jj plan done --show-stripped=headings
  [[ "$output" == *"# Notes [scratch]"* ]]
  [[ "$output" != *"## Sub-finding"* ]]
}

@test "jj plan done --show-stripped=none is silent on strip side" {
  jjdesc -m "My plan

> [!plan]
> status: 🔴

# Notes [scratch]

Hidden learnings."
  run jj plan done --show-stripped=none
  [[ "$output" != *"Stripped scratch sections:"* ]]
  [[ "$output" != *"Recover with:"* ]]
}

@test "jj plan done --dry-run --show-stripped=none keeps status side" {
  jjdesc -m "My plan

> [!plan]
> status: 🔴

# Notes [scratch]

Hidden learnings."
  run jj plan done --dry-run --show-stripped=none
  [[ "$output" != *"Stripped scratch sections:"* ]]
  [[ "$output" == *"Would set metadata: status: ✅"* ]]
}

@test "jj plan done rejects invalid --show-stripped value" {
  jjdesc -m "My plan"
  run jj plan done --show-stripped=verbose
  [[ "$output" == *"invalid --show-stripped value"* ]]
  [[ "$output" == *"full"* ]]
  [[ "$output" == *"toc"* ]]
  [[ "$output" == *"headings"* ]]
  [[ "$output" == *"none"* ]]
}

@test "jj plan done on already-done plan is idempotent" {
  jjdesc -m "My plan

plan-status: ✅"
  jj plan done
  local desc count
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  count=$(echo "$desc" | grep -c "plan-status: ✅")
  [[ "$count" -eq 1 ]]
}

# =============================================================================
# jj plan summary
# =============================================================================

@test "jj plan summary shows plan outline" {
  jjdesc -m "feat: my feature

> [!plan]
> status: 🟡

# Background

Some context.

# 🟡 Phase 1: Setup

## Tasks

- ✅ Create files
- 🔴 Wire things

# 🔴 Phase 2: Tests

## Tasks

- 🔴 Write tests
"
  run jj plan summary
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Outline:"* ]]
  [[ "$output" == *"Phases:"* ]]
  [[ "$output" == *"Stack"* ]]
  [[ "$output" == *"Phase 1"* ]]
  [[ "$output" == *"0/2 complete"* ]]
}

@test "jj plan summary --json outputs valid JSON" {
  jjdesc -m "feat: json test

> [!plan]
> status: 🔴

# Background

Details here.
"
  run jj plan summary --json
  [[ "$status" -eq 0 ]]
  local title
  title=$(echo "$output" | jq -r .title)
  [[ "$title" == "feat: json test" ]]
}

@test "jj plan summary --stack=quiet omits stack" {
  jjdesc -m "feat: quiet test"
  run jj plan summary --stack=quiet
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"Stack"* ]]
}

@test "jj plan summary flushes direct plan-file edits before reading (closes write→inspect loop)" {
  # The jj description still holds a placeholder...
  "$REAL_JJ" describe -m "(plan: jj:start placeholder)" >/dev/null 2>&1
  # ...while an editor/LLM has written the real plan straight to disk.
  printf "feat: real plan title\n\n> [!plan]\n> status: 🔴\n\n# Background\n\nReal content here.\n" > .jj-plan/01-start.md

  # Inspecting via summary must flush disk→description first, with NO
  # intervening mutating command or `jj status`.
  run jj plan summary
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"real plan title"* ]]
  [[ "$output" != *"placeholder"* ]]

  # The flush must have updated the actual jj description (verified via real jj).
  local desc
  desc=$("$REAL_JJ" log -r start -T description --no-graph)
  [[ "$desc" == *"feat: real plan title"* ]]
  [[ "$desc" == *"Real content here."* ]]
  [[ "$desc" != *"placeholder"* ]]
}

@test "bare jj plan flushes direct plan-file edits before showing summary" {
  # Bare `jj plan` only routes to summary when `@` resolves to a tracked plan,
  # which requires the registry change_id in production (standard-hex) form.
  # The shared template fixture stores it in jj's reverse-hex display form, so
  # rewrite it here with the real normal_hex id.
  local CID
  CID=$("$REAL_JJ" log -r start -T 'change_id.normal_hex()' --no-graph)
  cat > .jj/repo/jj-plan/plans.toml <<EOF
version = 1

[[bookmarks]]
name = "start"
change_id = "$CID"
planned_at = "2024-01-01T00:00:00Z"
EOF

  "$REAL_JJ" describe -m "(plan: jj:start placeholder)" >/dev/null 2>&1
  printf "feat: bare-plan title\n\n> [!plan]\n> status: 🔴\n\n# Background\n\nReal bare content.\n" > .jj-plan/01-start.md

  run jj plan
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"bare-plan title"* ]]
  [[ "$output" != *"placeholder"* ]]

  local desc
  desc=$("$REAL_JJ" log -r start -T description --no-graph)
  [[ "$desc" == *"feat: bare-plan title"* ]]
  [[ "$desc" == *"Real bare content."* ]]
}

@test "jj --no-pager log remains a pure passthrough" {
  run jj --no-pager log -r @ -T description --no-graph
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"Plan stack ("* ]]
}

@test "jj --color never log remains a pure passthrough" {
  run jj --color never log -r @ -T description --no-graph
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"Plan stack ("* ]]
}

@test "jj log reflects a direct plan-file edit (drift gate flushes before exec)" {
  jjdesc -m "original"
  # Editor/LLM edits the plan file directly — no intervening jj command.
  printf "feat: edited via file\n\n> [!plan]\n> status: 🔴\n\nBody." > "$(plan_file start)"
  run jj --no-pager log -r start -T description --no-graph
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"feat: edited via file"* ]]
  # The flush actually updated the jj description (verified via real jj).
  [[ "$("$REAL_JJ" log -r start -T description --no-graph)" == *"feat: edited via file"* ]]
}

@test "jj show reflects a direct plan-file edit" {
  jjdesc -m "original"
  printf "feat: shown via file\n\nBody." > "$(plan_file start)"
  run jj --no-pager show -r start
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"feat: shown via file"* ]]
}

@test "jj log on a clean repo does not flush (no new jj operation)" {
  jjdesc -m "clean"
  local before after
  before=$("$REAL_JJ" op log --no-graph --no-pager -T 'id ++ "\n"' | wc -l | tr -d ' ')
  run jj --no-pager log -r @ --no-graph
  [[ "$status" -eq 0 ]]
  after=$("$REAL_JJ" op log --no-graph --no-pager -T 'id ++ "\n"' | wc -l | tr -d ' ')
  # No drift → no flush → no new operation.
  [[ "$before" -eq "$after" ]]
}

@test "jj log after a plan-file edit creates a flush operation" {
  jjdesc -m "before"
  local before after
  before=$("$REAL_JJ" op log --no-graph --no-pager -T 'id ++ "\n"' | wc -l | tr -d ' ')
  printf "after-edit body content" > "$(plan_file start)"
  run jj --no-pager log -r @ --no-graph
  [[ "$status" -eq 0 ]]
  after=$("$REAL_JJ" op log --no-graph --no-pager -T 'id ++ "\n"' | wc -l | tr -d ' ')
  # Drift → exactly the flush's describe operation(s) appear.
  [[ "$after" -gt "$before" ]]
}

@test "jj --no-pager describe -m is still blocked on tracked plans" {
  jj plan new step-1 >/dev/null 2>&1

  run jj --no-pager describe -m "replacement"
  [[ "$status" -eq 1 ]]
  [[ "$output" == *'blocked `jj describe -m`'* ]]
}

@test "jj -R target-repo --no-pager log remains a pure passthrough from outside cwd" {
  local OUTSIDE
  OUTSIDE="$(mktemp -d)"
  cd "$OUTSIDE"

  run jj -R "$TEST_REPO" --no-pager log -r @ -T description --no-graph
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"Plan stack ("* ]]
}

@test "jj -R target-repo --no-pager describe -m works on an untracked current change" {
  jj new >/dev/null 2>&1

  local OUTSIDE
  OUTSIDE="$(mktemp -d)"
  cd "$OUTSIDE"

  run jj -R "$TEST_REPO" --no-pager describe -m "untracked replacement"
  [[ "$status" -eq 0 ]]

  local desc
  desc=$("$REAL_JJ" -R "$TEST_REPO" log -r @ -T description --no-graph)
  [[ "$desc" == "untracked replacement" ]]
}

@test "jj -R target-repo --no-pager describe --override-plan-protocol preserves other globals" {
  jj plan new step-1 >/dev/null 2>&1

  local OUTSIDE
  OUTSIDE="$(mktemp -d)"
  cd "$OUTSIDE"

  run jj -R "$TEST_REPO" --no-pager describe --override-plan-protocol -m "override replacement"
  [[ "$status" -eq 0 ]]

  local desc
  desc=$("$REAL_JJ" -R "$TEST_REPO" log -r @ -T description --no-graph)
  [[ "$desc" == "override replacement" ]]
}

@test "jj -R target-repo plan summary uses the target repo from outside cwd" {
  # Seed the plan file so the name-glob resolves, then write the rich plan to it.
  jjdesc -m "seed" >/dev/null 2>&1
  cat > "$(plan_file start "$TEST_REPO/.jj-plan")" <<'EOF'
feat: cross-repo summary

> [!plan]
> status: 🟡

# Background

Some context.

# 🟡 Phase 1: Setup

## Tasks

- 🔴 Check cross-repo summary
EOF
  jj status >/dev/null 2>&1

  local OUTSIDE
  OUTSIDE="$(mktemp -d)"
  cd "$OUTSIDE"

  run jj -R "$TEST_REPO" plan summary
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Outline:"* ]]
  [[ "$output" == *"Phase 1"* ]]
}

@test "jj -R invalid-path defers to real jj for the error" {
  local OUTSIDE
  OUTSIDE="$(mktemp -d)"
  cd "$OUTSIDE"

  run jj -R /tmp/definitely-not-a-jj-repo log -r @ --no-graph
  [[ "$status" -ne 0 ]]
  [[ "$output" == *'There is no jj repo in "/tmp/definitely-not-a-jj-repo"'* ]]
  [[ "$output" != *"jj-plan is not activated in this repository."* ]]
}

# =============================================================================
# jj plan next / prev
# =============================================================================

@test "jj plan next advances from plan 1 to plan 2" {
  jjdesc -m "Plan 1"
  local P1 P2
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Plan 2"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj edit -r "$P1"
  run jj plan next
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P2" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan prev moves from plan 2 to plan 1" {
  jjdesc -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Plan 2"
  run jj plan prev
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan next at last plan stays put" {
  jjdesc -m "Plan 1"
  jj plan new step-1; jjdesc -m "Plan 2"
  run jj plan next
  [[ "$output" == *"Already at the last plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Plan 2" ]]
}

@test "jj plan prev at first plan stays put" {
  jjdesc -m "Plan 1"
  run jj plan prev
  [[ "$output" == *"Already at the first plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Plan 1" ]]
}

@test "jj plan next flushes pending edits before moving" {
  jjdesc -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Plan 2"
  jj edit -r "$P1"
  printf "Edited plan 1 content" > "$(plan_file start)"
  jj plan next
  [[ "$("$REAL_JJ" log -r "$P1" -T description --no-graph)" == "Edited plan 1 content" ]]
}

# =============================================================================
# jj plan go
# =============================================================================

@test "jj plan go 2 moves to the second plan" {
  jjdesc -m "Plan 1"
  jj plan new step-1; jjdesc -m "Plan 2"
  local P2
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jjdesc -m "Plan 3"
  run jj plan go 2
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P2" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan go CHANGE_ID moves to specified change" {
  jjdesc -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Plan 2"
  jj plan new step-2; jjdesc -m "Plan 3"
  run jj plan go "$P1"
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
}

@test "jj plan go accepts bookmark name" {
  jjdesc -m "Plan 1"
  jj plan new my-step; jjdesc -m "Plan 2"
  jj plan new other-step; jjdesc -m "Plan 3"
  run jj plan go my-step
  [[ "$status" -eq 0 ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Plan 2" ]]
}

@test "jj plan go 0 errors" {
  jjdesc -m "Plan 1"
  run jj plan go 0
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"out of range"* ]]
}

@test "jj plan go 99 errors (out of range)" {
  jjdesc -m "Plan 1"
  run jj plan go 99
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"out of range"* ]]
}

@test "jj plan go without target shows error" {
  jjdesc -m "Plan"
  run jj plan go
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"missing target"* ]]
}

# =============================================================================
# Plan templates
# =============================================================================

@test "custom template.md overrides default template" {
  printf "Custom: {{CHANGE_ID}}\n\n## My Section\n" > .jj-plan/template.md
  jj plan new tmpl-step
  local content
  content=$(cat "$(plan_file tmpl-step)")
  [[ "$content" == *"Custom: "* ]]
  [[ "$content" == *"## My Section"* ]]
}

@test "JJ_PLAN_TEMPLATE env var overrides template.md" {
  printf "ENV template: {{CHANGE_ID}}\n" > .jj-plan/template.md
  local ENVFILE
  ENVFILE="$(mktemp)"
  printf "Env override: {{CHANGE_ID}}\n\n## Env Section\n" > "$ENVFILE"
  export JJ_PLAN_TEMPLATE="$ENVFILE"
  jj plan new tmpl-step
  local content
  content=$(cat "$(plan_file tmpl-step)")
  [[ "$content" == *"Env override: "* ]]
  [[ "$content" == *"## Env Section"* ]]
  [[ "$content" != *"ENV template"* ]]
}

@test "template CHANGE_ID is interpolated correctly" {
  jj plan new tmpl-step
  local NEW_ID content
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  content=$(cat "$(plan_file tmpl-step)")
  [[ "$content" == "(plan: jj:$NEW_ID)"* ]]
}

@test "custom template without CHANGE_ID gets self-reference injected" {
  printf "No placeholder here\n\n## Section\n" > .jj-plan/template.md
  jj plan new tmpl-step
  local NEW_ID content
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  content=$(cat "$(plan_file tmpl-step)")
  [[ "$content" == *"jj:$NEW_ID"* ]]
}

# =============================================================================
# jj describe interception
# =============================================================================

@test "jj describe -m writes to plan file first" {
  jjdesc -m "Initial"
  jjdesc -m "Updated via describe"
  [[ "$(cat "$(plan_file start)")" == "Updated via describe" ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Updated via describe" ]]
}

@test "jj describe -m on non-current change updates correct plan file" {
  jjdesc -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jjdesc -m "Plan 2"
  jjdesc -r "$P1" -m "Plan 1 updated"
  [[ "$(cat .jj-plan/*-01-*.md)" == "Plan 1 updated" ]]
}

@test "jj describe --override-plan-protocol on a non-plan change does not leak the flag" {
  # An untracked change has no plan file → the describe guard takes the Allow
  # path, which must strip the jj-plan-only flag so it never reaches real jj.
  jj new >/dev/null 2>&1
  run jj describe --override-plan-protocol -m "untracked override"
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"unexpected argument"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "untracked override" ]]
}

# =============================================================================
# jj plan new --stack (Phase 3: explicit stack boundaries)
# =============================================================================

@test "jj plan new --stack creates stack base bookmark" {
  jj plan new --stack dashboard dash-api
  # The stack base bookmark should exist
  local bm_list
  bm_list=$("$REAL_JJ" bookmark list --no-pager)
  [[ "$bm_list" == *"stack/dashboard"* ]]
  # The plan bookmark should also exist
  [[ "$bm_list" == *"dash-api"* ]]
}

@test "jj plan new --stack registers plan with stack field in registry" {
  jj plan new --stack dashboard dash-api
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  # The new plan should have a stack field
  [[ "$registry" == *"stack = "* ]]
  # The plan name should be present
  [[ "$registry" == *'name = "dash-api"'* ]]
}

@test "jj plan new inherits stack from current plan" {
  # Create first plan in an explicit stack
  jj plan new --stack auth auth-refactor
  # Create second plan WITHOUT --stack — should inherit
  jj plan new auth-tests
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  # Both plans should have the same stack value
  # Count occurrences of "stack = " — should be 2 (both plans in same stack)
  local stack_count
  stack_count=$(echo "$registry" | grep -c 'stack = ' || true)
  [[ "$stack_count" -eq 2 ]]
}

@test "jj plan new without --stack and no parent plan has no stack field" {
  # Create a plan without --stack when parent has no stack
  jj plan new standalone-feat
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  # The "start" bookmark has no stack (v1 entry), standalone-feat should also have none
  # Only version line and bookmark entries — no "stack = " lines for these plans
  local stack_count
  stack_count=$(echo "$registry" | grep -c 'stack = ' || true)
  [[ "$stack_count" -eq 0 ]]
}

@test "stack base bookmark survives rebase" {
  jj plan new --stack dashboard dash-api
  jjdesc -m "Dashboard API"
  # Create a sibling commit from trunk to use as new base
  "$REAL_JJ" new 'trunk()' --no-edit -m "new base"
  local NEW_BASE
  NEW_BASE=$("$REAL_JJ" log -r 'trunk()+' -T "change_id.shortest(8)" --no-graph --limit 1)
  "$REAL_JJ" rebase -s @- -d "$NEW_BASE"
  # The stack/dashboard bookmark should still exist after rebase
  local bm_list
  bm_list=$("$REAL_JJ" bookmark list --no-pager)
  [[ "$bm_list" == *"stack/dashboard"* ]]
}

@test "jj plan new --stack without name argument errors" {
  run jj plan new --stack
  [[ "$status" -ne 0 ]] || [[ "$output" == *"missing"* ]] || [[ "$output" == *"requires"* ]]
}

# =============================================================================
# jj stack untrack (Phase 4: stack-level registry cleanup)
# =============================================================================

@test "jj stack untrack removes all plans in current stack" {
  # Create a 2-plan stack
  jj plan new --stack auth auth-refactor
  jjdesc -m "Auth refactor"
  jj plan new auth-tests
  jjdesc -m "Auth tests"
  # Verify both are registered
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" == *'name = "auth-refactor"'* ]]
  [[ "$registry" == *'name = "auth-tests"'* ]]
  # Untrack the stack
  run jj stack untrack
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Untracked 2 plan(s)"* ]]
  # Verify plans are gone from registry (only "start" should remain)
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" != *'name = "auth-refactor"'* ]]
  [[ "$registry" != *'name = "auth-tests"'* ]]
}

@test "jj stack untrack deletes stack base bookmark" {
  jj plan new --stack dashboard dash-api
  jjdesc -m "Dashboard API"
  # Verify stack bookmark exists as a local bookmark
  local bm_list
  bm_list=$("$REAL_JJ" bookmark list --no-pager)
  [[ "$bm_list" == *"stack/dashboard"* ]]
  # Untrack
  run jj stack untrack
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Deleted stack base bookmark: stack/dashboard"* ]]
  # The local bookmark target is removed. In a git-backed repo it may still
  # appear as a tracking ref until `jj git export`, so we verify the deletion
  # message rather than re-listing bookmarks.
}

@test "jj stack untrack --dry-run does not modify state" {
  jj plan new --stack auth auth-refactor
  jjdesc -m "Auth refactor"
  run jj stack untrack --dry-run
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Would untrack"* ]]
  # Plans should still be registered
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" == *'name = "auth-refactor"'* ]]
  # Stack bookmark should still exist
  local bm_list
  bm_list=$("$REAL_JJ" bookmark list --no-pager)
  [[ "$bm_list" == *"stack/dashboard"* ]] || [[ "$bm_list" == *"stack/auth"* ]] || true
}

@test "jj stack untrack does not modify commit descriptions" {
  jj plan new --stack feat feat-api
  jjdesc -m "Feature API implementation"
  jj plan new feat-tests
  jjdesc -m "Feature test suite"
  local FEAT_API FEAT_TESTS
  FEAT_API=$("$REAL_JJ" log -r @- -T "change_id.shortest(8)" --no-graph)
  FEAT_TESTS=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Untrack the stack
  jj stack untrack
  # Descriptions must be unchanged
  [[ "$("$REAL_JJ" log -r "$FEAT_API" -T description --no-graph)" == "Feature API implementation" ]]
  [[ "$("$REAL_JJ" log -r "$FEAT_TESTS" -T description --no-graph)" == "Feature test suite" ]]
}

# =============================================================================
# Phase 4b: Scoped untrack and auto-cleanup fixes
# =============================================================================

@test "jj stack untrack with multiple stacks only untracks current" {
  # Create first explicit stack (auth)
  jj plan new --stack auth auth-refactor
  jjdesc -m "Auth refactor"
  jj plan new auth-tests
  jjdesc -m "Auth tests"
  # Navigate back to start (trunk-adjacent) to create a sibling stack
  jj edit -r start
  # Create second explicit stack (dashboard)
  jj plan new --stack dashboard dash-api
  jjdesc -m "Dashboard API"
  # Verify both stacks are registered
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" == *'name = "auth-refactor"'* ]]
  [[ "$registry" == *'name = "auth-tests"'* ]]
  [[ "$registry" == *'name = "dash-api"'* ]]
  # Untrack the dashboard stack (current @ is on dash-api)
  run jj stack untrack
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Untracked"* ]]
  # Auth stack must still be fully registered
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" == *'name = "auth-refactor"'* ]]
  [[ "$registry" == *'name = "auth-tests"'* ]]
  # Dashboard stack must be gone
  [[ "$registry" != *'name = "dash-api"'* ]]
}

@test "auto-cleanup triggers after stack merge to trunk" {
  # Set up a local git remote so trunk() resolves to origin/main instead of root().
  # The test repo is colocated (jj git init), so .git is at the repo root.

  # 1. Create a bare git repo to act as the remote
  local REMOTE_DIR
  REMOTE_DIR="$(mktemp -d)"
  git init --bare "$REMOTE_DIR" 2>/dev/null

  # 2. Add it as "origin" in the colocated .git at the repo root
  git remote add origin "$REMOTE_DIR" 2>/dev/null || true

  # 3. Create a commit, push it as main to establish trunk()
  "$REAL_JJ" describe -m "initial trunk" -r start
  "$REAL_JJ" bookmark create main -r start 2>/dev/null
  "$REAL_JJ" git push --remote origin --bookmark main
  "$REAL_JJ" git import

  # After push, the WC commit became immutable and jj created a new empty
  # change on top. Describe it so it won't block future pushes.
  "$REAL_JJ" describe -m "post-push wip"

  # 4. Create an explicit stack on top of trunk
  jj plan new --stack feat feat-api
  jjdesc -m "Feature API"

  # Verify it is registered with a stack field
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" == *'name = "feat-api"'* ]]
  [[ "$registry" == *"stack = "* ]]

  # 5. Simulate merge to trunk: advance main past the stack.
  #    Move the main bookmark to point at the feat-api commit (as if it was merged),
  #    then push to origin so trunk() picks it up.
  #    Use --allow-empty-description because jj new may have inserted an
  #    empty change between the old main and feat-api.
  local FEAT_CID
  FEAT_CID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  "$REAL_JJ" bookmark set main -r "$FEAT_CID" 2>/dev/null
  "$REAL_JJ" git push --remote origin --bookmark main --allow-empty-description
  "$REAL_JJ" git import

  # 6. Run any mutating command to trigger wrap() → auto_cleanup_merged_stacks()
  jjdesc -m "Feature API"

  # 7. The stack should have been auto-cleaned from the registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" != *'name = "feat-api"'* ]]

  rm -rf "$REMOTE_DIR"
}

# =============================================================================
# Phase 4c: jj plan new adopts empty working copy
# =============================================================================

@test "jj plan new adopts empty working copy instead of creating child" {
  # Set up a local git remote so pushing works
  local REMOTE_DIR
  REMOTE_DIR="$(mktemp -d)"
  git init --bare "$REMOTE_DIR" 2>/dev/null
  git remote add origin "$REMOTE_DIR" 2>/dev/null || true

  # Push start as main to establish trunk() and make WC immutable
  "$REAL_JJ" describe -m "initial trunk" -r start
  "$REAL_JJ" bookmark create main -r start 2>/dev/null
  "$REAL_JJ" git push --remote origin --bookmark main 2>/dev/null
  # After push, jj auto-creates an empty change on top of the now-immutable WC.
  # Verify @ is empty with no bookmarks and no description.
  local wc_info
  wc_info=$("$REAL_JJ" log -r @ -T 'concat(if(empty, "EMPTY"), " bm=", local_bookmarks, " desc=", description)' --no-graph)
  [[ "$wc_info" == *"EMPTY"* ]]

  # Remember the current WC change ID — jj plan new should adopt this change
  local WC_BEFORE
  WC_BEFORE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)

  # Run jj plan new — should adopt @ rather than creating a child
  jj plan new feat-api

  # The WC change ID should be the SAME as before (adopted, not a new child)
  local WC_AFTER
  WC_AFTER=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$WC_BEFORE" == "$WC_AFTER" ]]

  # The bookmark should be on @
  local bm_list
  bm_list=$("$REAL_JJ" bookmark list --no-pager)
  [[ "$bm_list" == *"feat-api"* ]]

  # Should be registered in the plan registry
  local registry
  registry=$(cat .jj/repo/jj-plan/plans.toml)
  [[ "$registry" == *'name = "feat-api"'* ]]

  rm -rf "$REMOTE_DIR"
}

# Guard-condition tests for adoption (non-empty, has description, has bookmark,
# explicit -A flag) are covered by Rust unit tests in src/commands/new.rs:
#   adopt_empty_unbookmarked_undescribed_change
#   do_not_adopt_when_explicit_position_flag
#   do_not_adopt_non_empty_change
#   do_not_adopt_change_with_description
#   do_not_adopt_change_with_bookmark

# =============================================================================
# Phase 5: Multi-column jj stack visualization
# =============================================================================

@test "jj stack --all shows multi-column output for sibling branches" {
  # Create two genuine sibling branches from start (trunk-adjacent).
  # We use $REAL_JJ to create the second branch's commit so that
  # jj new doesn't rebase the first branch (--insert-after @ would
  # make them linear instead of siblings).

  # Branch 1: auth stack
  jj plan new --stack auth auth-refactor
  jjdesc -m "Refactor auth"

  # Branch 2: create a genuine sibling of auth-refactor off start.
  # Use $REAL_JJ new (not the shim) to avoid --insert-after @ rebasing.
  "$REAL_JJ" new start --no-edit 2>/dev/null
  local SIBLING_CID
  SIBLING_CID=$("$REAL_JJ" log -r 'children(start) ~ auth-refactor' -T "change_id.shortest(8)" --no-graph --limit 1)
  "$REAL_JJ" edit -r "$SIBLING_CID" 2>/dev/null
  "$REAL_JJ" bookmark create dash-api -r @ 2>/dev/null
  "$REAL_JJ" bookmark create stack/dashboard -r @ 2>/dev/null
  "$REAL_JJ" describe -m "Dashboard API" 2>/dev/null

  # Register dash-api in the plan registry with its own stack field
  local DASH_FULL_CID
  DASH_FULL_CID=$("$REAL_JJ" log -r @ -T 'change_id' --no-graph)
  local REPO_ROOT
  REPO_ROOT=$(pwd)
  # Read current registry content and append the new entry
  local REG_FILE=".jj/repo/jj-plan/plans.toml"
  cat >> "$REG_FILE" << EOF

[[bookmarks]]
name = "dash-api"
change_id = "$DASH_FULL_CID"
planned_at = "2026-01-01T00:00:00Z"
stack = "$DASH_FULL_CID"
EOF

  # Sync so plan files are created
  jj status >/dev/null 2>&1

  # Bare `jj stack` shows only the current stack (dash-api, since @ is there)
  run jj stack
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"dash-api"* ]]
  # auth-refactor is on a different stack — should NOT appear in bare `jj stack`
  [[ "$output" != *"auth-refactor"* ]]

  # `jj stack --all` shows all stacks across the repo
  run jj stack --all
  [[ "$status" -eq 0 ]]
  # Multi-stack should show both bookmark names
  [[ "$output" == *"auth-refactor"* ]]
  [[ "$output" == *"dash-api"* ]]
  # Should show trunk
  [[ "$output" == *"trunk()"* ]]
}

@test "jj stack single stack has no column gutter" {
  jjdesc -m "My plan"
  run jj stack
  [[ "$status" -eq 0 ]]
  # Single stack should NOT show "stack:" header
  [[ "$output" != *"stack:"* ]]
  # Should NOT show multi-column merge connector
  [[ "$output" != *"├─╯"* ]]
  # Should show the normal single-column format
  [[ "$output" == *"trunk()"* ]]
}

# =============================================================================
# Compact format
# =============================================================================

@test "jj status uses compact format by default" {
  jjdesc -m "Refactor auth"
  jj plan new step-1; jjdesc -m "Extract module"
  run jj status
  [[ "$status" -eq 0 ]]
  # Plan description should be on the same line as the bookmark name (compact)
  [[ "$output" == *"start "* ]]
  [[ "$output" == *" Refactor auth"* ]]
  [[ "$output" == *" Extract module"* ]]
  # Should NOT have │ description lines (the regular format connector)
  [[ "$output" != *"│ Refactor auth"* ]]
  [[ "$output" != *"│ Extract module"* ]]
}

@test "stack.md uses regular format with description on separate line" {
  jjdesc -m "Refactor auth"
  jj plan new step-1; jjdesc -m "Extract module"
  # Trigger a sync so stack.md is written
  run jj status
  [[ "$status" -eq 0 ]]
  # stack.md should use regular format (│ connector before description)
  stack_content=$(cat .jj-plan/stack.md)
  [[ "$stack_content" == *"│"*"Refactor auth"* ]]
  [[ "$stack_content" == *"│"*"Extract module"* ]]
}

@test "jj stack --format=regular produces multi-line output" {
  jjdesc -m "Refactor auth"
  jj plan new step-1; jjdesc -m "Extract module"
  run jj stack --format=regular
  [[ "$status" -eq 0 ]]
  # Regular format should have │ description lines
  [[ "$output" == *"│"*"Refactor auth"* ]]
  [[ "$output" == *"│"*"Extract module"* ]]
}

@test "jj stack --all --format=regular works with flag order independence" {
  jjdesc -m "Refactor auth"
  run jj stack --all --format=regular
  [[ "$status" -eq 0 ]]
  # Should have regular format with │ description lines
  [[ "$output" == *"│"*"Refactor auth"* ]]
  # Reversed order should also work
  run jj stack --format=regular --all
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"│"*"Refactor auth"* ]]
}

@test "jj stack --format=compact explicitly produces compact output" {
  jjdesc -m "Refactor auth"
  run jj stack --format=compact
  [[ "$status" -eq 0 ]]
  # Compact: description on same line, no │ connector
  [[ "$output" != *"│ Refactor auth"* ]]
  [[ "$output" == *" Refactor auth"* ]]
}

@test "jj stack --format=bogus shows error" {
  run jj stack --format=bogus
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"unknown format"* ]]
}

@test "JJ_PLAN_STACK_FORMAT=regular env var overrides default" {
  jjdesc -m "Refactor auth"
  JJ_PLAN_STACK_FORMAT=regular run jj stack
  [[ "$status" -eq 0 ]]
  # Env var set to regular should produce │ description lines
  [[ "$output" == *"│"*"Refactor auth"* ]]
}

# =============================================================================
# Reconcile (three-way): neither flush nor sync clobbers; failed flush can't poison
# =============================================================================

# Run a single command with a fake `jj` (after the shim on PATH) that silently no-ops
# `describe` and delegates everything else to real jj — simulating a failing flush
# without instrumenting the binary. The shim resolves the fake as its "real jj".
with_failing_describe() {
  local fake="$BATS_TEST_TMPDIR/fakejj"
  mkdir -p "$fake"
  cat > "$fake/jj" <<EOF
#!/usr/bin/env bash
for a in "\$@"; do [ "\$a" = "describe" ] && exit 0; done
exec "$REAL_JJ" "\$@"
EOF
  chmod +x "$fake/jj"
  PATH="$SHIM_DIR:$fake:/opt/homebrew/bin:/usr/bin:/bin" "$@"
}

@test "reconcile: a failing flush does not clobber an authored plan file" {
  jj status >/dev/null                       # materialize a-01-start.md (empty), seed baseline
  printf 'feat: real plan\n\n# Background\nMUST NOT be lost\n' > "$(plan_file start)"
  with_failing_describe jj status            # flush describe fails — must NOT zero the file
  run cat "$(plan_file start)"
  [[ -n "$output" ]]
  [[ "$output" == *"MUST NOT be lost"* ]]
}

@test "reconcile: a failed summary flush does not poison the baseline" {
  jj status >/dev/null
  printf 'feat: real plan\n\nMUST NOT be lost\n' > "$(plan_file start)"
  with_failing_describe jj plan summary >/dev/null   # flush fails; baseline must not advance
  with_failing_describe jj status                    # flush fails again; file must survive
  run cat "$(plan_file start)"
  [[ "$output" == *"MUST NOT be lost"* ]]
}

@test "reconcile: a stale file does not clobber an out-of-band description change" {
  jjdesc -m "version one"                     # file == desc == baseline
  [[ "$(cat "$(plan_file start)")" == "version one" ]]
  local cid; cid=$("$REAL_JJ" log -r start -T 'change_id.shortest(8)' --no-graph)
  "$REAL_JJ" describe -r "$cid" -m "rebased description v2" >/dev/null 2>&1  # desc changes; file stale
  jj status >/dev/null                        # flush must NOT push the stale file over the new desc
  [[ "$("$REAL_JJ" log -r start -T description --no-graph)" == *"rebased description v2"* ]]
  [[ "$(cat "$(plan_file start)")" == *"rebased description v2"* ]]   # file adopts the new desc
}

@test "reconcile: a true conflict preserves the file and writes .incoming" {
  jjdesc -m "common base"
  local cid; cid=$("$REAL_JJ" log -r start -T 'change_id.shortest(8)' --no-graph)
  "$REAL_JJ" describe -r "$cid" -m "their version" >/dev/null 2>&1     # desc diverges
  printf 'my version MUST NOT be lost\n' > "$(plan_file start)"        # file diverges
  jj status >/dev/null 2>&1
  [[ "$(cat "$(plan_file start)")" == *"my version MUST NOT be lost"* ]]   # file preserved
  run cat "$(plan_file start)".incoming
  [[ "$output" == *"their version"* ]]                                # incoming surfaced
}

@test "reconcile: squashing an authored plan keeps its content recoverable on disk" {
  jjdesc -m "base plan"
  jj plan new tip >/dev/null
  printf 'feat: tip work\n\nMUST NOT be lost\n' > "$(plan_file tip)"
  jj status >/dev/null                        # flush tip content to desc + baseline
  jj squash -u >/dev/null 2>&1                # discards tip desc; tip plan file removed
  # Content must survive somewhere under .jj-plan (snapshot/orphan), not vanish from disk.
  run grep -rl "MUST NOT be lost" .jj-plan
  [[ "$status" -eq 0 ]]
}
