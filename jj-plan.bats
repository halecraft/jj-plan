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
  # Create shim directory with jj symlink
  export SHIM_DIR="$(mktemp -d)"
  ln -s "$(cd "$BATS_TEST_DIRNAME" && realpath "$JJ_PLAN_BIN")" "$SHIM_DIR/jj"

  # Set PATH globally for all tests
  export PATH="$SHIM_DIR:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"

  # Pre-create template repo (avoids jj git init + bookmark set per test)
  export TEMPLATE_REPO="$(mktemp -d)"
  "$REAL_JJ" git init "$TEMPLATE_REPO" 2>/dev/null
  "$REAL_JJ" -R "$TEMPLATE_REPO" bookmark set stack -r @ -B 2>/dev/null
  mkdir -p "$TEMPLATE_REPO/.jj-plan"
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

# =============================================================================
# Basic sync
# =============================================================================

@test "describe creates plan file in .jj-plan" {
  jj describe -m "My plan"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 1 ]]
}

@test "plan file contains the description" {
  jj describe -m "My detailed plan"
  [[ "$(cat .jj-plan/current.md)" == "My detailed plan" ]]
}

@test "current.md is a symlink to the active change" {
  jj describe -m "Plan"
  [[ -L .jj-plan/current.md ]]
}

# =============================================================================
# Stack building
# =============================================================================

@test "jj new creates a new plan file and updates current.md" {
  jj describe -m "Plan"
  jj new
  jj describe -m "Step 1"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/current.md)" == "Step 1" ]]
}

@test "three-change stack produces three numbered files in order" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  [[ "$(cat .jj-plan/01-*.md)" == "Plan" ]]
  [[ "$(cat .jj-plan/02-*.md)" == "Step 1" ]]
  [[ "$(cat .jj-plan/03-*.md)" == "Step 2" ]]
}

@test "sort order is bottom-endian: 01 is closest to stack bookmark" {
  jj describe -m "Stack-root"
  jj new; jj describe -m "Middle"
  jj new; jj describe -m "Tip"
  [[ "$(cat .jj-plan/01-*.md)" == "Stack-root" ]]
}

# =============================================================================
# Inclusive model: bookmark is first member
# =============================================================================

@test "stack bookmark change is included in .stack as first member" {
  jj describe -m "I am the stack bookmark"
  [[ "$(cat .jj-plan/.stack)" == *"01-"*":: I am the stack bookmark"* ]]
}

@test "single-change stack (@ is the bookmark) shows one entry" {
  jj describe -m "Solo change"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 1 ]]
  [[ "$(cat .jj-plan/current.md)" == "Solo change" ]]
}

# =============================================================================
# Switching changes
# =============================================================================

@test "jj edit switches current.md symlink" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Impl"
  [[ "$(readlink .jj-plan/current.md)" == "02-"* ]]
  jj edit -r "$PLAN"
  [[ "$(readlink .jj-plan/current.md)" == "01-"* ]]
}

@test "all stack files remain visible when editing a middle change" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  jj edit -r "$STEP1"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 3 ]]
}

# =============================================================================
# Editing plan files
# =============================================================================

@test "editing current.md flushes to jj description on switch" {
  jj describe -m "Original plan"
  local PLAN IMPL
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Impl"
  IMPL=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Updated impl description" > .jj-plan/current.md
  jj edit -r "$PLAN"
  [[ "$("$REAL_JJ" log -r "$IMPL" -T description --no-graph)" == "Updated impl description" ]]
}

# =============================================================================
# Non-current file flush (data loss prevention)
# =============================================================================

@test "editing a non-current plan file is flushed to jj on next command" {
  jj describe -m "Plan"
  local PLAN STEP1
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 1"
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  # Edit the Plan file (not current) with rich content
  printf "Plan\n\n## Background\nDetailed context here" > ".jj-plan/01-${PLAN}.md"
  # Trigger a sync with any mutating command
  jj describe -m "Step 2 updated"
  local desc
  desc=$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)
  [[ "$desc" == *"Plan"* ]]
  [[ "$desc" == *"## Background"* ]]
  [[ "$desc" == *"Detailed context here"* ]]
}

@test "editing a non-current plan file survives jj edit to another change" {
  jj describe -m "Phase 1"
  local P1 P2
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "phase 2 placeholder"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Phase 3"
  # Write rich plan to Phase 2 (not current)
  printf "Phase 2: Full implementation plan\n\n## Steps\n- Do X\n- Do Y\n- Do Z" > ".jj-plan/02-${P2}.md"
  # Switch to Phase 2
  jj edit -r "$P2"
  [[ "$(cat .jj-plan/current.md)" == *"Phase 2: Full implementation plan"* ]]
  [[ "$(cat .jj-plan/current.md)" == *"- Do X"* ]]
  [[ "$("$REAL_JJ" log -r "$P2" -T description --no-graph)" == *"Phase 2: Full implementation plan"* ]]
}

@test "editing multiple non-current plan files flushes all of them" {
  jj describe -m "Change A"
  local CA CB
  CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Change B"
  CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Change C"
  # Edit both A and B (neither is current)
  printf "Change A revised with detail" > ".jj-plan/01-${CA}.md"
  printf "Change B revised with detail" > ".jj-plan/02-${CB}.md"
  # Trigger sync
  jj describe -m "Change C updated"
  [[ "$("$REAL_JJ" log -r "$CA" -T description --no-graph)" == "Change A revised with detail" ]]
  [[ "$("$REAL_JJ" log -r "$CB" -T description --no-graph)" == "Change B revised with detail" ]]
}

@test "non-current file edits survive stack renumbering" {
  jj describe -m "Will be abandoned"
  local DOOMED KEEP
  DOOMED=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Important plan"
  KEEP=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Current work"
  # Edit the Important plan file (index 02)
  printf "Important plan\n\n## Revised\nWith critical details" > ".jj-plan/02-${KEEP}.md"
  # Abandon the first change — causes renumbering (KEEP goes from 02 to 01)
  "$REAL_JJ" bookmark set stack -r "$KEEP" 2>/dev/null
  jj abandon "$DOOMED"
  local desc
  desc=$("$REAL_JJ" log -r "$KEEP" -T description --no-graph)
  [[ "$desc" == *"Important plan"* ]]
  [[ "$desc" == *"## Revised"* ]]
  [[ "$desc" == *"critical details"* ]]
  [[ "$(cat ".jj-plan/01-${KEEP}.md")" == *"Important plan"* ]]
}

@test "jj describe does not get clobbered by stale file content" {
  jj describe -m "First version"
  [[ "$(cat .jj-plan/current.md)" == "First version" ]]
  jj describe -m "Second version"
  [[ "$(cat .jj-plan/current.md)" == "Second version" ]]
  jj describe -m "Third version"
  [[ "$(cat .jj-plan/current.md)" == "Third version" ]]
}

@test "non-current edits and jj describe on current do not interfere" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Impl"
  # Edit non-current (Plan) file
  printf "Plan\n\n## Updated background" > ".jj-plan/01-${PLAN}.md"
  # Also jj describe current
  jj describe -m "Impl revised"
  # Plan should have the locally edited content
  local plan_desc
  plan_desc=$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)
  [[ "$plan_desc" == *"Plan"* ]]
  [[ "$plan_desc" == *"## Updated background"* ]]
  # Impl should have the jj describe content (not clobbered)
  [[ "$(cat .jj-plan/current.md)" == "Impl revised" ]]
}

@test "exact reproduction of data loss scenario: write to non-current then jj edit" {
  # Build a stack of 4 phases
  jj describe -m "Phase 1: schema refactor"
  jj new; jj describe -m "phase 2 placeholder"
  local P2
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "phase 3 placeholder"
  jj new; jj describe -m "phase 4 placeholder"
  # Write rich plan to phase 2 (NOT current — current is phase 4)
  printf "Phase 2: Implement branded InterpreterLayer\n\n## Background\nThis is the detailed plan that must not be lost.\n\n## Steps\n- Step A: extract trait\n- Step B: implement layer\n- Step C: wire up" > ".jj-plan/02-${P2}.md"
  # Now jj edit to phase 2 (this is the operation that caused data loss)
  jj edit -r "$P2"
  # Verify plan survived in BOTH the file and jj description
  [[ "$(head -1 .jj-plan/current.md)" == "Phase 2: Implement branded InterpreterLayer" ]]
  [[ "$(grep -c "Step A" .jj-plan/current.md)" -eq 1 ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Phase 2: Implement branded InterpreterLayer" ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph | grep -c "Step A")" -eq 1 ]]
}

# =============================================================================
# Editing plan files (original tests)
# =============================================================================

@test "jj describe updates the plan file (not clobbered)" {
  jj describe -m "First version"
  [[ "$(cat .jj-plan/current.md)" == "First version" ]]
  jj describe -m "Second version"
  [[ "$(cat .jj-plan/current.md)" == "Second version" ]]
}

# =============================================================================
# Multiline descriptions
# =============================================================================

@test "multiline descriptions are preserved" {
  jj describe -m "Auth refactor

## Why
Need JWT and API key support

## Steps
- Extract module
- Add JWT"
  local content
  content=$(cat .jj-plan/current.md)
  [[ "$content" == *"## Why"* ]]
  [[ "$content" == *"## Steps"* ]]
  [[ "$content" == *"- Extract module"* ]]
}

@test "multiline edits to plan files round-trip through jj" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Plan\n\n## Background\nSome context here\n\n## Steps\n- [x] Done\n- [ ] Todo" > .jj-plan/current.md
  jj new
  local desc
  desc=$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)
  [[ "$desc" == *"## Background"* ]]
  [[ "$desc" == *"- [x] Done"* ]]
  [[ "$desc" == *"- [ ] Todo"* ]]
}

# =============================================================================
# Stack summary
# =============================================================================

@test ".stack file is generated with first lines of plan files" {
  jj describe -m "Refactor auth middleware"
  jj new; jj describe -m "Extract auth module"
  jj new; jj describe -m "Implement JWT strategy"
  local stack
  stack=$(cat .jj-plan/.stack)
  [[ "$stack" == *"01-"*":: Refactor auth middleware"* ]]
  [[ "$stack" == *"02-"*":: Extract auth module"* ]]
  [[ "$stack" == *"03-"*":: Implement JWT strategy"* ]]
}

@test ".stack marks current change with asterisk" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  # Current is Step 2 (tip)
  [[ "$(grep '^\*' .jj-plan/.stack)" == *"03-"*":: Step 2"* ]]
  # Switch to first
  jj edit -r "$PLAN"
  [[ "$(grep '^\*' .jj-plan/.stack)" == *"01-"*":: Plan"* ]]
}

@test ".stack updates when stack changes" {
  jj describe -m "Plan"
  local before
  before=$(cat .jj-plan/.stack | wc -l | tr -d " ")
  jj new; jj describe -m "Step 1"
  local after
  after=$(cat .jj-plan/.stack | wc -l | tr -d " ")
  [[ "$before" -eq 1 ]]
  [[ "$after" -eq 2 ]]
}

# =============================================================================
# Status indicators
# =============================================================================

@test ".stack shows blank for empty not-started changes" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  [[ "$(grep '01-' .jj-plan/.stack)" == "    01-"* ]]
}

@test ".stack shows ~ for non-empty non-current changes" {
  jj describe -m "Step 1"
  echo "some work" > file.txt
  jj new; jj describe -m "Step 2"
  [[ "$(grep '01-' .jj-plan/.stack)" == "  ~ 01-"* ]]
}

@test ".stack shows ✓ for changes with plan-status: ✅ in description" {
  jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  # Mark Step 1 as done by editing its plan file
  printf "Step 1\n\nDid the work.\n\nplan-status: ✅" > ".jj-plan/01-${STEP1}.md"
  # Trigger a sync
  jj describe -m "Step 2 updated"
  [[ "$(grep '01-' .jj-plan/.stack)" == "  ✓ 01-"* ]]
}

@test ".stack shows all four status types together" {
  # Change 0: will be marked done
  jj describe -m "Done change"
  local DONE
  DONE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Change 1: will have file changes (has-changes)
  jj new; jj describe -m "Has changes"
  echo "work" > file.txt
  # Change 2: will be current (in-progress)
  jj new; jj describe -m "Current work"
  # Change 3: empty, not started
  jj new; jj describe -m "Future work"
  # Now go back to change 2 to make it current
  jj edit -r @-
  # Mark change 0 as done
  printf "Done change\n\nplan-status: ✅" > ".jj-plan/01-${DONE}.md"
  # Trigger sync
  jj describe -m "Current work"
  local stack
  stack=$(cat .jj-plan/.stack)
  [[ "$stack" == *"  ✓ 01-"*":: Done change"* ]]
  [[ "$stack" == *"  ~ 02-"*":: Has changes"* ]]
  [[ "$stack" == *"*   03-"*":: Current work"* ]]
  [[ "$stack" == *"    04-"*":: Future work"* ]]
}

@test "plan-status: ✅ round-trips through jj description" {
  jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Write done status to plan file
  printf "Step 1\n\nCompleted.\n\nplan-status: ✅" > .jj-plan/current.md
  # Switch away (flushes to jj)
  jj new; jj describe -m "Step 2"
  # Check the description was preserved
  local desc
  desc=$("$REAL_JJ" log -r "$STEP1" -T description --no-graph)
  [[ "$desc" == *"Step 1"* ]]
  [[ "$desc" == *"plan-status: ✅"* ]]
}

@test "jj status flushes non-current file edits and updates .stack" {
  jj describe -m "Phase 1"
  local P2
  jj new; jj describe -m "phase 2 placeholder"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Phase 3"
  # Write rich plan to Phase 2 (not current) WITHOUT running a jj command
  printf "Phase 2: Full implementation plan\n\nDetailed steps here" > ".jj-plan/02-${P2}.md"
  # jj status should flush the edit and show updated .stack
  run jj status
  [[ "$output" == *":: Phase 2: Full implementation plan"* ]]
  [[ "$("$REAL_JJ" log -r "$P2" -T description --no-graph)" == *"Phase 2: Full implementation plan"* ]]
}

@test "jj st flushes edits to multiple non-current files" {
  jj describe -m "Change A"
  local CA CB
  CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Change B"
  CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Change C"
  # Edit both non-current files
  printf "Change A: revised plan" > ".jj-plan/01-${CA}.md"
  printf "Change B: revised plan" > ".jj-plan/02-${CB}.md"
  # jj st should flush both
  run jj st
  [[ "$output" == *":: Change A: revised plan"* ]]
  [[ "$output" == *":: Change B: revised plan"* ]]
  [[ "$("$REAL_JJ" log -r "$CA" -T description --no-graph)" == "Change A: revised plan" ]]
  [[ "$("$REAL_JJ" log -r "$CB" -T description --no-graph)" == "Change B: revised plan" ]]
}

# =============================================================================
# "Done" workflow (replaces empty-stack cleanup)
# =============================================================================

@test "done workflow: new stack bookmark replaces old stack in .stack" {
  jj describe -m "Old task"
  jj new; jj describe -m "Old step 1"
  local before
  before=$(cat .jj-plan/.stack | wc -l | tr -d " ")
  # Done — start a new stack
  jj new
  "$REAL_JJ" bookmark set stack/new-task -r @ 2>/dev/null
  jj describe -m "New task"
  local after
  after=$(cat .jj-plan/.stack | wc -l | tr -d " ")
  [[ "$before" -eq 2 ]]
  [[ "$after" -eq 1 ]]
  [[ "$(cat .jj-plan/current.md)" == "New task" ]]
  ! grep -q "Old task" .jj-plan/.stack
}

@test "done workflow: old stack bookmark stays in jj history" {
  jj describe -m "Phase 1 work"
  local PHASE1
  PHASE1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new
  "$REAL_JJ" bookmark set stack/phase2 -r @ 2>/dev/null
  jj describe -m "Phase 2 work"
  [[ "$("$REAL_JJ" log -r "$PHASE1" -T description --no-graph)" == "Phase 1 work" ]]
}

@test "done workflow: moving bare stack bookmark forward starts new stack" {
  jj describe -m "Old plan"
  jj new; jj describe -m "Old step"
  # Move stack bookmark to a new change above
  jj new
  "$REAL_JJ" bookmark set stack -r @ 2>/dev/null
  jj describe -m "New plan"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 1 ]]
  [[ "$(cat .jj-plan/current.md)" == "New plan" ]]
}

# =============================================================================
# Cleanup
# =============================================================================

@test "files for abandoned changes are removed" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  local before
  before=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  jj abandon "$STEP1"
  local after
  after=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$before" -eq 3 ]]
  [[ "$after" -eq 2 ]]
}

# =============================================================================
# Read-only passthrough
# =============================================================================

@test "jj log passes through without sync overhead" {
  jj describe -m "Plan"
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
  jj describe -m "Refactor auth"
  jj new; jj describe -m "Extract module"
  run jj status
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"01-"*":: Refactor auth"* ]]
  [[ "$output" == *"02-"*":: Extract module"* ]]
}

@test "jj st also appends plan stack" {
  jj describe -m "My plan"
  run jj st
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: My plan"* ]]
}

# =============================================================================
# Subdirectory support
# =============================================================================

@test "jj status appends plan stack from a subdirectory" {
  jj describe -m "Refactor auth"
  jj new; jj describe -m "Extract module"
  mkdir -p src/deep/nested
  cd src/deep/nested
  run jj status
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"01-"*":: Refactor auth"* ]]
  [[ "$output" == *"02-"*":: Extract module"* ]]
}

@test "jj st appends plan stack from a subdirectory" {
  jj describe -m "My plan"
  mkdir -p lib
  cd lib
  run jj st
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: My plan"* ]]
}

@test "mutating commands sync plans from a subdirectory" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  mkdir -p src
  cd src
  jj new; jj describe -m "Step 2"
  local count
  count=$(ls ../.jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 3 ]]
  [[ "$(cat ../.jj-plan/current.md)" == "Step 2" ]]
}

@test "editing current.md from subdir flushes to jj on switch" {
  jj describe -m "Original"
  local PLAN IMPL
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Impl"
  IMPL=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Updated from subdir" > .jj-plan/current.md
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
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  jj new; jj describe -m "Step 3"
  [[ -f .jj-plan/error.md ]]
  [[ "$(readlink .jj-plan/current.md)" == "error.md" ]]
}

@test "error.md contains a descriptive message" {
  export JJ_PLAN_MAX=3
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  jj new; jj describe -m "Step 3"
  local msg
  msg=$(cat .jj-plan/error.md)
  [[ "$msg" == *"max 3"* ]]
  [[ "$msg" == *"Refusing to sync"* ]]
}

@test "error state self-heals when stack shrinks below max" {
  export JJ_PLAN_MAX=3
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  jj new; jj describe -m "Step 3"
  [[ -f .jj-plan/error.md ]]
  jj squash -m "Step 2+3 combined"
  jj edit -r @-
  [[ ! -f .jj-plan/error.md ]]
}

@test "flush is skipped during error state (no description clobber)" {
  export JJ_PLAN_MAX=3
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  jj new; jj describe -m "Step 3"
  jj describe -m "Step 3 updated"
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Step 3 updated" ]]
}

# =============================================================================
# Edge cases
# =============================================================================

@test "jj new from empty description produces empty plan file" {
  jj describe -m "Plan"
  jj new
  local content
  content=$(cat .jj-plan/current.md)
  [[ -z "$content" ]]
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
  jj describe -m "Should not create .jj-plan"
  [[ ! -d .jj-plan ]]
}

@test "passthrough still runs jj commands correctly without .jj-plan" {
  rm -rf .jj-plan
  jj describe -m "Test description"
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Test description" ]]
}

@test "creating .jj-plan activates sync" {
  rm -rf .jj-plan
  jj describe -m "Before activation"
  [[ ! -d .jj-plan ]]
  mkdir .jj-plan
  jj describe -m "After activation"
  [[ -f .jj-plan/current.md ]]
}

# =============================================================================
# Stack bookmark: bare "stack"
# =============================================================================

@test "bare stack bookmark is used when present" {
  # Template already has bare stack bookmark; build on it
  "$REAL_JJ" describe -m "initial" 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "landed feature" 2>/dev/null
  "$REAL_JJ" bookmark set stack -r @ 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "Active work"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "landed feature" ]]
  [[ "$(cat .jj-plan/current.md)" == "Active work" ]]
}

@test "bare stack bookmark excludes changes below it from the stack" {
  "$REAL_JJ" describe -m "old work 1" 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "old work 2" 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "stack start" 2>/dev/null
  "$REAL_JJ" bookmark set stack -r @ 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "New step 1" 2>/dev/null
  local files
  files=""
  for f in .jj-plan/[0-9][0-9]-*.md; do
    files="$files FILE:$(cat "$f")"
  done
  [[ "$files" == *"FILE:stack start"* ]]
  [[ "$files" == *"FILE:New step 1"* ]]
  [[ "$files" != *"FILE:old work"* ]]
}

@test "advancing bare stack bookmark shrinks the stack" {
  jj describe -m "Plan"
  local STEP1
  jj new; jj describe -m "Step 1"
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  local before
  before=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  # Advance stack to Step 1 — Plan drops out
  "$REAL_JJ" bookmark set stack -r "$STEP1" 2>/dev/null
  jj describe -m "Step 2 updated"
  local after
  after=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$before" -eq 3 ]]
  [[ "$after" -eq 2 ]]
  ! grep -q "Plan" .jj-plan/.stack
}

# =============================================================================
# Stack bookmark: named "stack/*"
# =============================================================================

@test "stack/named bookmark works as stack boundary" {
  # Remove the default stack bookmark and set up a custom one
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  "$REAL_JJ" describe -m "pre-work" 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "feature start" 2>/dev/null
  "$REAL_JJ" bookmark set stack/my-feature -r @ -B 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "Feature step 1"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "feature start" ]]
  [[ "$(cat .jj-plan/current.md)" == "Feature step 1" ]]
}

# =============================================================================
# Nearest ancestor resolution
# =============================================================================

@test "nearest stack/* ancestor wins when multiple exist" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  "$REAL_JJ" describe -m "phase 1 root" 2>/dev/null
  "$REAL_JJ" bookmark set stack/phase1 -r @ -B 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "phase 1 work" 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "phase 2 root" 2>/dev/null
  "$REAL_JJ" bookmark set stack/phase2 -r @ -B 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "phase 2 work"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "phase 2 root" ]]
  [[ "$(cat .jj-plan/current.md)" == "phase 2 work" ]]
}

@test "bare stack and stack/named coexist — nearest wins" {
  # Keep the bare stack bookmark from template, add a named one closer
  "$REAL_JJ" describe -m "old base" 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "named start" 2>/dev/null
  "$REAL_JJ" bookmark set stack/feature -r @ -B 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "Feature work"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "named start" ]]
}

@test "ambiguous sibling stack/* bookmarks produce an error" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  "$REAL_JJ" describe -m "root" 2>/dev/null
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Create two sibling branches with stack bookmarks
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "branch a" 2>/dev/null
  "$REAL_JJ" bookmark set stack/a -r @ -B 2>/dev/null
  local BA
  BA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  "$REAL_JJ" new -r "$ROOT" 2>/dev/null
  "$REAL_JJ" describe -m "branch b" 2>/dev/null
  "$REAL_JJ" bookmark set stack/b -r @ -B 2>/dev/null
  # Create a merge of both branches
  "$REAL_JJ" new -r "$BA" -r @  2>/dev/null
  jj describe -m "merge work"
  [[ -f .jj-plan/error.md ]]
  [[ "$(cat .jj-plan/error.md)" == *"Ambiguous stack"* ]]
}

@test "stack/* on a different branch does not affect stack" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  "$REAL_JJ" describe -m "root" 2>/dev/null
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Create a side branch and put stack/other there
  "$REAL_JJ" new 2>/dev/null
  "$REAL_JJ" describe -m "side branch" 2>/dev/null
  "$REAL_JJ" bookmark set stack/other -r @ -B 2>/dev/null
  # Go back to root, start a different line of work
  "$REAL_JJ" new -r "$ROOT" 2>/dev/null
  jj describe -m "Main line work"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ ! -f .jj-plan/current.md ]]
  [[ "$count" -eq 0 ]]
}

# =============================================================================
# trunk() fallback (exclusive)
# =============================================================================

@test "trunk() is used as fallback when no stack bookmark exists" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  # Create a remote so trunk() resolves
  local REMOTE
  REMOTE="$(mktemp -d)"
  git init --bare "$REMOTE" 2>/dev/null
  "$REAL_JJ" git remote add origin "$REMOTE" 2>/dev/null
  "$REAL_JJ" describe -m "initial" 2>/dev/null
  "$REAL_JJ" bookmark set main -r @ 2>/dev/null
  "$REAL_JJ" git push --bookmark main 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "Feature work"
  [[ -f .jj-plan/current.md ]]
  [[ "$(cat .jj-plan/current.md)" == "Feature work" ]]
}

@test "trunk() fallback is exclusive — trunk commit not in stack" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  local REMOTE
  REMOTE="$(mktemp -d)"
  git init --bare "$REMOTE" 2>/dev/null
  "$REAL_JJ" git remote add origin "$REMOTE" 2>/dev/null
  "$REAL_JJ" describe -m "trunk commit" 2>/dev/null
  "$REAL_JJ" bookmark set main -r @ 2>/dev/null
  "$REAL_JJ" git push --bookmark main 2>/dev/null
  "$REAL_JJ" new 2>/dev/null
  jj describe -m "My work"
  local files
  files=""
  for f in .jj-plan/[0-9][0-9]-*.md; do
    files="$files FILE:$(cat "$f")"
  done
  [[ "$files" == *"FILE:My work"* ]]
  [[ "$files" != *"FILE:trunk commit"* ]]
}

@test "no sync when neither stack bookmark nor useful trunk() exists" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  jj describe -m "Orphan work"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$count" -eq 0 ]]
  [[ ! -f .jj-plan/current.md ]]
}

# =============================================================================
# jj plan stack
# =============================================================================

@test "jj plan stack creates a change with bare stack bookmark" {
  jj describe -m "Old plan"
  run jj plan stack
  [[ "$output" == *"Started new stack: stack ("* ]]
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"stack:"* ]]
  local NEW_ID desc
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == "(plan: jj:$NEW_ID)"* ]]
}

@test "jj plan stack my-feature creates a change with stack/my-feature bookmark" {
  jj describe -m "Old plan"
  run jj plan stack my-feature
  [[ "$output" == *"Started new stack: stack/my-feature ("* ]]
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"stack/my-feature:"* ]]
}

@test "jj plan stack -r REV roots the new stack off the given revision" {
  jj describe -m "Base"
  local BASE
  BASE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Child"
  jj new; jj describe -m "Grandchild"
  run jj plan stack -r "$BASE"
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$("$REAL_JJ" log -r @- -T description --no-graph)" == "Base" ]]
}

@test "jj plan stack -r REV my-feature combines revision and name" {
  jj describe -m "Base"
  local BASE
  BASE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Child"
  run jj plan stack -r "$BASE" my-feature
  [[ "$output" == *"Started new stack: stack/my-feature ("* ]]
  [[ "$("$REAL_JJ" log -r @- -T description --no-graph)" == "Base" ]]
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"stack/my-feature:"* ]]
}

@test "jj plan stack flushes pending edits before creating the new stack" {
  jj describe -m "Original plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Revised plan with important details" > .jj-plan/current.md
  jj plan stack
  [[ "$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)" == "Revised plan with important details" ]]
}

@test "current.md is updated after jj plan stack" {
  jj describe -m "Old plan"
  jj plan stack
  local NEW_ID
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ -L .jj-plan/current.md ]]
  local link
  link=$(readlink .jj-plan/current.md)
  [[ "$link" == *"$NEW_ID"* ]]
}

@test "jj plan stack prints confirmation with change ID" {
  jj describe -m "Old plan"
  run jj plan stack
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$output" == *")"* ]]
}

@test "jj plan stack with invalid name fails cleanly and rolls back" {
  jj describe -m "Old plan"
  local BEFORE
  BEFORE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan stack "invalid name" 2>&1 || true
  local AFTER
  AFTER=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$BEFORE" == "$AFTER" ]]
}

@test "jj plan stack -r <ancestor> moves bare stack bookmark sideways with -B" {
  jj describe -m "Root"
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Middle"
  jj new; jj describe -m "Tip"
  run jj plan stack -r "$ROOT"
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$("$REAL_JJ" log -r @- -T description --no-graph)" == "Root" ]]
}

# =============================================================================
# jj plan new
# =============================================================================

@test "jj plan new creates a change with placeholder description" {
  jj describe -m "Existing plan"
  run jj plan new
  [[ "$output" == *"Created plan change: jj:"* ]]
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == "(plan: jj:"* ]]
}

@test "jj plan new placeholder contains actual change ID" {
  jj describe -m "Existing plan"
  jj plan new
  local NEW_ID desc
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == "(plan: jj:$NEW_ID)"* ]]
}

@test "jj plan new flushes pending edits" {
  jj describe -m "Original plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Revised plan with important details" > .jj-plan/current.md
  jj plan new
  [[ "$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)" == "Revised plan with important details" ]]
}

@test "jj plan new forwards -r flag" {
  jj describe -m "Base"
  local BASE
  BASE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Child"
  jj new; jj describe -m "Grandchild"
  run jj plan new -r "$BASE"
  [[ "$output" == *"Created plan change: jj:"* ]]
  [[ "$("$REAL_JJ" log -r @- -T description --no-graph)" == "Base" ]]
}

@test "jj plan new updates current.md and shows stack" {
  jj describe -m "Old plan"
  run jj plan new
  local NEW_ID
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ -L .jj-plan/current.md ]]
  local link
  link=$(readlink .jj-plan/current.md)
  [[ "$link" == *"$NEW_ID"* ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
}

@test "jj plan new current.md contains placeholder" {
  jj describe -m "Old plan"
  jj plan new
  [[ "$(cat .jj-plan/current.md)" == "(plan: jj:"* ]]
}

@test "jj plan new from mid-stack inserts linearly (not a fork)" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  # Move @ back to the middle
  jj edit -r @-
  jj plan new
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 4 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "Plan" ]]
  [[ "$(cat .jj-plan/02-*.md)" == "Step 1" ]]
  [[ "$(cat .jj-plan/03-*.md)" == "(plan: jj:"* ]]
  [[ "$(cat .jj-plan/04-*.md)" == "Step 2" ]]
}

# =============================================================================
# jj plan error handling
# =============================================================================

@test "jj plan with no subcommand shows usage" {
  jj describe -m "Plan"
  run jj plan
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"jj plan: missing subcommand"* ]]
  [[ "$output" == *"jj plan --help"* ]]
}

@test "jj plan bogus shows usage" {
  jj describe -m "Plan"
  run jj plan bogus
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"jj plan: unknown subcommand"* ]]
  [[ "$output" == *"jj plan --help"* ]]
}

# =============================================================================
# jj plan new --first / --last
# =============================================================================

@test "jj plan new --first inserts before first stack member" {
  jj describe -m "First"
  jj new; jj describe -m "Second"
  jj new; jj describe -m "Third"
  run jj plan new --first
  [[ "$output" == *"Created plan change: jj:"* ]]
  [[ "$(cat .jj-plan/01-*.md)" == "(plan: jj:"* ]]
  [[ "$(cat .jj-plan/02-*.md)" == "First" ]]
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 4 ]]
}

@test "jj plan new --first moves the stack bookmark" {
  jj describe -m "Root plan"
  jj new; jj describe -m "Step 1"
  jj plan new --first
  local NEW_ID bm_change
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  bm_change=$("$REAL_JJ" log -r 'bookmarks(exact:"stack")' -T "change_id.shortest(8)" --no-graph)
  [[ "$NEW_ID" == "$bm_change" ]]
}

@test "jj plan new --first moves stack bookmark when first member has multiple bookmarks" {
  jj describe -m "Root plan"
  "$REAL_JJ" bookmark set extra-bm -r @ 2>/dev/null
  jj new; jj describe -m "Step 1"
  jj plan new --first
  local NEW_ID bm_change
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  bm_change=$("$REAL_JJ" log -r 'bookmarks(exact:"stack")' -T "change_id.shortest(8)" --no-graph)
  [[ "$NEW_ID" == "$bm_change" ]]
  # extra-bm should stay on the original root, not follow
  local extra_bm_desc
  extra_bm_desc=$("$REAL_JJ" log -r 'bookmarks(exact:"extra-bm")' -T "description.first_line()" --no-graph)
  [[ "$extra_bm_desc" == "Root plan" ]]
}

@test "jj plan new --first sets placeholder description" {
  jj describe -m "Plan"
  jj plan new --first
  local NEW_ID desc
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == "(plan: jj:$NEW_ID)"* ]]
}

@test "jj plan new --last inserts after last stack member" {
  jj describe -m "First"
  local FIRST
  FIRST=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Second"
  jj new; jj describe -m "Third"
  # Move @ back to first so tip is not @
  jj edit -r "$FIRST"
  run jj plan new --last
  [[ "$output" == *"Created plan change: jj:"* ]]
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 4 ]]
  local last_file
  last_file=$(ls .jj-plan/[0-9][0-9]-*.md | sort | tail -1)
  [[ "$(cat "$last_file")" == "(plan: jj:"* ]]
}

@test "jj plan new --last sets placeholder description" {
  jj describe -m "Plan"
  jj plan new --last
  local NEW_ID desc
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == "(plan: jj:$NEW_ID)"* ]]
}

@test "jj plan new --first and --last together errors" {
  jj describe -m "Plan"
  run jj plan new --first --last
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"cannot specify both --first and --last"* ]]
}

@test "jj plan new --first errors when no stack resolved" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  run jj plan new --first
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"could not resolve stack"* ]]
}

# =============================================================================
# jj plan --help
# =============================================================================

@test "jj plan --help prints help" {
  run jj plan --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Subcommands:"* ]]
  [[ "$output" == *"--first"* ]]
  [[ "$output" == *"--last"* ]]
}

@test "jj plan -h prints help" {
  run jj plan -h
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Subcommands:"* ]]
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

@test "jj plan config shows real jj binary path" {
  run jj plan config
  [[ "$output" == *"real jj binary:"*"/jj"* ]]
  [[ "$output" == *"shim path:"* ]]
}

@test "jj plan config shows stack info" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  run jj plan config
  [[ "$output" == *"stack base:"*"(inclusive)"* ]]
  [[ "$output" == *"stack size:"*"2"* ]]
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

@test "jj plan config shows no stack when no bookmark or trunk" {
  "$REAL_JJ" bookmark delete stack 2>/dev/null
  run jj plan config
  [[ "$output" == *"stack base:"*"(none)"* ]]
  [[ "$output" == *"stack size:"*"0"* ]]
}

# =============================================================================
# Navigation commands show plan stack
# =============================================================================

@test "jj new appends plan stack when .jj-plan is active" {
  jj describe -m "Plan"
  jj new
  jj describe -m "Step 1"
  run jj new
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: Plan"* ]]
  [[ "$output" == *":: Step 1"* ]]
}

@test "jj edit appends plan stack when .jj-plan is active" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 1"
  jj new; jj describe -m "Step 2"
  run jj edit -r "$PLAN"
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"*   01-"*":: Plan"* ]]
  [[ "$output" == *":: Step 1"* ]]
  [[ "$output" == *":: Step 2"* ]]
}

@test "jj plan stack appends plan stack after confirmation" {
  jj describe -m "Old plan"
  run jj plan stack my-feature
  [[ "$output" == *"Started new stack: stack/my-feature ("* ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"*   01-"* ]]
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

@test "change that is both current AND done shows * ✓" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 1"
  # Mark Plan as done
  printf "Plan\n\nplan-status: ✅" > ".jj-plan/01-${PLAN}.md"
  # Switch back to Plan — it is both current AND done
  jj edit -r "$PLAN"
  [[ "$(grep '01-' .jj-plan/.stack)" == "* ✓ 01-"*":: Plan"* ]]
}

@test "plan-status: ✅ detected when not on the last line" {
  jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  # Write plan-status in the middle, with trailing content after it
  printf "Step 1\n\nplan-status: ✅\n\n## Notes\nSome trailing content" > ".jj-plan/01-${STEP1}.md"
  jj describe -m "Step 2 updated"
  [[ "$(grep '01-' .jj-plan/.stack)" == "  ✓ 01-"* ]]
}

# =============================================================================
# Stack bookmark protection on abandon
# =============================================================================

@test "abandon stack-bookmarked change with descendants moves bookmark to first child" {
  jj describe -m "Stack root"
  local ROOT CHILD
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Child"
  CHILD=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Grandchild"
  run jj abandon "$ROOT"
  [[ "$output" == *"moved stack bookmark stack to"* ]]
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"stack:"* ]]
}

@test "abandon stack-bookmarked @ with no descendants moves bookmark to new @" {
  jj describe -m "Sole member"
  run jj abandon
  [[ "$output" == *"moved stack bookmark stack to"* ]]
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"stack:"* ]]
}

@test "abandon non-bookmarked middle change does not interfere with bookmark" {
  jj describe -m "Plan"
  jj new; jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Step 2"
  run jj abandon "$STEP1"
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"stack:"* ]]
  [[ "$output" != *"moved stack bookmark"* ]]
  [[ "$output" != *"WARNING"* ]]
}

@test "abandon with --retain-bookmarks does not trigger shim recovery" {
  jj describe -m "Stack root"
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Child"
  run jj abandon "$ROOT" --retain-bookmarks
  [[ "$output" != *"moved stack bookmark"* ]]
}

@test "general bookmark loss detection warns on jj bookmark delete" {
  jj describe -m "Plan"
  run jj bookmark delete stack
  [[ "$output" == *"WARNING: stack bookmark was lost"* ]]
}

@test ".jj-plan is correctly synced after bookmark recovery on abandon" {
  jj describe -m "Stack root"
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Child"
  jj new; jj describe -m "Grandchild"
  jj abandon "$ROOT"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "Child" ]]
  [[ "$(cat .jj-plan/current.md)" == "Grandchild" ]]
}

# =============================================================================
# Legacy .jj-plans fallback
# =============================================================================

@test "legacy .jj-plans/ works when .jj-plan/ does not exist" {
  rm -rf .jj-plan
  mkdir -p .jj-plans
  jj describe -m "Legacy plan"
  [[ -f .jj-plans/current.md ]]
  [[ "$(cat .jj-plans/current.md)" == "Legacy plan" ]]
  run jj status
  [[ "$output" == *"Plan stack (.jj-plans/;"* ]]
}

@test ".jj-plan/ takes precedence when both .jj-plan/ and .jj-plans/ exist" {
  mkdir -p .jj-plans
  jj describe -m "Precedence test"
  [[ -f .jj-plan/current.md ]]
  local legacy_count
  legacy_count=$(ls .jj-plans/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$legacy_count" -eq 0 ]]
  run jj status
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
}

@test "JJ_PLAN_DIR env var overrides both .jj-plan/ and .jj-plans/" {
  mkdir -p .jj-plans .custom-plans
  export JJ_PLAN_DIR="$(pwd)/.custom-plans"
  jj describe -m "Custom dir test"
  [[ -f .custom-plans/current.md ]]
  local default_count legacy_count
  default_count=$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  legacy_count=$(ls .jj-plans/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$default_count" -eq 0 ]]
  [[ "$legacy_count" -eq 0 ]]
  [[ "$(cat .custom-plans/current.md)" == "Custom dir test" ]]
  run jj status
  [[ "$output" == *"Plan stack (.custom-plans/;"* ]]
}

# =============================================================================
# jj plan done
# =============================================================================

@test "jj plan done marks current plan as done" {
  jj describe -m "My plan"
  jj plan done
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"plan-status: ✅"* ]]
}

@test "jj plan done advances to next undone plan" {
  jj describe -m "Plan 1"
  local P1 P2
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj edit -r "$P1"
  jj plan done
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P2" ]]
}

@test "jj plan done wraps around to earlier undone plan when at end of stack" {
  jj describe -m "Plan 1"
  local P1 P2 P3
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 3"
  P3=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Mark plan 2 done (middle), leave plan 1 undone
  jj plan done "$P2"
  # Now at plan 3 (last); mark it done — should wraparound to plan 1
  jj edit -r "$P3"
  jj plan done
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
}

@test "jj plan done --dry-run does not modify description" {
  jj describe -m "My plan

## Scratch [scratch]

Working notes here"
  run jj plan done --dry-run
  [[ "$output" == *"Would strip scratch sections"* ]]
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"My plan"* ]]
  [[ "$desc" == *"Working notes here"* ]]
}

@test "jj plan done --keep-scratch preserves scratch content" {
  jj describe -m "My plan

## Notes [scratch]

Important scratch notes"
  jj plan done --keep-scratch
  local desc
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  [[ "$desc" == *"Important scratch notes"* ]]
  [[ "$desc" == *"plan-status: ✅"* ]]
}

@test "jj plan done strips scratch sections" {
  jj describe -m "My plan

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
  [[ "$desc" == *"plan-status: ✅"* ]]
}

@test "jj plan done --stack marks all plans done" {
  jj describe -m "Plan 1"
  jj new; jj describe -m "Plan 2"
  jj new; jj describe -m "Plan 3"
  jj plan done --stack
  local descs
  descs=$("$REAL_JJ" log -r "@ | @- | @--" -T "description" --no-graph)
  [[ "$descs" == *"plan-status: ✅"* ]]
}

@test "jj plan done on already-done plan is idempotent" {
  jj describe -m "My plan

plan-status: ✅"
  jj plan done
  local desc count
  desc=$("$REAL_JJ" log -r @ -T description --no-graph)
  count=$(echo "$desc" | grep -c "plan-status: ✅")
  [[ "$count" -eq 1 ]]
}

# =============================================================================
# jj plan next / prev
# =============================================================================

@test "jj plan next advances from plan 1 to plan 2" {
  jj describe -m "Plan 1"
  local P1 P2
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj edit -r "$P1"
  run jj plan next
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P2" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan prev moves from plan 2 to plan 1" {
  jj describe -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  run jj plan prev
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan next at last plan stays put" {
  jj describe -m "Plan 1"
  jj new; jj describe -m "Plan 2"
  run jj plan next
  [[ "$output" == *"Already at the last plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Plan 2" ]]
}

@test "jj plan prev at first plan stays put" {
  jj describe -m "Plan 1"
  run jj plan prev
  [[ "$output" == *"Already at the first plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Plan 1" ]]
}

@test "jj plan next flushes pending edits before moving" {
  jj describe -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  jj edit -r "$P1"
  printf "Edited plan 1 content" > .jj-plan/current.md
  jj plan next
  [[ "$("$REAL_JJ" log -r "$P1" -T description --no-graph)" == "Edited plan 1 content" ]]
}

# =============================================================================
# jj plan go
# =============================================================================

@test "jj plan go 2 moves to the second plan" {
  jj describe -m "Plan 1"
  jj new; jj describe -m "Plan 2"
  local P2
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 3"
  run jj plan go 2
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P2" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan go CHANGE_ID moves to specified change" {
  jj describe -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  jj new; jj describe -m "Plan 3"
  run jj plan go "$P1"
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
}

@test "jj plan go 0 errors" {
  jj describe -m "Plan 1"
  run jj plan go 0
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"out of range"* ]]
}

@test "jj plan go 99 errors (out of range)" {
  jj describe -m "Plan 1"
  run jj plan go 99
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"out of range"* ]]
}

# =============================================================================
# Plan templates
# =============================================================================

@test "custom template.md overrides default template" {
  printf "Custom: {{CHANGE_ID}}\n\n## My Section\n" > .jj-plan/template.md
  jj plan new
  local content
  content=$(cat .jj-plan/current.md)
  [[ "$content" == *"Custom: "* ]]
  [[ "$content" == *"## My Section"* ]]
}

@test "JJ_PLAN_TEMPLATE env var overrides template.md" {
  printf "ENV template: {{CHANGE_ID}}\n" > .jj-plan/template.md
  local ENVFILE
  ENVFILE="$(mktemp)"
  printf "Env override: {{CHANGE_ID}}\n\n## Env Section\n" > "$ENVFILE"
  export JJ_PLAN_TEMPLATE="$ENVFILE"
  jj plan new
  local content
  content=$(cat .jj-plan/current.md)
  [[ "$content" == *"Env override: "* ]]
  [[ "$content" == *"## Env Section"* ]]
  [[ "$content" != *"ENV template"* ]]
}

@test "template CHANGE_ID is interpolated correctly" {
  jj plan new
  local NEW_ID content
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  content=$(cat .jj-plan/current.md)
  [[ "$content" == "(plan: jj:$NEW_ID)"* ]]
}

@test "custom template without CHANGE_ID gets self-reference injected" {
  printf "No placeholder here\n\n## Section\n" > .jj-plan/template.md
  jj plan new
  local NEW_ID content
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  content=$(cat .jj-plan/current.md)
  [[ "$content" == *"jj:$NEW_ID"* ]]
}

# =============================================================================
# jj describe interception
# =============================================================================

@test "jj describe -m writes to plan file first" {
  jj describe -m "Initial"
  jj describe -m "Updated via describe"
  [[ "$(cat .jj-plan/current.md)" == "Updated via describe" ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Updated via describe" ]]
}

@test "jj describe -m on non-current change updates correct plan file" {
  jj describe -m "Plan 1"
  local P1
  P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj new; jj describe -m "Plan 2"
  jj describe -r "$P1" -m "Plan 1 updated"
  [[ "$(cat .jj-plan/01-*.md)" == "Plan 1 updated" ]]
}

# =============================================================================
# jj plan go missing target
# =============================================================================

@test "jj plan go without target shows error" {
  jj describe -m "Plan"
  run jj plan go
  [[ "$status" -eq 1 ]]
  [[ "$output" == *"missing target"* ]]
}