# jj-plan Technical Reference

> Architecture and internals of the jj-plan Rust binary.

## Overview

jj-plan is a Rust binary installed as `jj` in `$PATH`, shadowing the real jj binary. It intercepts mutating commands to keep a `.jj-plan/` directory in sync with the current stack's change descriptions. Read-only commands (`log`, `diff`, `show`, etc.) pass through with zero overhead via Unix `exec`.

The binary resolves the real `jj` binary by walking `$PATH` and skipping itself (via `std::fs::canonicalize` comparison). Repository reads (stack resolution, commit metadata, bookmark enumeration) use **jj-lib** for in-process access (~1ms startup, sub-millisecond reads). Mutations (`jj describe`, `jj new`, `jj edit`, `jj abandon`, `jj bookmark set`) use CLI subprocess calls because the CLI handles working copy snapshotting, auto-rebase, and user-facing output. Repo root discovery uses a filesystem walk for `.jj/` (no subprocess).

If jj-lib cannot load the repository (version mismatch, corrupt state), the binary degrades gracefully to exec passthrough — the jj command runs directly without plan sync features.

## Project Structure

| Module | Lines | Responsibility |
|---|---|---|
| `src/main.rs` | 127 | Argument parsing, top-level dispatch, read-only passthrough, jj-lib repo loading |
| `src/jj_binary.rs` | 144 | Real jj binary resolution, `exec`/`run_inherit`/`run_silent` helpers |
| `src/plan_dir.rs` | 208 | Repo root discovery, plan directory resolution (env → `.jj-plan/` → `.jj-plans/`) |
| `src/repo.rs` | 621 | In-process repository access via jj-lib: workspace loading, revset evaluation, commit reads, bookmark enumeration |
| `src/stack.rs` | 52 | Shared domain types: `StackBase` enum, `StackChange` struct |
| `src/sync.rs` | 577 | FC/IS sync: gather → plan → execute, `.stack` generation |
| `src/flush.rs` | 219 | FC/IS flush: gather (jj-lib) → plan → execute (`jj describe` subprocess) |
| `src/wrap.rs` | 95 | Unified mutating command lifecycle: flush → command → reload → resolve_and_sync |
| `src/markdown.rs` | 633 | `[scratch]` section stripping, code fence immunity, heading-level scoping |
| `src/template.rs` | 297 | Plan template resolution, `{{CHANGE_ID}}` interpolation |
| `src/plan_file.rs` | 310 | Plan file parsing, I/O helpers with error observability |
| `src/error.rs` | 37 | `JjPlanError` enum via `thiserror` |
| `src/commands/mod.rs` | 73 | `jj plan` subcommand dispatch |
| `src/commands/config.rs` | 68 | `jj plan config` — read-only introspection (jj-lib reads) |
| `src/commands/help.rs` | 28 | `jj plan --help` text |
| `src/commands/stack.rs` | 128 | `jj plan stack` — atomic stack creation |
| `src/commands/new.rs` | 189 | `jj plan new` — plan change creation with `--first`/`--last` |
| `src/commands/done.rs` | 303 | `jj plan done` — completion marking, scratch stripping, advance |
| `src/commands/nav.rs` | 159 | `jj plan next`/`prev`/`go` — stack navigation |
| `src/commands/abandon.rs` | 121 | `jj abandon` — bookmark recovery handler (jj-lib reads, subprocess writes) |
| `src/commands/describe.rs` | 334 | `jj describe` — interception for `-m` mode |

Total: ~4,723 lines of Rust across 21 source files.

## Repo Root and Plan Directory Resolution

### Repo Root Discovery

The repo root is discovered by walking up from `std::env::current_dir()` looking for a `.jj/` directory, via `find_repo_root()` in `src/plan_dir.rs`. This mirrors the logic in jj's own CLI (`cli/src/cli_util.rs::find_workspace_dir()`). It replaces the previous approach of shelling out to `jj root` (~15ms subprocess overhead eliminated).

If no `.jj/` directory is found in any ancestor, the command is passed through to the real `jj` binary (which will produce its own "not in a repo" error).

### Plan Directory Resolution

Fallback chain (after repo root is known):

1. **`JJ_PLAN_DIR` env var** — if set, used as-is (absolute or relative). No further fallback.
2. **`$repo_root/.jj-plan/`** — preferred default.
3. **`$repo_root/.jj-plans/`** — legacy fallback (silent, no warning).
4. **None found** — `exec` to real jj (full passthrough, not activated).

When `.jj-plan/` and `.jj-plans/` both exist, `.jj-plan/` wins.

Implementation: `src/plan_dir.rs` — `find_repo_root()` returns `Option<PathBuf>`, `resolve_plan_dir()` returns `Option<PlanDir>` with `PlanDirSource` enum.

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
flush_all()          ← files → jj (before the command; reads via jj-lib, writes via subprocess)
jj.run_inherit()     ← the actual jj command (subprocess)
loaded_repo.reload() ← refresh jj-lib snapshot after the mutation (~0.2ms)
resolve_and_sync()   ← re-resolve stack via jj-lib, sync files, show summary
```

**Ordering is critical**: flush before command ensures user edits are written to jj descriptions before the command modifies state. After the subprocess command mutates the repository, `reload()` refreshes the in-process `ReadonlyRepo` snapshot so that `resolve_and_sync()` sees the new state via jj-lib.

### resolve_and_sync (`src/wrap.rs`)

`resolve_and_sync()` is the canonical post-mutation sync path. It:

1. Resolves the stack base via jj-lib (`repo::resolve_stack_base_lib`)
2. Resolves stack changes via jj-lib (`repo::resolve_stack_changes_lib`)
3. Handles ambiguous bookmarks (sets error state)
4. Calls `sync()` to update plan files
5. Calls `show_stack()` to display the summary

All command modules (`nav.rs`, `done.rs`, `new.rs`, `stack.rs`, `abandon.rs`) use this single function instead of maintaining their own sync helpers. All reads are in-process via jj-lib — no subprocess calls.

### Flush (`src/flush.rs`)

Structured as FC/IS (Functional Core / Imperative Shell):

1. **Gather** (`gather_flush_state`): Reads plan file contents from disk via `plan_file::plan_files_by_id()`, batch-reads jj descriptions via jj-lib (`repo::gather_descriptions()`).
2. **Plan** (`plan_flush`): Pure function — compares file contents against jj descriptions, produces `Vec<FlushAction>` for changes that differ.
3. **Execute** (`execute_flush`): Shells out to `jj describe -r CHANGEID -m CONTENT` for each action. This is the only subprocess usage in flush — all reads are jj-lib.

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

All repository reads use **jj-lib** for in-process access (`src/repo.rs`). The workspace and repository are loaded once at startup via `load_repo()` (~1ms), then refreshed after each CLI mutation via `LoadedRepo::reload()` (~0.2ms, calls `ReadonlyRepo::reload_at_head()`).

### LoadedRepo lifecycle

```
main.rs: load_repo()     → LoadedRepo { workspace, repo }    (~1ms)
wrap.rs: flush_all()     → reads via jj-lib (repo is fresh)
         jj command      → subprocess mutation
         repo.reload()   → refresh ReadonlyRepo snapshot      (~0.2ms)
         resolve_and_sync() → reads via jj-lib (repo is fresh)
```

Commands that read stack state after `flush_all()` (which may call `jj describe`) must call `loaded_repo.reload()` before their reads to pick up flush mutations.

### Read functions (`src/repo.rs`)

| Function | Purpose |
|---|---|
| `resolve_stack_base_lib()` | Find nearest `stack`/`stack/*` bookmark or `trunk()` fallback |
| `resolve_stack_changes_lib()` | Evaluate stack revset, return ordered `Vec<StackChange>` |
| `batch_read_changes_lib()` | Evaluate arbitrary revset, return `Vec<StackChange>` |
| `gather_descriptions()` | Read descriptions for plan file comparison (flush) |
| `read_change_id_at_wc()` | Working copy's shortest change ID |
| `read_description_at()` | Single change description by revset target |
| `resolve_change_id()` | Resolve revset to shortest change ID |
| `snapshot_bookmark_state()` | Pre-abandon bookmark snapshot for recovery |
| `stack_bookmark_survives()` | Post-abandon bookmark existence check |
| `commit_exists()` | Check if a revset target resolves |
| `first_child_change_id()` | First child of a change (for abandon recovery) |

All functions use jj-lib's revset engine (`revset::parse()` → `resolve_user_expression()` → `evaluate()`) and commit API (`Commit::description()`, `Commit::change_id()`, `Commit::parent_tree()`).

### Domain types (`src/stack.rs`)

```rust
pub struct StackChange {
    pub change_id: String,      // shortest(8) prefix, reverse hex (k-z alphabet)
    pub description: String,    // full description text
    pub is_empty: bool,         // no file changes
    pub is_working_copy: bool,  // is @
    pub bookmarks: Vec<String>, // bookmark names
}
```

Change IDs use jj's **reverse hex encoding** (`zyxwvutsrqponmlk` alphabet, not standard hex `0123456789abcdef`). This is critical: standard hex overlaps with commit ID prefixes and causes revset resolution failures. Use `jj_lib::hex_util::encode_reverse_hex()`.

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

## Performance

The shim adds overhead to every mutating command due to the flush→command→reload→sync lifecycle. Key costs:

| Operation | Cost | Notes |
|---|---|---|
| Repo root discovery | ~0ms | Filesystem walk for `.jj/` |
| `load_repo()` (startup) | ~1ms | jj-lib workspace + repo load (once per invocation) |
| `reload_at_head()` | ~0.2ms | Refresh repo snapshot after CLI mutation |
| jj-lib revset evaluation | <1ms | Stack resolution, commit reads, bookmark enumeration |
| `jj describe` (flush write) | ~21ms | Per changed file (subprocess) |
| `jj edit` / `jj new` | ~60ms | CLI mutation subprocess |

### Subprocess call model

All repository reads are in-process via jj-lib. Only mutations use subprocess calls. The subprocess count depends on the command:

| Command | Subprocess calls | Notes |
|---|---|---|
| `wrap` (e.g. `jj status`) | 1 (the command itself) | Flush reads + sync reads are jj-lib |
| `jj plan next/prev` | 1 (`jj edit`) | Flush reads + stack resolution are jj-lib |
| `jj plan done` (single) | 1-2 (`jj describe` + optional `jj edit` for advance) | Stack resolution via jj-lib |
| `jj plan done --stack` | N (`jj describe` × N changes) | One subprocess per change |
| `jj plan new` | 1-2 (`jj new` + `jj describe`) | Plus `jj bookmark set` if `--first` |
| `jj plan stack` | 3 (`jj new` + `jj bookmark set` + `jj describe`) | Atomic stack creation |

Flush adds 1 `jj describe` subprocess per changed plan file (only when file content differs from jj description).

### Measured overhead

On a typical repo (this project, ~20 commits in history, warm cache, `/usr/bin/time`, best of 5):

| Metric | Value |
|---|---|
| Raw `jj status` | ~10ms |
| Shimmed `jj status` | ~55ms |
| **Shim overhead** | **~45ms** |
| Shimmed `jj plan next` | ~95ms |
| Shimmed `jj plan done --dry-run` | <10ms |

The ~45ms shim overhead breaks down as:
- `load_repo()`: ~1ms
- Flush (jj-lib reads): <1ms
- `jj status` subprocess: ~45ms (includes process spawn + jj startup)
- `reload_at_head()`: ~0.2ms
- `resolve_and_sync` (jj-lib reads): <1ms
- Binary startup / dynamic linking: ~5ms

The jj-plan read overhead above the single subprocess call is **~2-3ms**.

Previous overhead (before jj-lib, subprocess-only reads): ~115ms. The jj-lib migration reduced shim overhead by **~60%**.

## Testing

Two complementary test suites:

### Bats (behavioral/acceptance)

`bats jj-plan.bats` — 138 tests validating the installed binary from the outside:
- **Template repo**: `setup_file()` pre-creates a jj repo with `stack` bookmark + `.jj-plan/`. Each test gets an isolated copy via `cp -r` (~2ms vs ~100ms for `jj git init`).
- **Per-test isolation**: `setup()` copies the template and `cd`s into it. `teardown()` removes it. No shared mutable state between tests.
- **Direct bats style**: Tests run commands inline (no `run_in_repo` wrapper, no `zsh -c` subprocess). Assertions use direct value checks (`[[ "$(cat file)" == "..." ]]`) instead of echo/grep patterns.
- **Parallel support**: `bats jj-plan.bats --jobs 8` runs in ~31s (vs ~54s sequential, ~64s before this rewrite). Requires GNU `parallel` (`brew install parallel`).
- `REAL_JJ` (`/opt/homebrew/bin/jj`) bypasses the shim for direct jj calls and repo setup.
- `$SHIM_DIR` is set once in `setup_file()` and added to `$PATH` globally — no test uses `$HOME/.local/bin`.

### Cargo (unit tests)

`cargo test` — 87 unit tests for pure functions:
- `markdown`: 20 tests (scratch stripping, code fence immunity, edge cases)
- `template`: 14 tests (resolve chain, interpolation, default structure)
- `commands/describe`: 17 tests (arg parsing for -m/-r variants)
- `sync`: 11 tests (plan_sync pure logic)
- `flush`: 4 tests (plan_flush pure logic)
- `plan_file`: 13 tests (filename parsing, I/O helpers)
- `plan_dir`: 8 tests (resolution chain, repo root discovery)

The FC/IS pattern ensures all business logic is unit-testable without subprocess overhead.