#!/bin/zsh
# jj-plan: plan-oriented programming shim
# Keeps .jj-plan/ in sync with the current stack's change descriptions.
#
# .jj-plan/ contains one .md file per change in the stack, named with a
# sort index and short change ID. A current.md symlink points to the
# active change's plan file.
#
# ACTIVATION: create .jj-plan/ in a repo root to enable plan sync.
# Without it, all jj commands pass through with zero overhead.
#
# PLAN DIRECTORY resolution (fallback chain):
#   1. JJ_PLAN_DIR env var — if set, use as-is (no fallback)
#   2. .jj-plan/ in repo root — preferred default
#   3. .jj-plans/ in repo root — legacy fallback
#
# STACK BASE resolution (fallback chain):
#   1. stack / stack/* bookmarks — nearest ancestor of @ (inclusive range)
#      The bookmarked change IS the first member of the stack.
#      If multiple stack/* bookmarks are equidistant siblings → error.
#   2. trunk() — if it resolves to something other than root() (exclusive range)
#      The trunk commit is NOT part of the stack.
#   3. No sync — stack boundary cannot be determined
#
# Requires .jj-plan (and .jj-plans) in global gitignore (e.g. ~/.config/git/ignore).
#
# Architecture:
#   __jj_plan_batch_read — core: single jj log call → associative arrays
#                          Uses RS (\x1e) as field separator, NUL (\0) as record separator.
#   __jj_plan_flush_all  — files → jj: flush ALL local file edits to jj descriptions
#   __jj_plan_sync       — jj → files: mirror jj stack state to plan files, symlink, .stack
#                          Also handles bookmark-loss detection (plan files exist but no base).
#   __jj_plan_show_stack — display: print .stack summary to stdout (pure display, no side effects)
#   __jj_plan_wrap       — unified handler for simple command paths (flush→cmd→sync→show)

# Resolve the real jj binary (skip this script)
SELF="$(realpath "$0")"
REAL_JJ=""
for dir in $path; do
  candidate="$dir/jj"
  if [[ -x "$candidate" && "$(realpath "$candidate")" != "$SELF" ]]; then
    REAL_JJ="$candidate"
    break
  fi
done

if [[ -z "$REAL_JJ" ]]; then
  echo "jj-plan: cannot find real jj binary" >&2
  exit 1
fi

JJ_PLAN_DIR="${JJ_PLAN_DIR:-}"
JJ_PLAN_MAX="${JJ_PLAN_MAX:-50}"
JJ_PLAN_DIR_SOURCE=""

# Read-only commands: zero overhead passthrough.
# Note: status/st are NOT here — they get special handling to append .stack.
__jj_plan_readonly_commands=(
  log diff show interdiff evolog file config
  help version root tag op operation util git
  gerrit sign unsign workspace
)

__jj_plan_set_error() {
  local plans_dir="$JJ_PLAN_DIR"
  local msg="$1"
  printf '%s\n' "$msg" > "$plans_dir/error.md"
  rm -f "$plans_dir/current.md"
  ln -s "error.md" "$plans_dir/current.md"
  echo "jj-plan: ERROR: $msg" >&2
}

__jj_plan_clear_error() {
  local plans_dir="$JJ_PLAN_DIR"
  if [[ -f "$plans_dir/error.md" ]]; then
    rm -f "$plans_dir/error.md"
  fi
}

# Batch-read change data from jj in a single call.
# Populates associative arrays keyed by change ID and an ordered ID list.
#
# Usage: __jj_plan_batch_read REVSET
# Sets:  _bp_desc[ID]  _bp_empty[ID]  _bp_wc[ID]  _bp_bm[ID]  _bp_ordered_ids=(...)
# Returns 1 if the revset produced no results.
__jj_plan_batch_read() {
  local revset="$1"

  # Clear previous results
  _bp_ordered_ids=()
  typeset -gA _bp_desc _bp_empty _bp_wc _bp_bm
  _bp_desc=()
  _bp_empty=()
  _bp_wc=()
  _bp_bm=()

  local raw
  raw="$("$REAL_JJ" log -r "$revset" \
    -T 'change_id.shortest(8) ++ "\x1e" ++ if(bookmarks, bookmarks.join(","), "-") ++ "\x1e" ++ if(empty, "E", "F") ++ "\x1e" ++ if(self.contained_in("@"), "C", "-") ++ "\x1e" ++ description ++ "\0"' \
    --reversed --no-graph 2>/dev/null)"

  if [[ -z "$raw" ]]; then
    return 1
  fi

  local id bm empty_flag wc_flag desc
  while IFS=$'\x1e' read -d $'\0' id bm empty_flag wc_flag desc; do
    [[ -z "$id" ]] && continue
    # Strip trailing newline from description (jj appends one)
    desc="${desc%$'\n'}"
    _bp_ordered_ids+=("$id")
    _bp_desc[$id]="$desc"
    _bp_empty[$id]="$empty_flag"
    _bp_wc[$id]="$wc_flag"
    _bp_bm[$id]="$bm"
  done <<< "$raw"

  [[ ${#_bp_ordered_ids} -eq 0 ]] && return 1
  return 0
}

# Resolve the stack base using the fallback chain.
# Prints "inclusive:CHANGE_ID" or "exclusive:REVSET" to signal range mode.
# Returns 1 if no usable base is found.
__jj_plan_resolve_stack_base() {
  # 1. stack / stack/* bookmarks — nearest ancestor of @ (inclusive)
  local stack_heads
  stack_heads="$("$REAL_JJ" log \
    -r 'heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)' \
    -T 'change_id.shortest(8) ++ "\n"' \
    --no-graph 2>/dev/null | grep .)"

  if [[ -n "$stack_heads" ]]; then
    local count
    count="$(echo "$stack_heads" | wc -l | tr -d ' ')"

    if [[ "$count" -eq 1 ]]; then
      echo "inclusive:$stack_heads"
      return 0
    fi

    # Multiple heads — ambiguous sibling bookmarks
    __jj_plan_set_error "Ambiguous stack: multiple stack/* bookmarks are equidistant ancestors of @. Conflicting change IDs: $(echo "$stack_heads" | tr '\n' ' '). Advance or remove one so a single nearest ancestor remains."
    return 1
  fi

  # 2. trunk() — if it resolves to something other than root() (exclusive)
  local trunk_check
  trunk_check="$("$REAL_JJ" log -r 'trunk() & ~root()' -T 'change_id' --no-graph 2>/dev/null)"
  if [[ -n "$trunk_check" ]]; then
    echo "exclusive:trunk()"
    return 0
  fi

  # 3. No usable base
  return 1
}

# files → jj: Flush ALL local plan file edits to jj descriptions.
# Uses a single batch read to get all current jj descriptions, then
# only calls jj describe for files that actually differ.
__jj_plan_flush_all() {
  local plans_dir="$JJ_PLAN_DIR"

  # Don't flush if current.md points to error.md (error state)
  if [[ -L "$plans_dir/current.md" && "$(readlink "$plans_dir/current.md")" == "error.md" ]]; then
    return
  fi

  # Collect change IDs from plan filenames
  local -a flush_ids
  local -A flush_files  # change_id → filepath
  for f in "$plans_dir"/[0-9][0-9]-*.md(N); do
    local fname="${f##*/}"
    local change_id="${fname#[0-9][0-9]-}"
    change_id="${change_id%.md}"
    flush_ids+=("$change_id")
    flush_files[$change_id]="$f"
  done

  [[ ${#flush_ids} -eq 0 ]] && return

  # Build a revset for all plan file change IDs and batch-read descriptions
  local revset="${(j: | :)flush_ids}"
  __jj_plan_batch_read "$revset" 2>/dev/null

  # Compare each file's content against the batch-read description
  for change_id in "${flush_ids[@]}"; do
    local f="${flush_files[$change_id]}"
    local file_content="$(cat "$f")"

    # Skip if change no longer exists (abandoned externally)
    if [[ -z "${_bp_desc[$change_id]+x}" ]]; then
      continue
    fi

    local jj_desc="${_bp_desc[$change_id]}"

    if [[ "$file_content" != "$jj_desc" && -n "$file_content" ]]; then
      "$REAL_JJ" describe -r "$change_id" -m "$file_content" 2>/dev/null
    fi
  done
}

# jj → files: Mirror jj stack state to plan files, symlink, and .stack.
# After this runs, .jj-plan/ exactly reflects the current jj stack.
# Assumes __jj_plan_flush_all has already been called (jj descriptions
# are authoritative at this point).
#
# Also handles bookmark-loss detection: if resolve fails but plan files
# exist, a stack was lost — emit a warning.
__jj_plan_sync() {
  local plans_dir="$JJ_PLAN_DIR"

  # Resolve stack base
  local resolve_result
  resolve_result="$(__jj_plan_resolve_stack_base)"
  if [[ $? -ne 0 ]]; then
    # Bookmark-loss detection: if plan files exist, a stack was lost
    if [[ -n "$plans_dir"/[0-9][0-9]-*.md(#qN[1]) ]]; then
      echo "jj-plan: WARNING: stack bookmark was lost. Run: jj bookmark set stack -r <change>" >&2
    fi
    return
  fi

  # Parse inclusive/exclusive mode and base value
  local range_mode="${resolve_result%%:*}"
  local stack_base="${resolve_result#*:}"

  # Build the stack revset based on range mode
  local stack_revset
  if [[ "$range_mode" == "inclusive" ]]; then
    stack_revset="($stack_base::@) | descendants(@)"
  else
    stack_revset="($stack_base..@) | descendants(@)"
  fi

  # Single batch read: gets all IDs, bookmarks, empty flags, WC flags, descriptions
  if ! __jj_plan_batch_read "$stack_revset"; then
    return
  fi

  # Check stack size against max
  if [[ ${#_bp_ordered_ids} -gt "$JJ_PLAN_MAX" ]]; then
    __jj_plan_set_error "Stack has ${#_bp_ordered_ids} changes (max $JJ_PLAN_MAX). Refusing to sync. Is @ in the right place? Consider: jj bookmark set stack -r <change>"
    return
  fi

  # Stack is within bounds — clear any previous error
  __jj_plan_clear_error

  # Build lookup set for current stack
  typeset -A current_stack
  for id in "${_bp_ordered_ids[@]}"; do
    current_stack[$id]=1
  done

  # Remove files for changes no longer in the stack
  for f in "$plans_dir"/[0-9][0-9]-*.md(N); do
    local fname="${f##*/}"
    local fid="${fname#[0-9][0-9]-}"
    fid="${fid%.md}"
    if [[ -z "${current_stack[$fid]+x}" ]]; then
      rm -f "$f"
    fi
  done

  # Derive current change ID from WC flag
  local current_change_id=""
  local current_file=""

  # Write/update plan files from batch-read descriptions (jj is authoritative)
  local idx=1
  for i in {1..${#_bp_ordered_ids}}; do
    local change_id="${_bp_ordered_ids[$i]}"
    local padded_idx="$(printf '%02d' $idx)"
    local target_file="$plans_dir/${padded_idx}-${change_id}.md"

    # Handle reordering: move existing file if index changed
    for existing in "$plans_dir"/[0-9][0-9]-${change_id}.md(N); do
      if [[ "$existing" != "$target_file" ]]; then
        mv "$existing" "$target_file"
      fi
    done

    # Write description to file
    printf '%s' "${_bp_desc[$change_id]}" > "$target_file"

    if [[ "${_bp_wc[$change_id]}" == "C" ]]; then
      current_change_id="$change_id"
      current_file="${padded_idx}-${change_id}.md"
    fi

    ((idx++))
  done

  # Update current.md symlink
  rm -f "$plans_dir/current.md"
  if [[ -n "$current_file" ]]; then
    ln -s "$current_file" "$plans_dir/current.md"
  fi

  # Generate .stack summary
  {
    for i in {1..${#_bp_ordered_ids}}; do
      local sid="${_bp_ordered_ids[$i]}"
      local padded="$(printf '%02d' $i)"
      local desc="${_bp_desc[$sid]}"
      local first_line="${desc%%$'\n'*}"

      local here=" "
      local status_marker=" "
      if [[ "${padded}-${sid}.md" == "$current_file" ]]; then
        here="*"
      fi
      if [[ "$desc" == *$'\n'"plan-status: ✅"* ]] || [[ "$desc" == "plan-status: ✅"* ]]; then
        status_marker="✓"
      elif [[ "${_bp_empty[$sid]}" == "F" ]]; then
        status_marker="~"
      fi

      printf '%s %s %s-%s :: %s\n' "$here" "$status_marker" "$padded" "$sid" "$first_line"
    done
  } > "$plans_dir/.stack"
}

# Display the plan stack summary to stdout.
# Pure display function — reads the .stack file and prints it.
# Call after __jj_plan_sync has run so .stack is up to date.
__jj_plan_show_stack() {
  if [[ -s "$JJ_PLAN_DIR/.stack" ]]; then
    echo ""
    echo "Plan stack (${JJ_PLAN_DIR##*/}/; *=here ✓=done ~=has changes):"
    cat "$JJ_PLAN_DIR/.stack"
  fi
}

# Unified handler for simple command paths: flush → command → sync → show.
# Used by status/st, new/edit, and the general catch-all.
__jj_plan_wrap() {
  __jj_plan_flush_all
  "$REAL_JJ" "$@"
  local jj_exit=$?
  __jj_plan_sync
  __jj_plan_show_stack
  exit $jj_exit
}

# --- Main ---

# Pass through if no subcommand, or if read-only
if [[ -z "$1" ]] || (( ${__jj_plan_readonly_commands[(Ie)$1]} )); then
  exec "$REAL_JJ" "$@"
fi

# Resolve repo root and plan directory
# Fallback chain: JJ_PLAN_DIR env var → .jj-plan → .jj-plans (legacy)
local repo_root
repo_root="$("$REAL_JJ" root 2>/dev/null)"
if [[ -n "$JJ_PLAN_DIR" ]]; then
  # Env var is set — use as-is (no fallback)
  JJ_PLAN_DIR_SOURCE="env var"
elif [[ -n "$repo_root" && -d "$repo_root/.jj-plan" ]]; then
  JJ_PLAN_DIR="$repo_root/.jj-plan"
  JJ_PLAN_DIR_SOURCE=".jj-plan"
elif [[ -n "$repo_root" && -d "$repo_root/.jj-plans" ]]; then
  JJ_PLAN_DIR="$repo_root/.jj-plans"
  JJ_PLAN_DIR_SOURCE=".jj-plans (legacy)"
else
  JJ_PLAN_DIR_SOURCE="none"
  exec "$REAL_JJ" "$@"
fi

# Special handling for "abandon": protect stack bookmark from deletion
if [[ "$1" == "abandon" ]]; then
  # Check if --retain-bookmarks is in the args — if so, skip our handling
  local has_retain=false
  for arg in "$@"; do
    if [[ "$arg" == "--retain-bookmarks" ]]; then
      has_retain=true
      break
    fi
  done

  # Snapshot stack bookmark state before abandon
  # Combined query: bookmark info + current @ detection in one call
  local old_bm_info=""
  local old_bm_change=""
  local old_bm_name=""
  local old_first_child=""
  local old_was_at_wc=false
  if ! $has_retain; then
    old_bm_info="$("$REAL_JJ" log \
      -r 'heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)' \
      -T 'change_id.shortest(8) ++ " " ++ bookmarks.join(",") ++ " " ++ if(self.contained_in("@"), "C", "-") ++ "\n"' \
      --no-graph 2>/dev/null | grep .)"
    if [[ -n "$old_bm_info" ]]; then
      # Parse: "CHANGE_ID BOOKMARKS WC_FLAG"
      local -a bm_parts=("${(@s/ /)old_bm_info}")
      old_bm_change="${bm_parts[1]}"
      old_bm_name="${bm_parts[2]}"
      local wc_flag="${bm_parts[3]}"
      if [[ "$wc_flag" == "C" ]]; then
        old_was_at_wc=true
      fi
      # First child query — irreducible separate call (children() is revset-only)
      old_first_child="$("$REAL_JJ" log \
        -r "children($old_bm_change) ~ $old_bm_change" \
        -T 'change_id.shortest(8) ++ "\n"' \
        --no-graph --reversed 2>/dev/null | head -1)"
    fi
  fi

  # Flush and run the abandon
  __jj_plan_flush_all
  "$REAL_JJ" "$@"
  jj_exit=$?

  # If abandon succeeded and we had a stack bookmark, check if it survived
  if [[ $jj_exit -eq 0 && -n "$old_bm_info" ]]; then
    local bm_after
    bm_after="$("$REAL_JJ" log \
      -r 'heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)' \
      -T 'change_id.shortest(8)' \
      --no-graph 2>/dev/null | grep .)"

    if [[ -z "$bm_after" ]]; then
      # Bookmark was lost — attempt recovery
      local recovery_target=""

      # Try first child (survived rebase)
      if [[ -n "$old_first_child" ]]; then
        if "$REAL_JJ" log -r "$old_first_child" -T '' --no-graph 2>/dev/null; then
          recovery_target="$old_first_child"
        fi
      fi

      # If no child, but the abandoned change was @, jj created a new @ — use it
      if [[ -z "$recovery_target" ]] && $old_was_at_wc; then
        recovery_target="$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)"
      fi

      if [[ -n "$recovery_target" ]]; then
        "$REAL_JJ" bookmark set "$old_bm_name" -r "$recovery_target" -B 2>/dev/null
        echo "jj-plan: moved stack bookmark $old_bm_name to $recovery_target (abandoned change held it)" >&2
      else
        echo "jj-plan: WARNING: stack bookmark $old_bm_name was lost (abandoned change had no descendants). Run: jj bookmark set $old_bm_name -r <change>" >&2
      fi
    fi
  fi

  __jj_plan_sync
  exit $jj_exit
fi

# Special handling for "plan" subcommand: plan stack, plan new, plan config
if [[ "$1" == "plan" ]]; then
  # --help / -h check before dispatch
  if [[ "$2" == "--help" || "$2" == "-h" ]]; then
    cat <<'EOF'
jj plan — plan-oriented programming commands

Subcommands:
  stack [name] [-r REV]    Start a new named stack (creates change + bookmark)
  new [flags] [jj-new-args]
                           Create a plan change with a self-referencing placeholder
    --first                Insert before the first stack member (moves bookmark)
    --last                 Insert after the last stack member
  config                   Show resolved configuration and stack info

Options:
  --help, -h               Show this help message
EOF
    exit 0
  fi

  case "$2" in
    stack)
      # jj plan stack — start a new stack atomically (replaces jj stack new)
      shift 2
      local stack_rev=""
      local stack_name=""
      while [[ $# -gt 0 ]]; do
        case "$1" in
          -r)
            if [[ -z "$2" ]]; then
              echo "jj plan stack: -r requires a revision argument" >&2
              exit 1
            fi
            stack_rev="$2"
            shift 2
            ;;
          *)
            stack_name="$1"
            shift
            ;;
        esac
      done

      # Determine bookmark name
      local bookmark_name
      if [[ -n "$stack_name" ]]; then
        bookmark_name="stack/$stack_name"
      else
        bookmark_name="stack"
      fi

      # Flush pending edits before creating the new change
      __jj_plan_flush_all

      # Create a new change (optionally rooted at REV)
      if [[ -n "$stack_rev" ]]; then
        "$REAL_JJ" new -r "$stack_rev" 2>/dev/null
      else
        "$REAL_JJ" new 2>/dev/null
      fi
      local new_exit=$?
      if [[ $new_exit -ne 0 ]]; then
        echo "jj plan stack: failed to create new change" >&2
        exit $new_exit
      fi

      # Set the bookmark on the new change (-B allows backwards/sideways moves)
      local bm_err
      bm_err="$("$REAL_JJ" bookmark set "$bookmark_name" -r @ -B 2>&1)"
      local bm_exit=$?
      if [[ $bm_exit -ne 0 ]]; then
        # Bookmark set failed — roll back the jj new
        echo "$bm_err" >&2
        "$REAL_JJ" undo 2>/dev/null
        exit $bm_exit
      fi

      # Read back the new change's ID and set the placeholder description
      local new_change_id
      new_change_id="$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)"
      "$REAL_JJ" describe -m "(placeholder: jj:$new_change_id)" 2>/dev/null

      # Sync plans directory
      __jj_plan_sync

      echo "Started new stack: $bookmark_name ($new_change_id)"
      __jj_plan_show_stack
      exit 0
      ;;

    new)
      # jj plan new — create a change with a self-referencing placeholder description
      shift 2

      # Parse --first / --last flags; collect remaining args for jj new
      local plan_first=false plan_last=false
      local -a jj_args=()
      while [[ $# -gt 0 ]]; do
        case "$1" in
          --first) plan_first=true; shift ;;
          --last)  plan_last=true; shift ;;
          *)       jj_args+=("$1"); shift ;;
        esac
      done

      if $plan_first && $plan_last; then
        echo "jj plan new: --first and --last are mutually exclusive" >&2
        exit 1
      fi

      # Flush pending edits before creating the new change
      __jj_plan_flush_all

      if $plan_first || $plan_last; then
        # Resolve the stack to find boundary IDs
        local resolve_result
        resolve_result="$(__jj_plan_resolve_stack_base)"
        if [[ $? -ne 0 ]]; then
          echo "jj plan new: cannot resolve stack (no stack bookmark or trunk). Use 'jj plan stack' to start a stack first." >&2
          exit 1
        fi

        local range_mode="${resolve_result%%:*}"
        local stack_base="${resolve_result#*:}"
        local stack_revset
        if [[ "$range_mode" == "inclusive" ]]; then
          stack_revset="($stack_base::@) | descendants(@)"
        else
          stack_revset="($stack_base..@) | descendants(@)"
        fi

        if ! __jj_plan_batch_read "$stack_revset"; then
          echo "jj plan new: stack is empty" >&2
          exit 1
        fi
      fi

      if $plan_first; then
        local first_id="${_bp_ordered_ids[1]}"

        "$REAL_JJ" new --insert-before "$first_id" "${jj_args[@]}"
        local new_exit=$?
        if [[ $new_exit -ne 0 ]]; then
          echo "jj plan new: failed to create new change" >&2
          exit $new_exit
        fi

        # Read back the new change's ID and set the placeholder description
        local new_id
        new_id="$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)"
        "$REAL_JJ" describe -m "(placeholder: jj:$new_id)" 2>/dev/null

        # Move stack bookmark to the new change (it was on first_id)
        local bm_raw="${_bp_bm[$first_id]}"
        if [[ "$bm_raw" != "-" ]]; then
          # Find the first stack/stack/* bookmark in the comma-separated list
          local bm_name=""
          local IFS=','
          for bm in $bm_raw; do
            if [[ "$bm" == "stack" || "$bm" == stack/* ]]; then
              bm_name="$bm"
              break
            fi
          done
          unset IFS
          if [[ -n "$bm_name" ]]; then
            "$REAL_JJ" bookmark set "$bm_name" -r @ -B 2>/dev/null
          fi
        fi

      elif $plan_last; then
        local last_id="${_bp_ordered_ids[-1]}"

        "$REAL_JJ" new --insert-after "$last_id" "${jj_args[@]}"
        local new_exit=$?
        if [[ $new_exit -ne 0 ]]; then
          echo "jj plan new: failed to create new change" >&2
          exit $new_exit
        fi

        # Read back the new change's ID and set the placeholder description
        local new_id
        new_id="$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)"
        "$REAL_JJ" describe -m "(placeholder: jj:$new_id)" 2>/dev/null

      else
        # Default: plain jj new with forwarded args
        "$REAL_JJ" new "${jj_args[@]}"
        local new_exit=$?
        if [[ $new_exit -ne 0 ]]; then
          echo "jj plan new: failed to create new change" >&2
          exit $new_exit
        fi

        # Read back the new change's ID and set the placeholder description
        local new_id
        new_id="$("$REAL_JJ" log -r @ -T 'change_id.shortest(8)' --no-graph 2>/dev/null)"
        "$REAL_JJ" describe -m "(placeholder: jj:$new_id)" 2>/dev/null
      fi

      __jj_plan_sync
      echo "Created plan change: jj:$new_id"
      __jj_plan_show_stack
      exit 0
      ;;

    config)
      # jj plan config — show all configuration and resolved state (read-only)
      echo "jj-plan configuration:"
      echo ""
      echo "  shim path:        $SELF"
      echo "  real jj binary:   $REAL_JJ"
      echo "  repo root:        ${repo_root:-(not in a repo)}"
      echo ""
      if [[ "$JJ_PLAN_DIR_SOURCE" == "env var" ]]; then
        echo "  JJ_PLAN_DIR env:  $JJ_PLAN_DIR"
      else
        echo "  JJ_PLAN_DIR env:  (not set)"
      fi
      echo "  JJ_PLAN_MAX env:  ${JJ_PLAN_MAX}"
      echo ""
      echo "  resolved dir:     $JJ_PLAN_DIR"
      echo "  resolution source: $JJ_PLAN_DIR_SOURCE"
      echo ""

      # Stack info (read-only, no flush/sync)
      local resolve_result
      resolve_result="$(__jj_plan_resolve_stack_base 2>/dev/null)"
      if [[ $? -eq 0 ]]; then
        local range_mode="${resolve_result%%:*}"
        local stack_base="${resolve_result#*:}"
        echo "  stack base:       $stack_base ($range_mode)"

        local stack_revset
        if [[ "$range_mode" == "inclusive" ]]; then
          stack_revset="($stack_base::@) | descendants(@)"
        else
          stack_revset="($stack_base..@) | descendants(@)"
        fi
        local stack_size
        stack_size="$("$REAL_JJ" log -r "$stack_revset" -T '"x"' --no-graph 2>/dev/null | wc -c | tr -d ' ')"
        echo "  stack size:       $stack_size"
      else
        echo "  stack base:       (none)"
        echo "  stack size:       0"
      fi

      exit 0
      ;;

    "")
      echo "jj plan: missing subcommand. Run 'jj plan --help' for usage." >&2
      exit 1
      ;;
    *)
      echo "jj plan: unknown subcommand '$2'. Run 'jj plan --help' for usage." >&2
      exit 1
      ;;
  esac
fi

# Simple command paths: status/st, new/edit, and general catch-all
# All use the unified flush → command → sync → show handler.
if [[ "$1" == "status" || "$1" == "st" || "$1" == "new" || "$1" == "edit" ]]; then
  __jj_plan_wrap "$@"
fi

# General catch-all for all other mutating commands
__jj_plan_wrap "$@"