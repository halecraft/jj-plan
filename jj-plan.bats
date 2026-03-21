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
  # Capture the real change ID for the registry entry
  local TMPL_CID
  TMPL_CID=$("$REAL_JJ" -R "$TEMPLATE_REPO" log -r @ -T 'change_id' --no-graph)
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
# Stack building (using jj plan new <bookmark> for new steps)
# =============================================================================

@test "jj plan new <bookmark> creates a new plan file and updates current.md" {
  jj describe -m "Plan"
  jj plan new step-1
  jj describe -m "Step 1"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 2 ]]
  [[ "$(cat .jj-plan/current.md)" == "Step 1" ]]
}

@test "three-change stack produces three numbered files in order" {
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  [[ "$(cat .jj-plan/01-*.md)" == "Plan" ]]
  [[ "$(cat .jj-plan/02-*.md)" == "Step 1" ]]
  [[ "$(cat .jj-plan/03-*.md)" == "Step 2" ]]
}

@test "sort order is bottom-endian: 01 is closest to start bookmark" {
  jj describe -m "Stack-root"
  jj plan new step-1; jj describe -m "Middle"
  jj plan new step-2; jj describe -m "Tip"
  [[ "$(cat .jj-plan/01-*.md)" == "Stack-root" ]]
}

# =============================================================================
# Inclusive model: bookmark is first member
# =============================================================================

@test "start bookmark change is included in .stack as first member" {
  jj describe -m "I am the start bookmark"
  [[ "$(cat .jj-plan/.stack)" == *"01-"*":: I am the start bookmark"* ]]
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
  jj plan new step-1; jj describe -m "Impl"
  [[ "$(readlink .jj-plan/current.md)" == "02-"* ]]
  jj edit -r "$PLAN"
  [[ "$(readlink .jj-plan/current.md)" == "01-"* ]]
}

@test "all stack files remain visible when editing a middle change" {
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Step 2"
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
  jj plan new step-1; jj describe -m "Impl"
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
  jj plan new step-1; jj describe -m "Step 1"
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Step 2"
  # Edit the Plan file (not current) with rich content
  printf "Plan\n\n## Background\nDetailed context here" > ".jj-plan/01-start.md"
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
  jj plan new step-1; jj describe -m "phase 2 placeholder"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Phase 3"
  # Write rich plan to Phase 2 (not current)
  printf "Phase 2: Full implementation plan\n\n## Steps\n- Do X\n- Do Y\n- Do Z" > ".jj-plan/02-step-1.md"
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
  jj plan new step-1; jj describe -m "Change B"
  CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Change C"
  # Edit both A and B (neither is current)
  printf "Change A revised with detail" > ".jj-plan/01-start.md"
  printf "Change B revised with detail" > ".jj-plan/02-step-1.md"
  # Trigger sync
  jj describe -m "Change C updated"
  [[ "$("$REAL_JJ" log -r "$CA" -T description --no-graph)" == "Change A revised with detail" ]]
  [[ "$("$REAL_JJ" log -r "$CB" -T description --no-graph)" == "Change B revised with detail" ]]
}

@test "non-current file edits survive stack renumbering" {
  jj describe -m "Will be abandoned"
  local DOOMED KEEP
  DOOMED=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jj describe -m "Important plan"
  KEEP=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Current work"
  # Edit the non-current plan file (index 02)
  printf "Important plan\n\n## Revised\nWith critical details" > ".jj-plan/02-step-1.md"
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
  for f in .jj-plan/[0-9][0-9]-*.md; do
    if [[ -f "$f" ]] && grep -q "critical details" "$f"; then
      found=true; break
    fi
  done
  [[ "$found" == "true" ]]
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
  jj plan new step-1; jj describe -m "Impl"
  # Edit non-current (Plan) file
  printf "Plan\n\n## Updated background" > ".jj-plan/01-start.md"
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
  jj plan new step-1; jj describe -m "phase 2 placeholder"
  local P2
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "phase 3 placeholder"
  jj plan new step-3; jj describe -m "phase 4 placeholder"
  # Write rich plan to phase 2 (NOT current — current is phase 4)
  # Plan files use bookmark names, not change IDs: step-1 is the bookmark for P2
  printf "Phase 2: Implement branded InterpreterLayer\n\n## Background\nThis is the detailed plan that must not be lost.\n\n## Steps\n- Step A: extract trait\n- Step B: implement layer\n- Step C: wire up" > ".jj-plan/02-step-1.md"
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

@test ".stack file is generated with first lines of plan files" {
  jj describe -m "Refactor auth middleware"
  jj plan new step-1; jj describe -m "Extract auth module"
  jj plan new step-2; jj describe -m "Implement JWT strategy"
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
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
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
  jj plan new step-1; jj describe -m "Step 1"
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
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  [[ "$(grep '01-' .jj-plan/.stack)" == "    01-"* ]]
}

@test ".stack shows ~ for non-empty non-current changes" {
  jj describe -m "Step 1"
  echo "some work" > file.txt
  jj plan new step-1; jj describe -m "Step 2"
  [[ "$(grep '01-' .jj-plan/.stack)" == "  ~ 01-"* ]]
}

@test ".stack shows ✓ for changes with plan-status: ✅ in description" {
  jj describe -m "Step 1"
  jj plan new step-1; jj describe -m "Step 2"
  # Mark Step 1 as done by editing its plan file (bookmark-named: 01-start.md)
  printf "Step 1\n\nDid the work.\n\nplan-status: ✅" > ".jj-plan/01-start.md"
  # Trigger a sync
  jj describe -m "Step 2 updated"
  [[ "$(grep '01-' .jj-plan/.stack)" == "  ✓ 01-"* ]]
}

@test ".stack shows all four status types together" {
  # Change 0: will be marked done
  jj describe -m "Done change"
  # Change 1: will have file changes (has-changes)
  jj plan new step-1; jj describe -m "Has changes"
  echo "work" > file.txt
  # Change 2: will be current (in-progress)
  jj plan new step-2; jj describe -m "Current work"
  # Change 3: empty, not started
  jj plan new step-3; jj describe -m "Future work"
  # Now go back to change 2 to make it current
  jj edit -r @-
  # Mark change 0 as done (bookmark-named: 01-start.md)
  printf "Done change\n\nplan-status: ✅" > ".jj-plan/01-start.md"
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
  local START_CID
  START_CID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  # Write done status to plan file
  printf "Step 1\n\nCompleted.\n\nplan-status: ✅" > .jj-plan/current.md
  # Switch away (flushes to jj)
  jj plan new step-1; jj describe -m "Step 2"
  # Check the description was preserved
  local desc
  desc=$("$REAL_JJ" log -r "$START_CID" -T description --no-graph)
  [[ "$desc" == *"Step 1"* ]]
  [[ "$desc" == *"plan-status: ✅"* ]]
}

@test "jj status flushes non-current file edits and updates .stack" {
  jj describe -m "Phase 1"
  local P2
  jj plan new step-1; jj describe -m "phase 2 placeholder"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Phase 3"
  # Write rich plan to Phase 2 (not current) WITHOUT running a jj command
  # Plan files use bookmark names, not change IDs: step-1 is bookmark for P2
  printf "Phase 2: Full implementation plan\n\nDetailed steps here" > ".jj-plan/02-step-1.md"
  # jj status should flush the edit and show updated .stack
  run jj status
  [[ "$output" == *":: Phase 2: Full implementation plan"* ]]
  [[ "$("$REAL_JJ" log -r "$P2" -T description --no-graph)" == *"Phase 2: Full implementation plan"* ]]
}

@test "jj st flushes edits to multiple non-current files" {
  jj describe -m "Change A"
  local CA CB
  CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jj describe -m "Change B"
  CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Change C"
  # Edit both non-current files (bookmark-named: start, step-1)
  printf "Change A: revised plan" > ".jj-plan/01-start.md"
  printf "Change B: revised plan" > ".jj-plan/02-step-1.md"
  # jj st should flush both
  run jj st
  [[ "$output" == *":: Change A: revised plan"* ]]
  [[ "$output" == *":: Change B: revised plan"* ]]
  [[ "$("$REAL_JJ" log -r "$CA" -T description --no-graph)" == "Change A: revised plan" ]]
  [[ "$("$REAL_JJ" log -r "$CB" -T description --no-graph)" == "Change B: revised plan" ]]
}

# =============================================================================
# Cleanup
# =============================================================================

@test "files for abandoned changes are removed" {
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Step 2"
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
  jj plan new step-1; jj describe -m "Extract module"
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
  jj plan new step-1; jj describe -m "Extract module"
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
  jj plan new step-1; jj describe -m "Step 1"
  mkdir -p src
  cd src
  jj plan new step-2; jj describe -m "Step 2"
  local count
  count=$(ls ../.jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 3 ]]
  [[ "$(cat ../.jj-plan/current.md)" == "Step 2" ]]
}

@test "editing current.md from subdir flushes to jj on switch" {
  jj describe -m "Original"
  local PLAN IMPL
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jj describe -m "Impl"
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
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  jj plan new step-3; jj describe -m "Step 3"
  [[ -f .jj-plan/error.md ]]
  [[ "$(readlink .jj-plan/current.md)" == "error.md" ]]
}

@test "error.md contains a descriptive message" {
  export JJ_PLAN_MAX=3
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  jj plan new step-3; jj describe -m "Step 3"
  local msg
  msg=$(cat .jj-plan/error.md)
  [[ "$msg" == *"max 3"* ]]
  [[ "$msg" == *"Refusing to sync"* ]]
}

@test "error state self-heals when stack shrinks below max" {
  export JJ_PLAN_MAX=3
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  jj plan new step-3; jj describe -m "Step 3"
  [[ -f .jj-plan/error.md ]]
  jj squash -m "Step 2+3 combined"
  jj edit -r @-
  [[ ! -f .jj-plan/error.md ]]
}

@test "flush is skipped during error state (no description clobber)" {
  export JJ_PLAN_MAX=3
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  jj plan new step-3; jj describe -m "Step 3"
  jj describe -m "Step 3 updated"
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Step 3 updated" ]]
}

# =============================================================================
# Edge cases
# =============================================================================

@test "jj plan new produces empty plan file before describe" {
  jj describe -m "Plan"
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
  jj describe -m "Orphan work"
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d " ")
  [[ "$count" -eq 0 ]]
  [[ ! -f .jj-plan/current.md ]]
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
  jj describe -m "Base"
  BASE=$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)
  jj plan new step-1
  jj describe -m "After"
  # Create plan rooted off base
  run jj plan new rooted-plan -r "$BASE"
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Created plan: rooted-plan"* ]]
}

@test "jj plan new flushes pending edits" {
  jj describe -m "Original plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  printf "Revised plan with important details" > .jj-plan/current.md
  jj plan new step-next
  [[ "$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)" == "Revised plan with important details" ]]
}

@test "jj plan new updates current.md and shows stack" {
  jj describe -m "Old plan"
  run jj plan new step-next
  local NEW_ID
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ -L .jj-plan/current.md ]]
  local link
  link=$(readlink .jj-plan/current.md)
  [[ "$link" == *"step-next"* ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
}

@test "jj plan new current.md contains placeholder" {
  jj describe -m "Old plan"
  jj plan new step-next
  [[ "$(cat .jj-plan/current.md)" == "(plan: jj:"* ]]
}

@test "jj plan new from mid-stack inserts linearly (not a fork)" {
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  # Move @ back to the middle
  jj edit -r @-
  jj plan new step-mid
  local count
  count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
  [[ "$count" -eq 4 ]]
  [[ "$(cat .jj-plan/01-*.md)" == "Plan" ]]
  [[ "$(cat .jj-plan/02-*.md)" == "Step 1" ]]
  [[ "$(cat .jj-plan/03-*.md)" == "(plan: jj:"* ]]
  [[ "$(cat .jj-plan/04-*.md)" == "Step 2" ]]
}

@test "jj plan new placeholder contains actual change ID" {
  jj describe -m "Existing plan"
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
  [[ -f .jj-plan/02-feat--auth.md ]]
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
  jj describe -m "My plan"
  run jj stack
  [[ "$status" -eq 0 ]]
  # Verify the visualization includes the bookmark name and a change ID
  [[ "$output" == *"start "* ]]
  # Change ID is the short reverse-hex (k-z alphabet, 8+ chars) between bookmark and indicator
  [[ "$output" =~ start\ [k-z]{8} ]]
  [[ "$output" == *"trunk()"* ]]
}

@test "jj stack submit without remote fails gracefully" {
  run jj stack submit
  [[ "$status" -ne 0 ]]
  [[ "$output" == *"no supported remotes"* ]] || [[ "$output" == *"authentication"* ]] || [[ "$output" == *"remote"* ]]
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
  jj describe -m "Precious content"
  run jj plan new --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan track --help prints help without side effects" {
  jj describe -m "Precious content"
  run jj plan track --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan untrack --help prints help without side effects" {
  jj describe -m "Precious content"
  run jj plan untrack --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan done --help prints help without side effects" {
  jj describe -m "Precious content"
  run jj plan done --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan go --help prints help without side effects" {
  jj describe -m "Precious content"
  run jj plan go --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan next --help prints help without side effects" {
  jj describe -m "Precious content"
  run jj plan next --help
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj plan"* ]]
  [[ "$("$REAL_JJ" log -r @ -T description --no-graph)" == "Precious content" ]]
}

@test "jj plan prev --help prints help without side effects" {
  jj describe -m "Precious content"
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
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
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
  jj describe -m "Plan"
  jj plan new step-1
  jj describe -m "Step 1"
  run jj plan new step-2
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: Plan"* ]]
  [[ "$output" == *":: Step 1"* ]]
}

@test "jj edit appends plan stack when .jj-plan is active" {
  jj describe -m "Plan"
  local PLAN
  PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-1; jj describe -m "Step 1"
  jj plan new step-2; jj describe -m "Step 2"
  run jj edit -r "$PLAN"
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"*   01-"*":: Plan"* ]]
  [[ "$output" == *":: Step 1"* ]]
  [[ "$output" == *":: Step 2"* ]]
}

@test "jj plan new appends plan stack after confirmation" {
  jj describe -m "Old plan"
  run jj plan new my-feature
  [[ "$output" == *"Created plan: my-feature"* ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"*   "* ]]
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
  jj plan new step-1; jj describe -m "Step 1"
  # Mark Plan as done (bookmark-named: 01-start.md)
  printf "Plan\n\nplan-status: ✅" > ".jj-plan/01-start.md"
  # Switch back to Plan — it is both current AND done
  jj edit -r "$PLAN"
  [[ "$(grep '01-' .jj-plan/.stack)" == "* ✓ 01-"*":: Plan"* ]]
}

@test "plan-status: ✅ detected when not on the last line" {
  jj describe -m "Step 1"
  jj plan new step-1; jj describe -m "Step 2"
  # Write plan-status in the middle, with trailing content after it (bookmark-named: 01-start.md)
  printf "Step 1\n\nplan-status: ✅\n\n## Notes\nSome trailing content" > ".jj-plan/01-start.md"
  jj describe -m "Step 2 updated"
  [[ "$(grep '01-' .jj-plan/.stack)" == "  ✓ 01-"* ]]
}

# =============================================================================
# Bookmark protection on abandon
# =============================================================================

@test "abandon bookmarked change with descendants succeeds and resyncs" {
  jj describe -m "Start root"
  local ROOT CHILD
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new child-step; jj describe -m "Child"
  CHILD=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new grandchild-step; jj describe -m "Grandchild"
  run jj abandon "$ROOT"
  # jj deletes the bookmark on abandon (not move). Verify the abandon succeeded
  # and plan files re-synced. The companion test ".jj-plan is correctly synced
  # after bookmark recovery on abandon" validates the file contents.
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Abandoned"* ]]
}

@test "abandon non-bookmarked middle change does not interfere with bookmark" {
  jj describe -m "Plan"
  jj plan new step-1; jj describe -m "Step 1"
  local STEP1
  STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Step 2"
  run jj abandon "$STEP1"
  local bm
  bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
  [[ "$bm" == *"start:"* ]]
  [[ "$output" != *"WARNING"* ]]
}

@test ".jj-plan is correctly synced after bookmark recovery on abandon" {
  jj describe -m "Start root"
  local ROOT
  ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new child-step; jj describe -m "Child"
  jj plan new grandchild-step; jj describe -m "Grandchild"
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
  jj plan new step-1; jj describe -m "Plan 2"
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
  jj plan new step-1; jj describe -m "Plan 2"
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Plan 3"
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
  jj plan new step-1; jj describe -m "Plan 2"
  jj plan new step-2; jj describe -m "Plan 3"
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
  jj plan new step-1; jj describe -m "Plan 2"
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
  jj plan new step-1; jj describe -m "Plan 2"
  run jj plan prev
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
  [[ "$output" == *"Plan stack"* ]]
}

@test "jj plan next at last plan stays put" {
  jj describe -m "Plan 1"
  jj plan new step-1; jj describe -m "Plan 2"
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
  jj plan new step-1; jj describe -m "Plan 2"
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
  jj plan new step-1; jj describe -m "Plan 2"
  local P2
  P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  jj plan new step-2; jj describe -m "Plan 3"
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
  jj plan new step-1; jj describe -m "Plan 2"
  jj plan new step-2; jj describe -m "Plan 3"
  run jj plan go "$P1"
  local CUR
  CUR=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  [[ "$CUR" == "$P1" ]]
}

@test "jj plan go accepts bookmark name" {
  jj describe -m "Plan 1"
  jj plan new my-step; jj describe -m "Plan 2"
  jj plan new other-step; jj describe -m "Plan 3"
  run jj plan go my-step
  [[ "$status" -eq 0 ]]
  [[ "$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)" == "Plan 2" ]]
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

@test "jj plan go without target shows error" {
  jj describe -m "Plan"
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
  jj plan new tmpl-step
  local content
  content=$(cat .jj-plan/current.md)
  [[ "$content" == *"Env override: "* ]]
  [[ "$content" == *"## Env Section"* ]]
  [[ "$content" != *"ENV template"* ]]
}

@test "template CHANGE_ID is interpolated correctly" {
  jj plan new tmpl-step
  local NEW_ID content
  NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
  content=$(cat .jj-plan/current.md)
  [[ "$content" == "(plan: jj:$NEW_ID)"* ]]
}

@test "custom template without CHANGE_ID gets self-reference injected" {
  printf "No placeholder here\n\n## Section\n" > .jj-plan/template.md
  jj plan new tmpl-step
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
  jj plan new step-1; jj describe -m "Plan 2"
  jj describe -r "$P1" -m "Plan 1 updated"
  [[ "$(cat .jj-plan/01-*.md)" == "Plan 1 updated" ]]
}