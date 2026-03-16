# jj-plan Technical Reference

> Architecture and internals of the jj-plan shim.

## Overview

jj-plan is a zsh shim installed as `jj` in `$PATH`, shadowing the real jj binary. It intercepts mutating commands to keep a `.jj-plan/` directory in sync with the current stack's change descriptions. Read-only commands (`log`, `diff`, `show`, etc.) pass through with zero overhead via `exec`.

The shim resolves the real `jj` binary by walking `$path` and skipping itself (via `realpath` comparison).

## Plan Directory Resolution

Resolution happens after `jj root` determines the repo root. Fallback chain:

1. **`JJ_PLAN_DIR` env var** — if set, used as-is (absolute or relative). No further fallback.
2. **`$repo_root/.jj-plan/`** — preferred default.
3. **`$repo_root/.jj-plans/`** — legacy fallback (silent, no warning).
4. **None found** — `exec "$REAL_JJ" "$@"` (full passthrough, not activated).

When `.jj-plan/` and `.jj-plans/` both exist, `.jj-plan/` wins.

## Stack Base Resolution

The shim needs to know which changes belong to the current stack. Resolution via `__jj_plan_resolve_stack_base`:

1. **`stack` / `stack/*` bookmarks** — finds `heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)`. If exactly one head: **inclusive** range (`base::@`). The bookmarked change IS the first stack member. If multiple equidistant heads: error (ambiguous siblings).
2. **`trunk()`** — if it resolves to something other than `root()`: **exclusive** range (`trunk()..@`). The trunk commit is NOT part of the stack.
3. **No usable base** — no sync occurs.

Both modes also include `descendants(@)` to capture changes ahead of the working copy.

## Command Dispatch

```
jj <subcommand> [args...]
│
├─ no subcommand or read-only? ──→ exec $REAL_JJ (zero overhead)
├─ no repo root or no plan dir? ─→ exec $REAL_JJ (not activated)
├─ "plan" ────────────────────────→ --help/-h check, then case dispatch:
│   ├─ "plan --help" ─────────────→ print help summary, exit 0
│   ├─ "plan stack" ──────────────→ atomic stack creation
│   ├─ "plan new" ────────────────→ placeholder plan change
│   │     ├─ --first ─────────────→ insert before first stack member (moves bookmark)
│   │     └─ --last ──────────────→ insert after last stack member
│   ├─ "plan config" ─────────────→ read-only introspection
│   └─ anything else ─────────────→ usage error (suggests --help)
├─ "abandon" ─────────────────────→ bookmark recovery handler
├─ "status/st/new/edit" ──────────→ __jj_plan_wrap
└─ everything else ───────────────→ __jj_plan_wrap (catch-all)
```

Read-only commands are listed in `__jj_plan_readonly_commands`. Note that `status`/`st` are NOT in that list — they get the flush→sync→show treatment to display the stack.

## Flush/Sync Lifecycle

All mutating commands go through `__jj_plan_wrap`:

```
__jj_plan_flush_all    ← files → jj (before the command)
"$REAL_JJ" "$@"        ← the actual jj command
__jj_plan_sync         ← jj → files (after the command)
__jj_plan_show_stack   ← display .stack to stdout
```

**Ordering is critical**: flush before command ensures user edits are written to jj descriptions before the command modifies state. Sync after command ensures files reflect the new jj state.

### `__jj_plan_flush_all`

Collects change IDs from plan filenames (`NN-CHANGEID.md`), batch-reads their current jj descriptions via `__jj_plan_batch_read`, and calls `jj describe` only for files that actually differ from the jj description. Skips flushing when in error state (`current.md` → `error.md`).

### `__jj_plan_sync`

jj is authoritative after this runs. Steps:

1. Resolve stack base (see above).
2. Batch-read all changes in the stack range.
3. Check stack size against `$JJ_PLAN_MAX`; set error state if exceeded.
4. Remove files for changes no longer in the stack.
5. Write/rename files to match the stack order (`01-ID.md`, `02-ID.md`, ...).
6. Update `current.md` symlink to point to the working copy's file.
7. Generate `.stack` summary.

### `__jj_plan_batch_read`

Single `jj log` call using a custom template with RS (`\x1e`) field separators and NUL (`\0`) record separators. Populates associative arrays:

- `_bp_desc[ID]` — full description
- `_bp_empty[ID]` — `"E"` (empty) or `"F"` (has file changes)
- `_bp_wc[ID]` — `"C"` (is working copy) or `"-"`
- `_bp_bm[ID]` — bookmark names or `"-"`
- `_bp_ordered_ids` — ordered array of change IDs

## `jj plan stack`

Atomic stack creation (replaces the old `jj stack new`):

1. Parse args: `-r REV` (optional root revision), positional name (optional).
2. Determine bookmark name: `stack/$name` or bare `stack`.
3. `__jj_plan_flush_all` — flush pending edits.
4. `jj new [-r REV]` — create the change.
5. `jj bookmark set $bookmark_name -r @ -B` — set bookmark (allows backwards/sideways with `-B`).
6. On bookmark failure: `jj undo` to roll back the `jj new`.
7. `__jj_plan_sync` — populates batch-read data.
8. Derive change ID from `_bp_wc` (avoids an extra `jj log` call).

## `jj plan new`

Creates a change with a self-referencing placeholder description:

1. Parse args: strip `--first` and `--last` (shim flags); collect remaining args for `jj new`.
2. `__jj_plan_flush_all` — flush pending edits.
3. Create the change (varies by flag — see below).
4. Read back the new change's ID via `jj log -r @ -T 'change_id.shortest(8)'`.
5. `jj describe -m "(placeholder: jj:$id)"` — set the placeholder.
6. `__jj_plan_sync` + display.

The placeholder description serves two purposes:
- **GC protection**: jj garbage-collects chains of empty-description changes. The placeholder is non-empty.
- **Self-referencing link**: the `jj:CHANGE_ID` is immediately usable in code comments, even before the real plan is written.

### Default (no flags)

Runs `jj new` with all remaining args forwarded (e.g. `-r @-`, `--insert-before X`).

### `--first`

Inserts before the first stack member:

1. Resolve stack via `__jj_plan_resolve_stack_base` + `__jj_plan_batch_read`. Error if no stack.
2. Extract `first_id` from `_bp_ordered_ids[1]`.
3. `jj new --insert-before $first_id` — rebases the old first member onto the new change.
4. Move the stack bookmark: parse `_bp_bm[$first_id]` for the first `stack`/`stack/*` bookmark, then `jj bookmark set $bm_name -r @ -B`. This is necessary because the bookmark was on the old first member — without moving it, the new change would fall outside the stack range.

### `--last`

Inserts after the last stack member:

1. Resolve stack via `__jj_plan_resolve_stack_base` + `__jj_plan_batch_read`. Error if no stack.
2. Extract `last_id` from `_bp_ordered_ids[-1]` (zsh last element).
3. `jj new --insert-after $last_id` — rebases the old last member's children onto the new change.

When `@` is already the tip of the stack, this is equivalent to plain `jj new`, but uses the explicit `--insert-after` form for consistency.

### Flag validation

`--first` and `--last` are mutually exclusive. If both are specified, the shim exits with an error.

## `jj plan config`

Read-only introspection command — no flush, no sync, no side effects. Prints all resolved configuration:

- **shim path**: the realpath of the shim script (`$SELF`)
- **real jj binary**: the resolved path to the real `jj` (`$REAL_JJ`)
- **repo root**: from `jj root`
- **JJ_PLAN_DIR env**: the raw env var value, or "(not set)"
- **JJ_PLAN_MAX env**: the effective max stack size
- **resolved dir**: the actual plan directory path being used
- **resolution source**: one of `env var`, `.jj-plan`, `.jj-plans (legacy)`, or `none`
- **stack base**: the resolved bookmark or `trunk()`, with range mode (inclusive/exclusive), or "(none)"
- **stack size**: number of changes in the current stack

## Abandon Recovery

The `abandon` handler protects `stack`/`stack/*` bookmarks from accidental deletion:

1. **Before abandon**: snapshot bookmark state — which change holds it, whether it's `@`, its first child.
2. **Run `jj abandon`**.
3. **After abandon**: check if the bookmark survived. If lost:
   - Try the first child of the old bookmarked change (it survived rebase).
   - If no child but the abandoned change was `@`, use the new `@` (jj creates one).
   - If recovery target found: `jj bookmark set $name -r $target -B`.
   - Otherwise: emit a WARNING with instructions to manually set the bookmark.

The `--retain-bookmarks` flag bypasses this handler entirely.

**Bookmark-loss detection** also fires in `__jj_plan_sync`: if stack base resolution fails but plan files exist, a WARNING is emitted.

## File Layout

```
.jj-plan/
  01-kpqxywon.md    # Plan file for first stack member
  02-mtzrlpvq.md    # Plan file for second stack member
  03-ykvsnxrl.md    # Plan file for third stack member
  current.md        # Symlink → active change's plan file (e.g. 02-mtzrlpvq.md)
  .stack            # One-line-per-change summary (for display)
  error.md          # Transient: exists only during error state
```

- **`NN-CHANGEID.md`**: sort index + shortest unique change ID. Index `01` is closest to the stack base.
- **`current.md`**: always a symlink. Points to `error.md` during error state. Editing this file edits the active change's plan.
- **`.stack`**: regenerated on every sync. Format: `{here} {status} {NN}-{ID} :: {first_line}`. Status markers: `*` = working copy, `✓` = `plan-status: ✅` in description, `~` = has file changes.
- **`error.md`**: created when the stack exceeds `JJ_PLAN_MAX` or when ambiguous bookmarks are detected. Auto-cleared when the condition resolves.

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DIR` | Override plan directory path | Auto-resolved (see above) |
| `JJ_PLAN_MAX` | Max changes in stack before refusing to sync | `50` |

## Testing

Tests use [bats](https://github.com/bats-core/bats-core) (`jj-plan.bats`). Two primary helpers:

- **`run_in_repo`**: creates a temp jj repo with `stack` bookmark and `.jj-plan/` activated. Runs a zsh script.
- **`run_in_repo_with_max`**: same, but sets `JJ_PLAN_MAX` to a custom value.

Tests call the shim (via `$PATH`) and assert on stdout/stderr output and file state. The real jj binary path is hardcoded in `REAL_JJ` for direct jj calls within tests (bypassing the shim).