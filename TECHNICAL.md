# jj-plan Technical Reference

> Architecture and internals of the jj-plan Rust binary.

## Overview

jj-plan is a Rust binary installed as `jj` in `$PATH`, shadowing the real jj binary. It intercepts mutating commands to keep a `.jj-plan/` directory in sync with the current stack's change descriptions. Read-only commands (`log`, `diff`, `show`, etc.) pass through with zero overhead via Unix `exec`.

The binary resolves the real `jj` binary by walking `$PATH` and skipping itself (via `std::fs::canonicalize` comparison). All repository access uses `jj` subprocess calls — the binary does not link `jj-lib`.

## Project Structure

| Module | Lines | Responsibility |
|---|---|---|
| `src/main.rs` | 122 | Argument parsing, top-level dispatch, read-only passthrough |
| `src/jj_binary.rs` | 157 | Real jj binary resolution, `exec`/`run_inherit`/`run_silent` helpers |
| `src/plan_dir.rs` | 163 | Plan directory resolution (env → `.jj-plan/` → `.jj-plans/`) |
| `src/stack.rs` | 213 | Stack base resolution, `StackChange` struct, `batch_read_changes` |
| `src/sync.rs` | 580 | FC/IS sync: gather → plan → execute, `.stack` generation |
| `src/flush.rs` | 258 | FC/IS flush: gather → plan → execute, file→jj description sync |
| `src/wrap.rs` | 79 | Unified mutating command lifecycle: flush → command → sync → show |
| `src/markdown.rs` | 633 | `[scratch]` section stripping, code fence immunity, heading-level scoping |
| `src/template.rs` | 300 | Plan template resolution, `{{CHANGE_ID}}` interpolation |
| `src/plan_file.rs` | 311 | Plan file parsing, I/O helpers with error observability |
| `src/error.rs` | 37 | `JjPlanError` enum via `thiserror` |
| `src/commands/mod.rs` | 71 | `jj plan` subcommand dispatch |
| `src/commands/config.rs` | 67 | `jj plan config` — read-only introspection |
| `src/commands/help.rs` | 28 | `jj plan --help` text |
| `src/commands/stack.rs` | 140 | `jj plan stack` — atomic stack creation |
| `src/commands/new.rs` | 208 | `jj plan new` — plan change creation with `--first`/`--last` |
| `src/commands/done.rs` | 318 | `jj plan done` — completion marking, scratch stripping, advance |
| `src/commands/nav.rs` | 159 | `jj plan next`/`prev`/`go` — stack navigation |
| `src/commands/abandon.rs` | 250 | `jj abandon` — bookmark recovery handler |
| `src/commands/describe.rs` | 354 | `jj describe` — interception for `-m` mode |

Total: ~4,450 lines of Rust across 20 source files.

## Plan Directory Resolution

Resolution happens after `jj root` determines the repo root. Fallback chain:

1. **`JJ_PLAN_DIR` env var** — if set, used as-is (absolute or relative). No further fallback.
2. **`$repo_root/.jj-plan/`** — preferred default.
3. **`$repo_root/.jj-plans/`** — legacy fallback (silent, no warning).
4. **None found** — `exec` to real jj (full passthrough, not activated).

When `.jj-plan/` and `.jj-plans/` both exist, `.jj-plan/` wins.

Implementation: `src/plan_dir.rs` — `resolve_plan_dir()` returns `Option<PlanDir>` with `PlanDirSource` enum.

## Stack Base Resolution

The binary needs to know which changes belong to the current stack. Resolution via `resolve_stack_base()` in `src/stack.rs`:

1. **`stack` / `stack/*` bookmarks** — finds `heads((bookmarks(exact:"stack") | bookmarks(glob:"stack/*")) & ::@)`. If exactly one head: **inclusive** range (`base::@`). The bookmarked change IS the first stack member. If multiple equidistant heads: error (ambiguous siblings).
2. **`trunk()`** — if it resolves to something other than `root()`: **exclusive** range (`trunk()..@`). The trunk commit is NOT part of the stack.
3. **No usable base** — no sync occurs.

Both modes also include `descendants(@)` to capture changes ahead of the working copy.

Returns `StackBase` enum: `Inclusive(change_id)`, `Exclusive`, or `Ambiguous(Vec<change_id>)`.

## Command Dispatch

```
jj <subcommand> [args...]
│
├─ no subcommand or read-only? ──→ exec $REAL_JJ (zero overhead)
├─ no repo root or no plan dir? ─→ exec $REAL_JJ (not activated)
├─ "plan" ────────────────────────→ --help/-h check, then subcommand dispatch:
│   ├─ "plan --help" ─────────────→ print help summary, exit 0
│   ├─ "plan stack" ──────────────→ atomic stack creation (templated description)
│   ├─ "plan new" ────────────────→ templated plan change creation
│   │     ├─ --first ─────────────→ insert before first stack member (moves bookmark)
│   │     └─ --last ──────────────→ insert after last stack member
│   ├─ "plan done" ───────────────→ mark done, strip [scratch], advance
│   │     ├─ --stack ─────────────→ mark all plans done
│   │     ├─ --keep-scratch ──────→ skip [scratch] stripping
│   │     └─ --dry-run ───────────→ preview what would change
│   ├─ "plan next" ───────────────→ advance @ to next plan in stack
│   ├─ "plan prev" ───────────────→ move @ to previous plan in stack
│   ├─ "plan go" ─────────────────→ jump to plan by index or change ID
│   ├─ "plan config" ─────────────→ read-only introspection
│   └─ anything else ─────────────→ usage error (suggests --help)
├─ "abandon" ─────────────────────→ bookmark recovery handler
├─ "describe" ────────────────────→ -m interception (write to plan file first)
├─ "status/st/new/edit" ──────────→ wrap::wrap()
└─ everything else ───────────────→ wrap::wrap() (catch-all)
```

Read-only commands are listed in `READONLY_COMMANDS` in `src/main.rs`. Note that `status`/`st` are NOT in that list — they get the flush→sync→show treatment to display the stack.

## Flush/Sync Lifecycle

All mutating commands go through `wrap::wrap()` in `src/wrap.rs`:

```
flush_all()        ← files → jj (before the command)
jj.run_inherit()   ← the actual jj command
sync()             ← jj → files (after the command)
show_stack()       ← display .stack to stdout
```

**Ordering is critical**: flush before command ensures user edits are written to jj descriptions before the command modifies state. Sync after command ensures files reflect the new jj state.

### Flush (`src/flush.rs`)

Structured as FC/IS (Functional Core / Imperative Shell):

1. **Gather** (`gather_flush_state`): Reads plan file contents from disk via `plan_file::plan_files_by_id()`, batch-reads jj descriptions via `batch_read_by_ids()`.
2. **Plan** (`plan_flush`): Pure function — compares file contents against jj descriptions, produces `Vec<FlushAction>` for changes that differ.
3. **Execute** (`execute_flush`): Shells out to `jj describe -r CHANGEID -m CONTENT` for each action.

Skips flushing when in error state (`current.md` → `error.md`).

### Sync (`src/sync.rs`)

Also FC/IS:

1. **Gather** (`gather_current_state`): Reads the plan directory once, builds a map of existing `NN-CHANGEID.md` files.
2. **Plan** (`plan_sync`): Pure function — given current files and stack changes from jj, computes:
   - Files to remove (changes no longer in stack)
   - Files to rename (reindexing after stack reorder)
   - Files to write (description content)
   - Symlink target for `current.md`
   - `.stack` summary content
   - Error/warning conditions
3. **Execute** (`execute_sync`): Applies the plan — removes, renames, writes files, updates symlink.

## Repository Access

A single `jj log` call using a custom template with RS (`\x1e`) field separators and NUL (`\0`) record separators. Parsed in Rust with type-safe `StackChange` structs (`src/stack.rs`):

```rust
pub struct StackChange {
    pub change_id: String,      // shortest(8) prefix
    pub description: String,    // full description text
    pub is_empty: bool,         // no file changes
    pub is_working_copy: bool,  // is @
    pub bookmarks: Vec<String>, // bookmark names
}
```

The template format:
```
change_id.shortest(8) RS bookmarks.join(",") RS empty_flag RS wc_flag RS description NUL
```

`batch_read_changes()` handles the parsing — splitting by NUL for records, RS for fields, and rejoining if the description contained RS characters.

## Markdown Processing (`src/markdown.rs`)

The `strip_scratch_sections()` function implements a line-oriented state machine:

- **Heading detection**: ATX headings (`# ` through `###### `) parsed for level (1-6).
- **`[scratch]` detection**: Case-insensitive match on `[scratch]` anywhere in the heading line.
- **Scope**: A `[scratch]` heading strips all content until a heading of the **same or higher level** (≤ N `#` marks) or end of document.
- **Code fence immunity**: Lines inside ``` or ~~~ fences are never treated as headings. Fence closer must match opener character and have ≥ opener count. Backtick info strings are handled correctly.

20 unit tests cover edge cases including: nested headings, code fences with info strings, mixed-case `[scratch]`, multiple scratch sections, adjacent headings, and fence character mismatch.

## Plan Templates (`src/template.rs`)

Template resolution chain:

1. **`JJ_PLAN_TEMPLATE` env var** → read file at that path
2. **`.jj-plan/template.md`** → read file if it exists
3. **Built-in default** (embedded in binary as `DEFAULT_TEMPLATE`)

The built-in default template:
```
(plan: jj:{{CHANGE_ID}})

## Background


## Approach


## Tasks

- [ ]

## Scratchpad [scratch]

```

`apply_template()` replaces `{{CHANGE_ID}}` with the actual change ID. If a custom template has no `{{CHANGE_ID}}` placeholder, a self-referencing HTML comment `<!-- jj:CHANGE_ID -->` is injected as the second line.

14 unit tests cover template resolution, interpolation, fallback chain, and injection.

## Describe Interception (`src/commands/describe.rs`)

When `jj describe -m "..."` is invoked:

1. Parse args: extract all `-m`/`--message` values and `-r`/`--revision` target.
2. If no `-m`/`--message` found → editor mode, pass through to `wrap::wrap()`.
3. Concatenate multiple `-m` values with newlines (matching jj behavior).
4. Resolve target change ID (defaults to `@`).
5. Find the matching plan file by change ID prefix match.
6. Write the message to the plan file.
7. Pass through to `wrap::wrap()`: flush picks up the file write, jj describe sets the same content (idempotent), sync reads jj back to files.

This eliminates the "NEVER call `jj describe` directly" rule — the binary handles it transparently.

17 unit tests cover arg parsing for all `-m`/`-r` variants.

## Stack Navigation (`src/commands/nav.rs`)

Three commands, all following the same lifecycle: flush → resolve stack → navigate → sync → show.

- **`jj plan next`**: Find `@` position in stack. If last → print "Already at the last plan" and stay put. Otherwise → `jj edit -r $next_id`.
- **`jj plan prev`**: Find `@` position in stack. If first → print "Already at the first plan" and stay put. Otherwise → `jj edit -r $prev_id`.
- **`jj plan go TARGET`**: Parse target as 1-based index (validates range) or change ID (pass through). → `jj edit -r $resolved_id`.

Shared helper `resolve_stack_and_position()` returns `(Vec<StackChange>, current_index)`.

## `jj plan stack` (`src/commands/stack.rs`)

Atomic stack creation:

1. Parse args: `-r REV` (optional root revision), positional name (optional).
2. Determine bookmark name: `stack/$name` or bare `stack`.
3. `flush_all()` — flush pending edits.
4. `jj new [-r REV]` — create the change.
5. `jj bookmark set $bookmark_name -r @ -B` — set bookmark (allows backwards with `-B`).
6. On bookmark failure: `jj undo` to roll back.
7. Read back change ID via `jj log -r @ -T change_id.shortest(8)`.
8. Set templated description via `template::render_template()`.
9. `sync()` + `show_stack()`.

## `jj plan new` (`src/commands/new.rs`)

Creates a change with a templated description:

1. Parse args: strip `--first` and `--last` (shim flags); detect explicit positioning flags; collect remaining args for `jj new`.
2. `flush_all()` — flush pending edits.
3. Create the change (varies by flag).
4. Read back the new change's ID.
5. Set templated description via `template::render_template()`.
6. `sync()` + `show_stack()`.

### Default (no flags)

`jj new --insert-after @` preserves stack linearity. Suppressed if user provides explicit positioning flags.

### `--first`

Insert before the first stack member. Moves the `stack`/`stack/*` bookmark to the new change.

### `--last`

Insert after the last stack member.

## `jj plan done` (`src/commands/done.rs`)

Mark one or all plans as done:

1. `flush_all()`.
2. Resolve stack.
3. For each target change:
   - Strip `[scratch]` sections (unless `--keep-scratch`).
   - Append `plan-status: ✅` (unless already present — idempotent).
   - `jj describe -r CHANGEID -m $cleaned_description`.
4. If targeting working copy (default): advance to next undone plan.
5. `sync()` + `show_stack()`.

Flags: `--stack` (all plans), `--keep-scratch`, `--dry-run`, positional `CHANGE_ID`.

## `jj plan config` (`src/commands/config.rs`)

Read-only introspection — no flush, no sync. Prints:
- shim path, real jj binary, repo root
- JJ_PLAN_DIR env, JJ_PLAN_MAX env
- resolved dir, resolution source
- stack base (with range mode), stack size

## Abandon Recovery (`src/commands/abandon.rs`)

Protects `stack`/`stack/*` bookmarks from accidental deletion:

1. **Before abandon**: Snapshot bookmark state — which change holds it, whether it's `@`, its first child.
2. **Run `jj abandon`** with all original args.
3. **After abandon**: Check if the bookmark survived via `stack_bookmark_survives()`. If lost:
   - Try the first child of the old bookmarked change.
   - If no child but the abandoned change was `@`, use the new `@`.
   - If recovery target found: `jj bookmark set $name -r $target -B`.
   - Otherwise: emit WARNING with manual instructions.

`--retain-bookmarks` bypasses this handler entirely.

## File Layout

```
.jj-plan/
  01-kpqxywon.md    # Plan file for first stack member
  02-mtzrlpvq.md    # Plan file for second stack member
  03-ykvsnxrl.md    # Plan file for third stack member
  current.md        # Symlink → active change's plan file
  .stack            # One-line-per-change summary (for display)
  error.md          # Transient: exists only during error state
  template.md       # Optional: custom plan template (overrides built-in default)
```

- **`NN-CHANGEID.md`**: sort index + shortest unique change ID. Index `01` is closest to the stack base.
- **`current.md`**: always a symlink. Points to `error.md` during error state.
- **`.stack`**: regenerated on every sync. Status markers: `*` = working copy, `✓` = done, `~` = has file changes.
- **`error.md`**: created when stack exceeds `JJ_PLAN_MAX` or ambiguous bookmarks. Auto-cleared when condition resolves.
- **`template.md`**: optional custom template with `{{CHANGE_ID}}` interpolation.

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DIR` | Override plan directory path | Auto-resolved (see above) |
| `JJ_PLAN_MAX` | Max changes in stack before refusing to sync | `50` |
| `JJ_PLAN_TEMPLATE` | Override plan template file path | `.jj-plan/template.md` → built-in default |

## Testing

Two complementary test suites:

### Bats (behavioral/acceptance)

`bats jj-plan.bats` — 137 tests validating the installed binary from the outside:
- `run_in_repo`: creates a temp jj repo with `stack` bookmark and `.jj-plan/` activated
- Tests call the shim via `$PATH` and assert on stdout/stderr and file state
- `REAL_JJ` (`/opt/homebrew/bin/jj`) bypasses the shim for direct jj calls

### Cargo (unit tests)

`cargo test` — 86 unit tests for pure functions:
- `markdown`: 20 tests (scratch stripping, code fence immunity, edge cases)
- `template`: 14 tests (resolve chain, interpolation, default structure)
- `commands/describe`: 17 tests (arg parsing for -m/-r variants)
- `sync`: 11 tests (plan_sync pure logic)
- `flush`: 5 tests (plan_flush pure logic)
- `plan_file`: 13 tests (filename parsing, I/O helpers)
- `plan_dir`: 6 tests (resolution chain)

The FC/IS pattern ensures all business logic is unit-testable without subprocess overhead.