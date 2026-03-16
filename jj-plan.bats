#!/usr/bin/env bats

# Tests for jj-plan
# Run: bats jj-plan.bats
#
# Requires ~/.local/bin/jj shim to be in PATH ahead of the real jj binary.

REAL_JJ="/opt/homebrew/bin/jj"
SHIM_JJ="$HOME/.local/bin/jj"

# Helper: run a zsh script in a fresh jj repo with .jj-plan/ activated.
# Sets a "stack" bookmark on the initial commit — that commit IS the first
# stack member (inclusive model).
run_in_repo() {
  local script="$1"
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    mkdir -p .jj-plan
    $script
  "
}

# Helper: same but with a custom JJ_PLAN_MAX
run_in_repo_with_max() {
  local max="$1"
  local script="$2"
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    export JJ_PLAN_MAX=$max
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    mkdir -p .jj-plan
    $script
  "
}

# --- Basic sync ---

@test "describe creates plan file in .jj-plan" {
  run_in_repo '
    jj describe -m "My plan"
    count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=1"* ]]
}

@test "plan file contains the description" {
  run_in_repo '
    jj describe -m "My detailed plan"
    echo "CONTENT:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"CONTENT:My detailed plan"* ]]
}

@test "current.md is a symlink to the active change" {
  run_in_repo '
    jj describe -m "Plan"
    [[ -L .jj-plan/current.md ]] && echo "RESULT:is_symlink" || echo "RESULT:not_symlink"
  '
  [[ "$output" == *"RESULT:is_symlink"* ]]
}

# --- Stack building ---

@test "jj new creates a new plan file and updates current.md" {
  run_in_repo '
    jj describe -m "Plan"
    jj new
    jj describe -m "Step 1"
    count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
    echo "CONTENT:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=2"* ]]
  [[ "$output" == *"CONTENT:Step 1"* ]]
}

@test "three-change stack produces three numbered files in order" {
  run_in_repo '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    for f in .jj-plan/[0-9][0-9]-*.md; do
      echo "FILE:$(basename $f):$(cat $f)"
    done
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"FILE:01-"*":Plan"* ]]
  [[ "$output" == *"FILE:02-"*":Step 1"* ]]
  [[ "$output" == *"FILE:03-"*":Step 2"* ]]
}

@test "sort order is bottom-endian: 01 is closest to stack bookmark" {
  run_in_repo '
    jj describe -m "Stack-root"
    jj new; jj describe -m "Middle"
    jj new; jj describe -m "Tip"
    echo "FIRST:$(cat .jj-plan/01-*.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"FIRST:Stack-root"* ]]
}

# --- Inclusive model: bookmark is first member ---

@test "stack bookmark change is included in .stack as first member" {
  run_in_repo '
    jj describe -m "I am the stack bookmark"
    cat .jj-plan/.stack
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"01-"*":: I am the stack bookmark"* ]]
}

@test "single-change stack (@ is the bookmark) shows one entry" {
  run_in_repo '
    jj describe -m "Solo change"
    count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
    echo "CURRENT:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=1"* ]]
  [[ "$output" == *"CURRENT:Solo change"* ]]
}

# --- Switching changes ---

@test "jj edit switches current.md symlink" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Impl"
    echo "BEFORE:$(readlink .jj-plan/current.md)"
    jj edit -r "$PLAN"
    echo "AFTER:$(readlink .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"BEFORE:02-"* ]]
  [[ "$output" == *"AFTER:01-"* ]]
}

@test "all stack files remain visible when editing a middle change" {
  run_in_repo '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    jj edit -r "$STEP1"
    count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=3"* ]]
}

# --- Editing plan files ---

@test "editing current.md flushes to jj description on switch" {
  run_in_repo '
    jj describe -m "Original plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Impl"
    IMPL=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    printf "Updated impl description" > .jj-plan/current.md
    jj edit -r "$PLAN"
    echo "DESC:$("$REAL_JJ" log -r "$IMPL" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Updated impl description"* ]]
}

# --- Non-current file flush (data loss prevention) ---

@test "editing a non-current plan file is flushed to jj on next command" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    # Edit the Plan file (not current) with rich content
    printf "Plan\n\n## Background\nDetailed context here" > ".jj-plan/01-${PLAN}.md"
    # Trigger a sync with any mutating command
    jj describe -m "Step 2 updated"
    echo "DESC:$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Plan"* ]]
  [[ "$output" == *"## Background"* ]]
  [[ "$output" == *"Detailed context here"* ]]
}

@test "editing a non-current plan file survives jj edit to another change" {
  run_in_repo '
    jj describe -m "Phase 1"
    P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "phase 2 placeholder"
    P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Phase 3"
    # Write rich plan to Phase 2 (not current)
    printf "Phase 2: Full implementation plan\n\n## Steps\n- Do X\n- Do Y\n- Do Z" > ".jj-plan/02-${P2}.md"
    # Switch to Phase 2
    jj edit -r "$P2"
    echo "CURRENT:$(cat .jj-plan/current.md)"
    echo "JJ_DESC:$("$REAL_JJ" log -r "$P2" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"CURRENT:Phase 2: Full implementation plan"* ]]
  [[ "$output" == *"- Do X"* ]]
  [[ "$output" == *"JJ_DESC:Phase 2: Full implementation plan"* ]]
}

@test "editing multiple non-current plan files flushes all of them" {
  run_in_repo '
    jj describe -m "Change A"
    CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Change B"
    CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Change C"
    # Edit both A and B (neither is current)
    printf "Change A revised with detail" > ".jj-plan/01-${CA}.md"
    printf "Change B revised with detail" > ".jj-plan/02-${CB}.md"
    # Trigger sync
    jj describe -m "Change C updated"
    echo "A_DESC:$("$REAL_JJ" log -r "$CA" -T description --no-graph)"
    echo "B_DESC:$("$REAL_JJ" log -r "$CB" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"A_DESC:Change A revised with detail"* ]]
  [[ "$output" == *"B_DESC:Change B revised with detail"* ]]
}

@test "non-current file edits survive stack renumbering" {
  run_in_repo '
    jj describe -m "Will be abandoned"
    DOOMED=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Important plan"
    KEEP=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Current work"
    # Edit the Important plan file (index 02)
    printf "Important plan\n\n## Revised\nWith critical details" > ".jj-plan/02-${KEEP}.md"
    # Abandon the first change — causes renumbering (KEEP goes from 02 to 01)
    # Also move stack bookmark to KEEP since the old root is gone
    "$REAL_JJ" bookmark set stack -r "$KEEP" 2>/dev/null
    jj abandon "$DOOMED"
    # Check if the edited content survived renumbering
    echo "DESC:$("$REAL_JJ" log -r "$KEEP" -T description --no-graph)"
    echo "FILE:$(cat ".jj-plan/01-${KEEP}.md")"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Important plan"* ]]
  [[ "$output" == *"## Revised"* ]]
  [[ "$output" == *"critical details"* ]]
  [[ "$output" == *"FILE:Important plan"* ]]
}

@test "jj describe does not get clobbered by stale file content" {
  run_in_repo '
    jj describe -m "First version"
    echo "V1:$(cat .jj-plan/current.md)"
    # jj describe changes the description — file should update from jj, not vice versa
    jj describe -m "Second version"
    echo "V2:$(cat .jj-plan/current.md)"
    # Do it again to make sure repeated describes work
    jj describe -m "Third version"
    echo "V3:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"V1:First version"* ]]
  [[ "$output" == *"V2:Second version"* ]]
  [[ "$output" == *"V3:Third version"* ]]
}

@test "non-current edits and jj describe on current do not interfere" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Impl"
    # Edit non-current (Plan) file
    printf "Plan\n\n## Updated background" > ".jj-plan/01-${PLAN}.md"
    # Also jj describe current
    jj describe -m "Impl revised"
    echo "PLAN_DESC:$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)"
    echo "IMPL_FILE:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  # Plan should have the locally edited content
  [[ "$output" == *"PLAN_DESC:Plan"* ]]
  [[ "$output" == *"## Updated background"* ]]
  # Impl should have the jj describe content (not clobbered)
  [[ "$output" == *"IMPL_FILE:Impl revised"* ]]
}

@test "exact reproduction of data loss scenario: write to non-current then jj edit" {
  run_in_repo '
    # Build a stack of 4 phases
    jj describe -m "Phase 1: schema refactor"
    jj new; jj describe -m "phase 2 placeholder"
    P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "phase 3 placeholder"
    jj new; jj describe -m "phase 4 placeholder"
    # Write rich plan to phase 2 (NOT current — current is phase 4)
    printf "Phase 2: Implement branded InterpreterLayer\n\n## Background\nThis is the detailed plan that must not be lost.\n\n## Steps\n- Step A: extract trait\n- Step B: implement layer\n- Step C: wire up" > ".jj-plan/02-${P2}.md"
    # Now jj edit to phase 2 (this is the operation that caused data loss)
    jj edit -r "$P2"
    # Verify plan survived in BOTH the file and jj description
    echo "FILE_FIRST_LINE:$(head -1 .jj-plan/current.md)"
    echo "FILE_HAS_STEPS:$(grep -c "Step A" .jj-plan/current.md)"
    echo "JJ_FIRST_LINE:$("$REAL_JJ" log -r @ -T "description.first_line()" --no-graph)"
    echo "JJ_HAS_STEPS:$("$REAL_JJ" log -r @ -T description --no-graph | grep -c "Step A")"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"FILE_FIRST_LINE:Phase 2: Implement branded InterpreterLayer"* ]]
  [[ "$output" == *"FILE_HAS_STEPS:1"* ]]
  [[ "$output" == *"JJ_FIRST_LINE:Phase 2: Implement branded InterpreterLayer"* ]]
  [[ "$output" == *"JJ_HAS_STEPS:1"* ]]
}

# --- Editing plan files (original tests) ---

@test "jj describe updates the plan file (not clobbered)" {
  run_in_repo '
    jj describe -m "First version"
    echo "V1:$(cat .jj-plan/current.md)"
    jj describe -m "Second version"
    echo "V2:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"V1:First version"* ]]
  [[ "$output" == *"V2:Second version"* ]]
}

# --- Multiline descriptions ---

@test "multiline descriptions are preserved" {
  run_in_repo '
    jj describe -m "Auth refactor

## Why
Need JWT and API key support

## Steps
- Extract module
- Add JWT"
    cat .jj-plan/current.md
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"## Why"* ]]
  [[ "$output" == *"## Steps"* ]]
  [[ "$output" == *"- Extract module"* ]]
}

@test "multiline edits to plan files round-trip through jj" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    printf "Plan\n\n## Background\nSome context here\n\n## Steps\n- [x] Done\n- [ ] Todo" > .jj-plan/current.md
    jj new
    echo "DESC:$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"## Background"* ]]
  [[ "$output" == *"- [x] Done"* ]]
  [[ "$output" == *"- [ ] Todo"* ]]
}

# --- Stack summary ---

@test ".stack file is generated with first lines of plan files" {
  run_in_repo '
    jj describe -m "Refactor auth middleware"
    jj new; jj describe -m "Extract auth module"
    jj new; jj describe -m "Implement JWT strategy"
    cat .jj-plan/.stack
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"01-"*":: Refactor auth middleware"* ]]
  [[ "$output" == *"02-"*":: Extract auth module"* ]]
  [[ "$output" == *"03-"*":: Implement JWT strategy"* ]]
}

@test ".stack marks current change with asterisk" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    # Current is Step 2 (tip)
    grep "^\*" .jj-plan/.stack
    echo "---"
    # Switch to first
    jj edit -r "$PLAN"
    grep "^\*" .jj-plan/.stack
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"*   03-"*":: Step 2"* ]]
  [[ "$output" == *"*   01-"*":: Plan"* ]]
}

@test ".stack updates when stack changes" {
  run_in_repo '
    jj describe -m "Plan"
    echo "BEFORE:$(cat .jj-plan/.stack | wc -l | tr -d " ")"
    jj new; jj describe -m "Step 1"
    echo "AFTER:$(cat .jj-plan/.stack | wc -l | tr -d " ")"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"BEFORE:1"* ]]
  [[ "$output" == *"AFTER:2"* ]]
}

# --- Status indicators ---

@test ".stack shows blank for empty not-started changes" {
  run_in_repo '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    # All empty except current — first two should be blank
    echo "LINE:$(grep "01-" .jj-plan/.stack)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"LINE:    01-"* ]]
}

@test ".stack shows ~ for non-empty non-current changes" {
  run_in_repo '
    jj describe -m "Step 1"
    echo "some work" > file.txt
    jj new; jj describe -m "Step 2"
    # Step 1 has file changes, is not @
    echo "LINE:$(grep "01-" .jj-plan/.stack)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"LINE:  ~ 01-"* ]]
}

@test ".stack shows ✓ for changes with plan-status: ✅ in description" {
  run_in_repo '
    jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    # Mark Step 1 as done by editing its plan file
    printf "Step 1\n\nDid the work.\n\nplan-status: ✅" > ".jj-plan/01-${STEP1}.md"
    # Trigger a sync
    jj describe -m "Step 2 updated"
    echo "LINE:$(grep "01-" .jj-plan/.stack)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"LINE:  ✓ 01-"* ]]
}

@test ".stack shows all four status types together" {
  run_in_repo '
    # Change 0: will be marked done
    jj describe -m "Done change"
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
    cat .jj-plan/.stack
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"  ✓ 01-"*":: Done change"* ]]
  [[ "$output" == *"  ~ 02-"*":: Has changes"* ]]
  [[ "$output" == *"*   03-"*":: Current work"* ]]
  [[ "$output" == *"    04-"*":: Future work"* ]]
}

@test "plan-status: ✅ round-trips through jj description" {
  run_in_repo '
    jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    # Write done status to plan file
    printf "Step 1\n\nCompleted.\n\nplan-status: ✅" > .jj-plan/current.md
    # Switch away (flushes to jj)
    jj new; jj describe -m "Step 2"
    # Check the description was preserved
    echo "DESC:$("$REAL_JJ" log -r "$STEP1" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Step 1"* ]]
  [[ "$output" == *"plan-status: ✅"* ]]
}

@test "jj status flushes non-current file edits and updates .stack" {
  run_in_repo '
    jj describe -m "Phase 1"
    P1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "phase 2 placeholder"
    P2=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Phase 3"
    # Write rich plan to Phase 2 (not current) WITHOUT running a jj command
    printf "Phase 2: Full implementation plan\n\nDetailed steps here" > ".jj-plan/02-${P2}.md"
    # jj status should flush the edit and show updated .stack
    jj status
    echo "JJ_DESC:$("$REAL_JJ" log -r "$P2" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  # .stack should show the new first line
  [[ "$output" == *":: Phase 2: Full implementation plan"* ]]
  # jj description should be flushed
  [[ "$output" == *"JJ_DESC:Phase 2: Full implementation plan"* ]]
}

@test "jj st flushes edits to multiple non-current files" {
  run_in_repo '
    jj describe -m "Change A"
    CA=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Change B"
    CB=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Change C"
    # Edit both non-current files
    printf "Change A: revised plan" > ".jj-plan/01-${CA}.md"
    printf "Change B: revised plan" > ".jj-plan/02-${CB}.md"
    # jj st should flush both
    jj st
    echo "A_DESC:$("$REAL_JJ" log -r "$CA" -T description --no-graph)"
    echo "B_DESC:$("$REAL_JJ" log -r "$CB" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"A_DESC:Change A: revised plan"* ]]
  [[ "$output" == *"B_DESC:Change B: revised plan"* ]]
  [[ "$output" == *":: Change A: revised plan"* ]]
  [[ "$output" == *":: Change B: revised plan"* ]]
}

# --- "Done" workflow (replaces empty-stack cleanup) ---

@test "done workflow: new stack bookmark replaces old stack in .stack" {
  run_in_repo '
    jj describe -m "Old task"
    jj new; jj describe -m "Old step 1"
    echo "BEFORE:$(cat .jj-plan/.stack | wc -l | tr -d " ")"
    # Done — start a new stack
    jj new
    "$REAL_JJ" bookmark set stack/new-task -r @ 2>/dev/null
    jj describe -m "New task"
    echo "AFTER:$(cat .jj-plan/.stack | wc -l | tr -d " ")"
    echo "CONTENT:$(cat .jj-plan/current.md)"
    # Old task should not appear in .stack
    grep -c "Old task" .jj-plan/.stack && echo "HAS_OLD:yes" || echo "HAS_OLD:no"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"BEFORE:2"* ]]
  [[ "$output" == *"AFTER:1"* ]]
  [[ "$output" == *"CONTENT:New task"* ]]
  [[ "$output" == *"HAS_OLD:no"* ]]
}

@test "done workflow: old stack bookmark stays in jj history" {
  run_in_repo '
    jj describe -m "Phase 1 work"
    PHASE1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new
    "$REAL_JJ" bookmark set stack/phase2 -r @ 2>/dev/null
    jj describe -m "Phase 2 work"
    # Phase 1 description is still in jj
    echo "DESC:$("$REAL_JJ" log -r "$PHASE1" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Phase 1 work"* ]]
}

@test "done workflow: moving bare stack bookmark forward starts new stack" {
  run_in_repo '
    jj describe -m "Old plan"
    jj new; jj describe -m "Old step"
    # Move stack bookmark to a new change above
    jj new
    "$REAL_JJ" bookmark set stack -r @ 2>/dev/null
    jj describe -m "New plan"
    count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
    echo "CONTENT:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=1"* ]]
  [[ "$output" == *"CONTENT:New plan"* ]]
}

# --- Cleanup ---

@test "files for abandoned changes are removed" {
  run_in_repo '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    echo "before=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")"
    jj abandon "$STEP1"
    echo "after=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"before=3"* ]]
  [[ "$output" == *"after=2"* ]]
}

# --- Read-only passthrough ---

@test "jj log passes through without sync overhead" {
  run_in_repo '
    jj describe -m "Plan"
    rm -rf .jj-plan
    jj log -r @ -T description --no-graph
    [[ -d .jj-plan ]] && echo "RESULT:dir_recreated" || echo "RESULT:no_dir"
  '
  [[ "$output" == *"Plan"* ]]
  [[ "$output" == *"RESULT:no_dir"* ]]
}

@test "jj status without .jj-plan does not create it" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    jj status
    [[ -d .jj-plan ]] && echo 'RESULT:dir_created' || echo 'RESULT:no_dir'
  "
  [[ "$output" == *"RESULT:no_dir"* ]]
}

@test "jj status appends plan stack when .jj-plan is active" {
  run_in_repo '
    jj describe -m "Refactor auth"
    jj new; jj describe -m "Extract module"
    jj status
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"01-"*":: Refactor auth"* ]]
  [[ "$output" == *"02-"*":: Extract module"* ]]
}

@test "jj st also appends plan stack" {
  run_in_repo '
    jj describe -m "My plan"
    jj st
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: My plan"* ]]
}

# --- Subdirectory support ---

@test "jj status appends plan stack from a subdirectory" {
  run_in_repo '
    jj describe -m "Refactor auth"
    jj new; jj describe -m "Extract module"
    mkdir -p src/deep/nested
    cd src/deep/nested
    jj status
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"01-"*":: Refactor auth"* ]]
  [[ "$output" == *"02-"*":: Extract module"* ]]
}

@test "jj st appends plan stack from a subdirectory" {
  run_in_repo '
    jj describe -m "My plan"
    mkdir -p lib
    cd lib
    jj st
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: My plan"* ]]
}

@test "mutating commands sync plans from a subdirectory" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 1"
    mkdir -p src
    cd src
    jj new; jj describe -m "Step 2"
    count=$(ls ../.jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
    echo "CONTENT:$(cat ../.jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=3"* ]]
  [[ "$output" == *"CONTENT:Step 2"* ]]
}

@test "editing current.md from subdir flushes to jj on switch" {
  run_in_repo '
    jj describe -m "Original"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Impl"
    IMPL=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    printf "Updated from subdir" > .jj-plan/current.md
    mkdir -p src
    cd src
    jj edit -r "$PLAN"
    echo "DESC:$("$REAL_JJ" log -r "$IMPL" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Updated from subdir"* ]]
}

# --- Error state: max changes ---

@test "exceeding max changes creates error.md" {
  run_in_repo_with_max 3 '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    jj new; jj describe -m "Step 3"
    [[ -f .jj-plan/error.md ]] && echo "RESULT:error_exists" || echo "RESULT:no_error"
    echo "LINK:$(readlink .jj-plan/current.md)"
  '
  [[ "$output" == *"RESULT:error_exists"* ]]
  [[ "$output" == *"LINK:error.md"* ]]
}

@test "error.md contains a descriptive message" {
  run_in_repo_with_max 3 '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    jj new; jj describe -m "Step 3"
    echo "MSG:$(cat .jj-plan/error.md)"
  '
  [[ "$output" == *"max 3"* ]]
  [[ "$output" == *"Refusing to sync"* ]]
}

@test "error state self-heals when stack shrinks below max" {
  run_in_repo_with_max 3 '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    jj new; jj describe -m "Step 3"
    [[ -f .jj-plan/error.md ]] && echo "STATE:in_error" || echo "STATE:no_error"
    jj squash -m "Step 2+3 combined"
    jj edit -r @-
    [[ -f .jj-plan/error.md ]] && echo "STATE:still_error" || echo "STATE:resolved"
  '
  [[ "$output" == *"STATE:in_error"* ]]
  [[ "$output" == *"STATE:resolved"* ]]
}

@test "flush is skipped during error state (no description clobber)" {
  run_in_repo_with_max 3 '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    jj new; jj describe -m "Step 3"
    jj describe -m "Step 3 updated"
    echo "DESC:$("$REAL_JJ" log -r @ -T description --no-graph)"
  '
  [[ "$output" == *"DESC:Step 3 updated"* ]]
}

# --- Edge cases ---

@test "jj new from empty description produces empty plan file" {
  run_in_repo '
    jj describe -m "Plan"
    jj new
    content=$(cat .jj-plan/current.md)
    [[ -z "$content" ]] && echo "RESULT:empty" || echo "RESULT:not_empty"
  '
  [[ "$output" == *"RESULT:empty"* ]]
}

@test "works outside a jj repo without errors" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    cd \"\$(mktemp -d)\"
    jj version
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"jj"* ]]
}

# --- Activation / deactivation ---

@test "no .jj-plan directory means full passthrough (no sync)" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    jj describe -m 'Should not create .jj-plan'
    [[ -d .jj-plan ]] && echo 'RESULT:dir_created' || echo 'RESULT:no_dir'
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:no_dir"* ]]
}

@test "passthrough still runs jj commands correctly without .jj-plan" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    jj describe -m 'Test description'
    echo \"DESC:\$($REAL_JJ log -r @ -T description --no-graph)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Test description"* ]]
}

@test "creating .jj-plan activates sync" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    jj describe -m 'Before activation'
    [[ -d .jj-plan ]] && echo 'RESULT:premature' || echo 'RESULT:inactive'
    mkdir .jj-plan
    jj describe -m 'After activation'
    [[ -f .jj-plan/current.md ]] && echo 'RESULT:active' || echo 'RESULT:still_inactive'
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:inactive"* ]]
  [[ "$output" == *"RESULT:active"* ]]
}

# --- Stack bookmark: bare "stack" ---

@test "bare stack bookmark is used when present" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'initial' 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'landed feature' 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'Active work'
    # The stack should include: landed feature (stack bookmark) + Active work
    count=\$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d ' ')
    echo \"count=\$count\"
    echo \"FIRST:\$(cat .jj-plan/01-*.md)\"
    echo \"CURRENT:\$(cat .jj-plan/current.md)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=2"* ]]
  [[ "$output" == *"FIRST:landed feature"* ]]
  [[ "$output" == *"CURRENT:Active work"* ]]
}

@test "bare stack bookmark excludes changes below it from the stack" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'old work 1' 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'old work 2' 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'stack start' 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'New step 1' 2>/dev/null
    for f in .jj-plan/[0-9][0-9]-*.md; do
      echo \"FILE:\$(cat \$f)\"
    done
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"FILE:stack start"* ]]
  [[ "$output" == *"FILE:New step 1"* ]]
  # Old work should NOT appear
  [[ "$output" != *"FILE:old work"* ]]
}

@test "advancing bare stack bookmark shrinks the stack" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    echo "before=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")"
    # Advance stack to Step 1 — Plan drops out
    "$REAL_JJ" bookmark set stack -r "$STEP1" 2>/dev/null
    # Trigger a sync
    jj describe -m "Step 2 updated"
    echo "after=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")"
    # Plan should not be in .stack
    grep -c "Plan" .jj-plan/.stack && echo "HAS_PLAN:yes" || echo "HAS_PLAN:no"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"before=3"* ]]
  [[ "$output" == *"after=2"* ]]
  [[ "$output" == *"HAS_PLAN:no"* ]]
}

# --- Stack bookmark: named "stack/*" ---

@test "stack/named bookmark works as stack boundary" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'pre-work' 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'feature start' 2>/dev/null
    $REAL_JJ bookmark set stack/my-feature -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'Feature step 1'
    count=\$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d ' ')
    echo \"count=\$count\"
    echo \"FIRST:\$(cat .jj-plan/01-*.md)\"
    echo \"CURRENT:\$(cat .jj-plan/current.md)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=2"* ]]
  [[ "$output" == *"FIRST:feature start"* ]]
  [[ "$output" == *"CURRENT:Feature step 1"* ]]
}

# --- Nearest ancestor resolution ---

@test "nearest stack/* ancestor wins when multiple exist" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'phase 1 root' 2>/dev/null
    $REAL_JJ bookmark set stack/phase1 -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'phase 1 work' 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'phase 2 root' 2>/dev/null
    $REAL_JJ bookmark set stack/phase2 -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'phase 2 work'
    # Should see phase 2 root + phase 2 work (not phase 1 stuff)
    count=\$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d ' ')
    echo \"count=\$count\"
    echo \"FIRST:\$(cat .jj-plan/01-*.md)\"
    echo \"CURRENT:\$(cat .jj-plan/current.md)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=2"* ]]
  [[ "$output" == *"FIRST:phase 2 root"* ]]
  [[ "$output" == *"CURRENT:phase 2 work"* ]]
}

@test "bare stack and stack/named coexist — nearest wins" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'old base' 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'named start' 2>/dev/null
    $REAL_JJ bookmark set stack/feature -r @ 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'Feature work'
    # stack/feature is nearer than stack — should be used
    count=\$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d ' ')
    echo \"count=\$count\"
    echo \"FIRST:\$(cat .jj-plan/01-*.md)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=2"* ]]
  [[ "$output" == *"FIRST:named start"* ]]
}

@test "ambiguous sibling stack/* bookmarks produce an error" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'root' 2>/dev/null
    ROOT=\$($REAL_JJ log -r @ -T 'change_id.shortest(8)' --no-graph)
    # Create two sibling branches with stack bookmarks
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'branch a' 2>/dev/null
    $REAL_JJ bookmark set stack/a -r @ 2>/dev/null
    BA=\$($REAL_JJ log -r @ -T 'change_id.shortest(8)' --no-graph)
    $REAL_JJ new -r \"\$ROOT\" 2>/dev/null
    $REAL_JJ describe -m 'branch b' 2>/dev/null
    $REAL_JJ bookmark set stack/b -r @ 2>/dev/null
    BB=\$($REAL_JJ log -r @ -T 'change_id.shortest(8)' --no-graph)
    # Create a merge of both branches
    $REAL_JJ new -r \"\$BA\" -r \"\$BB\" 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'merge work'
    [[ -f .jj-plan/error.md ]] && echo 'RESULT:error' || echo 'RESULT:no_error'
    echo \"MSG:\$(cat .jj-plan/error.md 2>/dev/null)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:error"* ]]
  [[ "$output" == *"Ambiguous stack"* ]]
}

@test "stack/* on a different branch does not affect stack" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'root' 2>/dev/null
    ROOT=\$($REAL_JJ log -r @ -T 'change_id.shortest(8)' --no-graph)
    # Create a side branch and put stack/other there
    $REAL_JJ new 2>/dev/null
    $REAL_JJ describe -m 'side branch' 2>/dev/null
    $REAL_JJ bookmark set stack/other -r @ 2>/dev/null
    # Go back to root, start a different line of work
    $REAL_JJ new -r \"\$ROOT\" 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'Main line work'
    # stack/other is not an ancestor of @, so it should not be used
    count=\$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d ' ')
    [[ -f .jj-plan/current.md ]] && echo 'RESULT:synced' || echo 'RESULT:no_sync'
    echo \"count=\$count\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:no_sync"* ]]
  [[ "$output" == *"count=0"* ]]
}

# --- trunk() fallback (exclusive) ---

@test "trunk() is used as fallback when no stack bookmark exists" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    # Create a remote so trunk() resolves to something useful
    REMOTE=\"\$(mktemp -d)\"
    git init --bare \"\$REMOTE\" 2>/dev/null
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ git remote add origin \"\$REMOTE\" 2>/dev/null
    $REAL_JJ describe -m 'initial' 2>/dev/null
    $REAL_JJ bookmark set main -r @ 2>/dev/null
    $REAL_JJ git push --bookmark main 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'Feature work'
    echo \"CONTENT:\$(cat .jj-plan/current.md)\"
    [[ -f .jj-plan/current.md ]] && echo 'RESULT:synced' || echo 'RESULT:no_sync'
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:synced"* ]]
  [[ "$output" == *"CONTENT:Feature work"* ]]
}

@test "trunk() fallback is exclusive — trunk commit not in stack" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    REMOTE=\"\$(mktemp -d)\"
    git init --bare \"\$REMOTE\" 2>/dev/null
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ git remote add origin \"\$REMOTE\" 2>/dev/null
    $REAL_JJ describe -m 'trunk commit' 2>/dev/null
    $REAL_JJ bookmark set main -r @ 2>/dev/null
    $REAL_JJ git push --bookmark main 2>/dev/null
    $REAL_JJ new 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'My work'
    # 'trunk commit' must NOT be in the stack (exclusive range)
    for f in .jj-plan/[0-9][0-9]-*.md; do
      echo \"FILE:\$(cat \$f)\"
    done
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"FILE:My work"* ]]
  [[ "$output" != *"FILE:trunk commit"* ]]
}

@test "no sync when neither stack bookmark nor useful trunk() exists" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    mkdir -p .jj-plan
    jj describe -m 'Orphan work'
    count=\$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d ' ')
    echo \"count=\$count\"
    [[ -f .jj-plan/current.md ]] && echo 'RESULT:has_current' || echo 'RESULT:no_current'
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=0"* ]]
  [[ "$output" == *"RESULT:no_current"* ]]
}

# --- jj stack new ---

@test "jj stack new creates a change with bare stack bookmark" {
  run_in_repo '
    jj describe -m "Old plan"
    jj stack new
    # The bare "stack" bookmark should be on @
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
    # @ should be empty (fresh change)
    desc=$("$REAL_JJ" log -r @ -T description --no-graph)
    echo "DESC:[$desc]"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$output" == *"BM:stack:"* ]]
  [[ "$output" == *"DESC:[]"* ]]
}

@test "jj stack new my-feature creates a change with stack/my-feature bookmark" {
  run_in_repo '
    jj describe -m "Old plan"
    jj stack new my-feature
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Started new stack: stack/my-feature ("* ]]
  [[ "$output" == *"stack/my-feature:"* ]]
}

@test "jj stack new -r REV roots the new stack off the given revision" {
  run_in_repo '
    jj describe -m "Base"
    BASE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Child"
    jj new; jj describe -m "Grandchild"
    # Start a new stack rooted at Base
    jj stack new -r "$BASE"
    # Parent of @ should be Base
    parent_desc=$("$REAL_JJ" log -r @- -T description --no-graph)
    echo "PARENT:$parent_desc"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$output" == *"PARENT:Base"* ]]
}

@test "jj stack new -r REV my-feature combines revision and name" {
  run_in_repo '
    jj describe -m "Base"
    BASE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Child"
    jj stack new -r "$BASE" my-feature
    parent_desc=$("$REAL_JJ" log -r @- -T description --no-graph)
    echo "PARENT:$parent_desc"
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Started new stack: stack/my-feature ("* ]]
  [[ "$output" == *"PARENT:Base"* ]]
  [[ "$output" == *"stack/my-feature:"* ]]
}

@test "jj stack new flushes pending edits before creating the new stack" {
  run_in_repo '
    jj describe -m "Original plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    # Write a local edit to current.md (not yet flushed)
    printf "Revised plan with important details" > .jj-plan/current.md
    # stack new should flush before creating the new change
    jj stack new
    # The old plan change should have the revised description
    echo "DESC:$("$REAL_JJ" log -r "$PLAN" -T description --no-graph)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"DESC:Revised plan with important details"* ]]
}

@test "current.md is updated after jj stack new" {
  run_in_repo '
    jj describe -m "Old plan"
    jj stack new
    NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    [[ -L .jj-plan/current.md ]] && echo "RESULT:is_symlink" || echo "RESULT:not_symlink"
    link=$(readlink .jj-plan/current.md)
    echo "LINK:$link"
    echo "NEW_ID:$NEW_ID"
    # The symlink target should contain the new change ID
    [[ "$link" == *"$NEW_ID"* ]] && echo "MATCH:yes" || echo "MATCH:no"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:is_symlink"* ]]
  [[ "$output" == *"MATCH:yes"* ]]
}

@test "jj stack new prints confirmation with change ID" {
  run_in_repo '
    jj describe -m "Old plan"
    jj stack new
    NEW_ID=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    echo "NEW_ID:$NEW_ID"
  '
  [[ "$status" -eq 0 ]]
  # Confirmation line should contain "Started new stack: stack (CHANGE_ID)"
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$output" == *")"* ]]
}

@test "jj stack new with invalid name fails cleanly and rolls back" {
  run_in_repo '
    jj describe -m "Old plan"
    BEFORE=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj stack new "invalid name" 2>&1
    AFTER=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    echo "BEFORE:$BEFORE"
    echo "AFTER:$AFTER"
    # @ should be back to the original change after rollback
    [[ "$BEFORE" == "$AFTER" ]] && echo "ROLLBACK:yes" || echo "ROLLBACK:no"
  '
  # The command itself fails, but the test script continues
  [[ "$output" == *"ROLLBACK:yes"* ]]
}

@test "jj stack new -r <ancestor> moves bare stack bookmark sideways with -B" {
  run_in_repo '
    jj describe -m "Root"
    ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Middle"
    jj new; jj describe -m "Tip"
    # stack bookmark is on Root; now start a new stack rooted at Root
    # This requires -B because the bookmark moves sideways (Root -> new sibling of Root)
    jj stack new -r "$ROOT"
    parent_desc=$("$REAL_JJ" log -r @- -T description --no-graph)
    echo "PARENT:$parent_desc"
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Started new stack: stack ("* ]]
  [[ "$output" == *"PARENT:Root"* ]]
}

# --- Navigation commands show plan stack ---

@test "jj new appends plan stack when .jj-plan is active" {
  run_in_repo '
    jj describe -m "Plan"
    jj new
    jj describe -m "Step 1"
    jj new
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *":: Plan"* ]]
  [[ "$output" == *":: Step 1"* ]]
}

@test "jj edit appends plan stack when .jj-plan is active" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 1"
    jj new; jj describe -m "Step 2"
    jj edit -r "$PLAN"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"*   01-"*":: Plan"* ]]
  [[ "$output" == *":: Step 1"* ]]
  [[ "$output" == *":: Step 2"* ]]
}

@test "jj stack new appends plan stack after confirmation" {
  run_in_repo '
    jj describe -m "Old plan"
    jj stack new my-feature
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"Started new stack: stack/my-feature ("* ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
  [[ "$output" == *"*   01-"* ]]
}

@test "jj new without .jj-plan does not show stack (passthrough)" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ describe -m 'base' 2>/dev/null
    jj new 2>&1
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"Plan stack"* ]]
}

# --- Two-column status: here + status independent ---

@test "change that is both current AND done shows * ✓" {
  run_in_repo '
    jj describe -m "Plan"
    PLAN=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 1"
    # Mark Plan as done
    printf "Plan\n\nplan-status: ✅" > ".jj-plan/01-${PLAN}.md"
    # Switch back to Plan — it is both current AND done
    jj edit -r "$PLAN"
    echo "LINE:$(grep "01-" .jj-plan/.stack)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"LINE:* ✓ 01-"*":: Plan"* ]]
}

@test "plan-status: ✅ detected when not on the last line" {
  run_in_repo '
    jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    # Write plan-status in the middle, with trailing content after it
    printf "Step 1\n\nplan-status: ✅\n\n## Notes\nSome trailing content" > ".jj-plan/01-${STEP1}.md"
    # Trigger a sync
    jj describe -m "Step 2 updated"
    echo "LINE:$(grep "01-" .jj-plan/.stack)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"LINE:  ✓ 01-"* ]]
}

# --- Stack bookmark protection on abandon ---

@test "abandon stack-bookmarked change with descendants moves bookmark to first child" {
  run_in_repo '
    jj describe -m "Stack root"
    ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Child"
    CHILD=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Grandchild"
    # Abandon the stack root (holds the bookmark)
    jj abandon "$ROOT"
    # Bookmark should have moved to Child
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
    echo "CHILD:$CHILD"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"moved stack bookmark stack to"* ]]
  [[ "$output" == *"BM:stack:"* ]]
}

@test "abandon stack-bookmarked @ with no descendants moves bookmark to new @" {
  run_in_repo '
    jj describe -m "Sole member"
    # @ is the stack bookmark, no children
    jj abandon
    # jj creates a new @ — bookmark should move there
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
    new_id=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    echo "NEW_ID:$new_id"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"moved stack bookmark stack to"* ]]
  [[ "$output" == *"BM:stack:"* ]]
}

# Note: "abandon bookmarked non-@ with no descendants" is unreachable.
# If the bookmarked change is in ::@ and is not @, it always has at least
# @ as a descendant. The warning path in the shim exists as a safety net
# but cannot be triggered in normal usage.

@test "abandon non-bookmarked middle change does not interfere with bookmark" {
  run_in_repo '
    jj describe -m "Plan"
    jj new; jj describe -m "Step 1"
    STEP1=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Step 2"
    jj abandon "$STEP1"
    # Bookmark should still be on the original root
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"BM:stack:"* ]]
  [[ "$output" != *"moved stack bookmark"* ]]
  [[ "$output" != *"WARNING"* ]]
}

@test "abandon with --retain-bookmarks does not trigger shim recovery" {
  run_in_repo '
    jj describe -m "Stack root"
    ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Child"
    # Abandon with --retain-bookmarks — shim should not interfere
    jj abandon "$ROOT" --retain-bookmarks 2>&1
    bm=$("$REAL_JJ" bookmark list --no-pager 2>&1)
    echo "BM:$bm"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" != *"moved stack bookmark"* ]]
}

@test "general bookmark loss detection warns on jj bookmark delete" {
  run_in_repo '
    jj describe -m "Plan"
    jj bookmark delete stack 2>&1
  '
  [[ "$output" == *"WARNING: stack bookmark was lost"* ]]
}

@test ".jj-plan is correctly synced after bookmark recovery on abandon" {
  run_in_repo '
    jj describe -m "Stack root"
    ROOT=$("$REAL_JJ" log -r @ -T "change_id.shortest(8)" --no-graph)
    jj new; jj describe -m "Child"
    jj new; jj describe -m "Grandchild"
    # Abandon the stack root — bookmark should move to Child
    jj abandon "$ROOT"
    # .jj-plan should reflect the recovered stack (Child + Grandchild)
    count=$(ls .jj-plan/[0-9][0-9]-*.md | wc -l | tr -d " ")
    echo "count=$count"
    echo "FIRST:$(cat .jj-plan/01-*.md)"
    echo "CURRENT:$(cat .jj-plan/current.md)"
  '
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"count=2"* ]]
  [[ "$output" == *"FIRST:Child"* ]]
  [[ "$output" == *"CURRENT:Grandchild"* ]]
}

# --- Legacy .jj-plans fallback ---

@test "legacy .jj-plans/ works when .jj-plan/ does not exist" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    mkdir -p .jj-plans
    jj describe -m 'Legacy plan'
    echo \"CONTENT:\$(cat .jj-plans/current.md)\"
    [[ -f .jj-plans/current.md ]] && echo 'RESULT:synced' || echo 'RESULT:no_sync'
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"RESULT:synced"* ]]
  [[ "$output" == *"CONTENT:Legacy plan"* ]]
  [[ "$output" == *"Plan stack (.jj-plans/;"* ]]
}

@test ".jj-plan/ takes precedence when both .jj-plan/ and .jj-plans/ exist" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    mkdir -p .jj-plan .jj-plans
    jj describe -m 'Precedence test'
    # .jj-plan should be used (preferred)
    [[ -f .jj-plan/current.md ]] && echo 'PREFERRED:yes' || echo 'PREFERRED:no'
    # .jj-plans should NOT get plan files
    count=\$(ls .jj-plans/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d ' ')
    echo \"LEGACY_COUNT=\$count\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"PREFERRED:yes"* ]]
  [[ "$output" == *"LEGACY_COUNT=0"* ]]
  [[ "$output" == *"Plan stack (.jj-plan/;"* ]]
}

@test "JJ_PLAN_DIR env var overrides both .jj-plan/ and .jj-plans/" {
  run zsh -c "
    export PATH=\"$HOME/.local/bin:\$PATH\"
    REAL_JJ=\"$REAL_JJ\"
    cd \"\$(mktemp -d)\"
    $REAL_JJ git init 2>/dev/null
    $REAL_JJ bookmark set stack -r @ 2>/dev/null
    mkdir -p .jj-plan .jj-plans .custom-plans
    export JJ_PLAN_DIR=\"\$(pwd)/.custom-plans\"
    jj describe -m 'Custom dir test'
    [[ -f .custom-plans/current.md ]] && echo 'CUSTOM:yes' || echo 'CUSTOM:no'
    # Neither default dir should get plan files
    default_count=\$(ls .jj-plan/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d ' ')
    legacy_count=\$(ls .jj-plans/[0-9][0-9]-*.md 2>/dev/null | wc -l | tr -d ' ')
    echo \"DEFAULT_COUNT=\$default_count\"
    echo \"LEGACY_COUNT=\$legacy_count\"
    echo \"CONTENT:\$(cat .custom-plans/current.md)\"
  "
  [[ "$status" -eq 0 ]]
  [[ "$output" == *"CUSTOM:yes"* ]]
  [[ "$output" == *"DEFAULT_COUNT=0"* ]]
  [[ "$output" == *"LEGACY_COUNT=0"* ]]
  [[ "$output" == *"CONTENT:Custom dir test"* ]]
  [[ "$output" == *"Plan stack (.custom-plans/;"* ]]
}