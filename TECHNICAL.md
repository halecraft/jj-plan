# jj-plan Technical Reference

> Architecture, internals, and implementation details.

For the quick-start guide, see [README.md](README.md). For the command reference, see [MANUAL.md](MANUAL.md).

---

## Overview

jj-plan is a Rust binary (~14,400 lines) that shadows the real `jj` binary. It provides two capabilities:

1. **Plan management** — Bidirectional sync between `.jj-plan/` markdown files and jj change descriptions, with navigation, templating, and working memory lifecycle.
2. **Stacked PRs** — Push bookmarks as PRs to GitHub or GitLab, with plan content as PR descriptions, stack-aware base branch targeting, merge readiness checks, and post-merge cleanup.

Read-only jj commands (`log`, `diff`, `show`, etc.) pass through via `exec` with zero overhead. Mutating commands go through the wrap lifecycle: flush pending edits → run command → reload → sync plan files → show stack.

---

## See Also

- [README.md](README.md) — Project overview, philosophy, quick start
- [MANUAL.md](MANUAL.md) — Exhaustive command reference, recipes

---

## Project Structure

| Module | Lines | Description |
|---|---|---|
| `src/lib.rs` | — | Library root, re-exports |
| `src/main.rs` | 149 | Entry point, command dispatch |
| `src/error.rs` | 110 | `JjPlanError` enum (~25 variants) |
| `src/types.rs` | 850 | All domain types: `Bookmark`, `LogEntry`, `Stack`, `PlanRegistry`, PR/platform types, `description_first_line`/`description_is_done` free functions |
| `src/workspace.rs` | 1045 | Unified jj-lib wrapper: reads + git write operations |
| `src/stack_render.rs` | 1075 | Pure stack rendering: Span/Style model, multi-column layout, ANSI/plain/markdown formatting |
| `src/stack_builder.rs` | 1477 | Stack construction, gap detection, `collect_submission_chain()` |
| `src/wrap.rs` | ~500 | Wrap lifecycle, `cleanup_stale_and_migrate()`, three-tier sync (`sync_to_disk`, `sync_and_show`, `full_sync_and_show`), `find_stale_bookmarks()`, `StackDisplayData`, `SyncChangeView` |
| `src/flush.rs` | 298 | Plan file → jj description sync (file is authoritative) |
| `src/sync.rs` | 587 | jj description → plan file sync (jj is authoritative post-flush); receives `stack_md_content` from caller |
| `src/plan_file.rs` | 574 | Plan file parsing, bookmark name encoding, legacy migration |
| `src/plan_dir.rs` | 219 | Repo root and plan directory resolution |
| `src/plan_registry.rs` | 229 | PlanRegistry persistence (`.jj/repo/jj-plan/plans.toml`) |
| `src/pr_cache.rs` | 252 | PR cache persistence (`.jj/repo/jj-plan/pr-cache.toml`) |
| `src/stack_context.rs` | 94 | Shared context for `jj stack` commands |
| `src/markdown.rs` | 633 | `strip_scratch_sections()` with code fence awareness |
| `src/template.rs` | 274 | Plan template resolution and interpolation |
| `src/jj_binary.rs` | 144 | Real jj binary discovery |
| `src/platform/mod.rs` | 52 | `PlatformService` async trait |
| `src/platform/github.rs` | 276 | GitHub implementation (octocrab) |
| `src/platform/gitlab.rs` | 487 | GitLab implementation (reqwest) |
| `src/platform/gitea.rs` | — | Gitea implementation (reqwest) |
| `src/platform/detection.rs` | 426 | URL → platform detection |
| `src/platform/factory.rs` | 31 | Service construction with auth |
| `src/auth/mod.rs` | 17 | `AuthSource` enum |
| `src/auth/github.rs` | 88 | gh CLI + env var token resolution |
| `src/auth/gitlab.rs` | 110 | glab CLI + env var token resolution |
| `src/auth/gitea.rs` | — | Gitea env var token resolution |
| `src/submit/mod.rs` | 13 | Submit engine re-exports |
| `src/submit/analysis.rs` | 127 | Submission analysis, plan-to-PR content bridge |
| `src/submit/plan.rs` | 133 | Execution step planning (push/create/retarget) |
| `src/submit/execute.rs` | 144 | Step execution with progress callbacks |
| `src/submit/progress.rs` | 85 | `ProgressCallback` trait, `NoopProgress` |
| `src/merge/mod.rs` | 10 | Merge engine re-exports |
| `src/merge/plan.rs` | 184 | Pure merge planning (intended sequence) |
| `src/merge/execute.rs` | 334 | Merge execution with just-in-time readiness polling |
| `src/commands/stack_cmd.rs` | 1160 | `jj stack` dispatch, submit/sync/merge/auth CLI |
| `src/commands/done.rs` | 315 | `jj plan done` with scratch stripping |
| `src/commands/describe.rs` | 379 | `jj describe -m` interception |
| `src/commands/new.rs` | 195 | `jj plan new` bookmark creation |
| `src/commands/nav.rs` | 273 | `jj plan next/prev/go` navigation |
| `src/commands/help.rs` | 639 | `jj plan --help` rendering |
| `src/commands/config.rs` | 92 | `jj plan config` introspection |
| `src/commands/track.rs` | 107 | `jj plan track` |
| `src/commands/untrack.rs` | 88 | `jj plan untrack` |
| `src/commands/abandon.rs` | 20 | `jj abandon` delegation |
| `src/commands/mod.rs` | 126 | `jj plan` subcommand dispatch |
| `tests/gitea_integration.rs` | — | Gitea full lifecycle integration tests |

---

## Workspace (`src/workspace.rs`)

The `Workspace` struct wraps `jj_lib::workspace::Workspace` and `Arc<ReadonlyRepo>`, providing all in-process repository access.

### Design: "Path A" — jj-lib for reads, CLI for writes

- **Reads** (revset evaluation, bookmark queries, description reads) use jj-lib's in-process API — no subprocess overhead.
- **Mutations** (`jj describe`, `jj new`, `jj edit`, `jj abandon`, `jj bookmark set`) use subprocess calls because the CLI handles working copy snapshotting, auto-rebase, and conflict resolution.
- **Git operations** (fetch, push, rebase, delete-bookmark) use jj-lib's in-process API directly, since they operate on the git backend and don't need working copy interaction.

### Cached repo snapshot

Unlike ryu's `JjWorkspace` (which re-loads at head on every call), jj-plan caches the repo and refreshes only via explicit `reload()` calls after CLI mutations.

For git write operations, the pattern is:
1. `self.reload()` to get fresh state before starting a transaction.
2. `self.repo.start_transaction()` to begin the write.
3. `tx.commit(description)` returns the new `Arc<ReadonlyRepo>`.
4. `self.repo = new_repo` to update the cached snapshot.

### Git write operations

Added in jj:zypnnqyt. Adapted from ryu's `JjWorkspace` for jj-lib 0.38:

| Method | Purpose |
|---|---|
| `git_remotes()` | List all git remotes with URLs (via gix) |
| `default_branch()` | Detect default branch from remote HEAD, fallback to main/master/trunk |
| `git_fetch(remote)` | Fetch + import refs + rebase descendants |
| `git_fetch_bookmarks(remote, bookmarks)` | Fetch only named bookmarks (uses `StringExpression::union_all` of `exact()`) — used by submit for pre-push tracking-ref refresh |
| `git_push(bookmark, remote)` → `PushOutcome` | Export refs + push + conditionally update tracking ref. Returns `PushOutcome::Success`, `Rejected { reason }` (lease failure), or `RemoteRejected { reason }` (server hooks/branch protection). On rejection, the tracking ref is **not** updated — preserving jj's accurate view of the remote and preventing cascading lease failures. |
| `rebase_bookmark_onto_trunk(bookmark)` | Resolve trunk/bookmark via revset, use `move_commits` |
| `delete_bookmark(bookmark)` | Set local bookmark target to absent |

### jj-lib 0.37 → 0.38 migration

Three breaking changes were resolved:

1. **`RemoteCallbacks` → `GitSubprocessCallback`**: A `NoopGitCallback` struct implements the 4-method trait (`needs_progress`, `progress`, `local_sideband`, `remote_sideband`).
2. **`expand_fetch_refspecs()` takes `GitFetchRefExpression`**: The struct has `bookmark: StringExpression` and `tag: StringExpression` fields instead of a bare `StringExpression`.
3. **`GitFetch::fetch()` trailing params**: Now typed as `Option<NonZeroU32>` (depth) and `Option<FetchTagsOverride>`.

---

## Stack Model (`src/stack_builder.rs`)

### Single-stack view (sync/flush pipeline)

The sync/flush pipeline uses `build_stack()`, which sees everything between trunk and the working copy:

```
trunk()..(@  | descendants(@))
```

This range is evaluated via jj-lib's in-process revset engine. The result is walked in topological order (parents before children) and partitioned into segments. This is the `@`-relative view — it only sees the lineage of the current working copy.

### Single-stack rendering (hot path)

The rendering pipeline (terminal output + `stack.md`) uses the same `build_stack()` result as the sync pipeline. In `sync_to_disk()`, the stack is built once and forked into two consumers:

1. **Sync views** (`stack_to_sync_changes`): Flat list of `SyncChangeView`s for plan file I/O.
2. **Rendering** (`build_column_from_stack`): Converts the `Stack` into a single `StackColumn` for the Span-based rendering pipeline (`render_stack` → `format_markdown`/`format_ansi`).

This "build once, fork twice" architecture avoids the redundant second jj-lib evaluation that previously occurred when `build_multi_stack()` was called on every mutating command. The `stack.md` file always contains clickable markdown links to plan files (no multi-stack link suppression).

### Multi-stack view (`jj stack --all`)

`build_multi_stack()` discovers ALL registered plan bookmarks across the repo, regardless of working copy position. It is used only by `jj stack --all` (explicit opt-in) and `jj stack untrack`. It is **not** called on the hot path (mutating commands, `jj status`, bare `jj stack`).

It uses a two-pass approach:

1. **Discovery**: For each registered bookmark, evaluate `trunk()..{bookmark}` and collect all commits into a unified map.
2. **Grouping**: Bookmarks with an explicit `stack` field in the registry are grouped by that value. Remaining bookmarks (`stack = None`) are grouped by DAG topology using a union-find algorithm (`group_bookmarks_by_ancestry()`).
3. **Segment building**: Each group's bookmarks are combined into a union revset (`trunk()..(bm1 | bm2 | ...)`), evaluated, and fed through `build_segments_and_gaps()`.

The result is a `MultiStack` containing one `StackGroup` per independent chain:

```rust
pub struct StackGroup {
    pub name: String,                    // Human-readable (from stack/* bookmark or first plan)
    pub segments: Vec<BookmarkSegment>,
    pub gaps: Vec<Gap>,
}

pub struct MultiStack {
    pub stacks: Vec<StackGroup>,         // Ordered: segment count desc, alpha tiebreaker
}
```

In `--all` mode, `build_columns()` applies multi-stack link suppression (clears `plan_filename` on rows) because per-group file indices don't match global plan file indices. This limitation only affects `--all` output.

### Stack base bookmarks

Explicit stack boundaries use `stack/<name>` bookmarks (prefix configurable via `JJ_PLAN_STACK_PREFIX`). These are regular jj bookmarks — visible in `jj log`, jjui, and any tool. jj-plan creates and manages them, but they're just bookmarks.

- Created by `jj plan new --stack <name>` on the same change as the first plan bookmark.
- Survive rebase (attached to jj change IDs, which are stable).
- Auto-cleaned when the stack base change falls behind `trunk()` (merged to trunk).
- Used for group naming in `build_multi_stack()` and for deletion in `jj stack untrack`.

### Implicit vs explicit stacks

| | Implicit (topology-inferred) | Explicit (`stack/` bookmark) |
|---|---|---|
| Created by | `jj plan new` without `--stack` | `jj plan new --stack <name>` |
| Registry `stack` field | `None` | `Some(change_id)` |
| Grouping | DAG parent-link analysis | Registry `stack` value equality |
| Boundary visible in jj log | No | Yes (`stack/<name>` bookmark) |
| Stack lifecycle | Manual `jj plan untrack` per bookmark | `jj stack untrack` for the whole stack |

### Segments and gaps

A **segment** is a contiguous run of changes ending at a bookmarked commit. The bookmark is at the tip. Segments are ordered trunk (index 0) to tip (last index).

A **gap** is a set of unbookmarked changes between two segments. Gaps are detected during stack construction and flagged at submit time.

### Merge commits

Merge commits (commits with two or more parents) in the `trunk()..@` range are handled gracefully. They are treated as ordinary unbookmarked entries and folded into the nearest segment or reported as gaps. The segment builder never inspects parent links — it walks a flat topologically-sorted array and groups by bookmarks, so linearity is not required.

### PlanRegistry filtering

When `build_stack()` receives a `PlanRegistry`, only bookmarks registered in the registry produce segments. Non-registered bookmarks are treated as if they don't exist — their changes are absorbed into adjacent segments or become gap material.

### Auto-cleanup of merged stacks

After every mutating command (via `wrap()`), `auto_cleanup_merged_stacks()` scans for explicit stacks whose base change ID is an ancestor of `trunk()`. These stacks have been fully merged — their plans are untracked from the registry and their base bookmarks are deleted automatically. The function loads the registry once, accumulates all bookmark names to untrack across all merged stacks, then applies mutations in a single post-loop pass (untrack + save + delete base bookmarks). A final `sync_to_disk` call ensures plan files are cleaned up immediately.

**Important**: Change IDs stored in the registry (`PlannedBookmark.change_id`) use standard hex encoding (`commit.change_id().hex()`). jj revsets require reverse-hex encoding. Any code embedding registry change IDs into revsets must convert via `Workspace::short_change_id_from_hex()` first.

### Key functions

| Function | Module | Signature | Purpose |
|---|---|---|---|
| `build_stack` | `stack_builder` | `(&Workspace, Option<&PlanRegistry>) → StackResult` | Build the @-relative stack (sync/flush/render) |
| `build_multi_stack` | `stack_builder` | `(&Workspace, &PlanRegistry) → MultiStack` | Build all stacks (`--all` only) |
| `build_column_from_stack` | `stack_render` | `(&Stack, &str, &PlanRegistry, &Workspace, Option<&PrCache>) → Option<StackColumn>` | Single stack → display column (hot path) |
| `build_columns` | `stack_render` | `(&MultiStack, &PlanRegistry, &Workspace, Option<&PrCache>) → Vec<StackColumn>` | Multi-stack → display columns (`--all`) |
| `group_bookmarks_by_ancestry` | `stack_builder` | `(&[(String, String)], &HashMap) → HashMap` | Union-find grouping by DAG topology |
| `find_submit_target` | `stack_builder` | `(&Stack) → Option<&BookmarkSegment>` | Find the segment nearest to `@` |
| `narrow_segments` | `stack_builder` | `(&Stack, &PlanRegistry) → Vec<NarrowedBookmarkSegment>` | One bookmark per segment |
| `collect_submission_chain` | `stack_builder` | `(&Stack, &str) → Result<SubmissionChain, String>` | Trunk-to-target chain with gaps |

---

## Command Dispatch (`src/main.rs`)

```
args[0] match:
  "plan"      → commands::dispatch_plan()
  "stack"     → commands::stack_cmd::dispatch_stack()
  "abandon"   → commands::abandon::run_abandon()
  "describe"  → commands::describe::handle_describe()
  "workspace" → subcommand routing (see below)
  read-only?  → exec(jj, args)     // zero overhead
  other       → wrap::wrap()        // flush → run → reload → sync → show
```

`dispatch_plan` routes to subcommands: `new`, `track`, `untrack`, `done`, `summary`, `next`, `prev`, `go`, `config`. The `summary` subcommand is read-only (no flush, no sync) — it reads from the workspace and jj subprocess, formats output, and prints to stdout.

Bare `jj plan` (no subcommand) shows **contextual orientation**:
- If `@` is a tracked plan → shows full summary (same as `jj plan summary`).
- If `@` is NOT a tracked plan → shows an orientation message with next steps. If plans exist in the current stack, lists them with navigation hints (`jj plan go`, `jj plan next/prev`). If no plans exist, hints `jj plan new`.
- `jj plan summary` (explicit) always shows the raw summary regardless of `@`'s state — it's a data tool, not an orientation tool.

The `resolve_plan_bookmark_at(workspace, registry, target)` helper (in `commands/mod.rs`) resolves whether a given revision has a tracked plan bookmark. Used by both `dispatch_plan` (orientation check) and `summary::run_summary` (bookmark resolution).

Before dispatch:
1. Resolve the real jj binary.
2. Check for `plan --help` early.
3. Find repo root and plan directory. **`plan` and `stack` are jj-plan-only commands** — if no plan directory exists, they show an activation message instead of falling through to real jj (which would give "unrecognized subcommand"). All other commands (`abandon`, `describe`, `new`, `edit`, etc.) are real jj commands and passthrough normally.
4. Open `Workspace` via jj-lib. If loading fails, degrade to passthrough.

### `workspace` subcommand routing

`workspace` is **not** in `READONLY_COMMANDS` because `workspace update-stale` is a mutating command that can snapshot the working copy, create recovery commits, and change `@`. If it bypassed the wrap lifecycle, flush would never run before the command and sync would never run after — the next wrapped command could see a diverged workspace state and `plan_sync`'s `None` arm would delete all plan files.

Routing uses a conservative classification via `is_workspace_readonly(args)`:

- **Read-only** (`workspace list`, `workspace root`): exec passthrough (zero overhead). Matched by checking if any element in `args[1..]` is in `WORKSPACE_READONLY_SUBS`.
- **Bare `workspace`** (no subcommand, shows help): exec passthrough (`args.len() <= 1`).
- **Everything else** (`workspace update-stale`, `workspace add`, `workspace forget`, `workspace rename`, `workspace --help`): falls through to `wrap::wrap()` for flush → run → reload → sync → show.

Unknown future workspace subcommands conservatively route through wrap, which is always safe — the only cost is flush/sync overhead, negligible for one-off workspace operations.

### `jj stack` dispatch

```
args[1] match:
  None        → show_stack_visualization()
  "--help"    → print_stack_help()
  "submit"    → run_submit()         // tokio block_on
  "sync"      → run_sync()           // tokio block_on
  "merge"     → run_merge()          // tokio block_on
  "auth"      → run_auth()           // tokio block_on
```

---

## Flush/Sync Lifecycle

The wrap lifecycle is the core mechanism that keeps plan files and jj descriptions in sync. The `wrap()` function in `src/wrap.rs` orchestrates 6 phases for every mutating command:

1. **Flush** — plan file → jj description (`flush_all`)
2. **Run** — execute the real jj command
3. **Reload** — `workspace.reload()`
4. **Full sync & show** — stale-bookmark cleanup + legacy migration + sync + display (`full_sync_and_show`)
5. **Auto-cleanup** — remove merged stacks (`auto_cleanup_merged_stacks`)

### Phase 1: Flush (`src/flush.rs`)

**Direction:** Plan file → jj description.

For each plan file in `.jj-plan/`:
1. Read the file content.
2. Resolve the bookmark name from the filename.
3. Look up the bookmark → change ID mapping via workspace bookmarks.
4. Read the current jj description for that change.
5. If the file content differs from the description, run `jj describe` to update.

This makes the plan file authoritative — any local edits to `.jj-plan/*.md` take precedence over the jj description.

### Phase 2: Run the jj command

The real `jj` command runs with inherited stdio. Its exit code is captured.

### Phase 3: Reload

`workspace.reload()` refreshes the cached `Arc<ReadonlyRepo>` to reflect the command's mutations.

### Phase 4: Full sync & show (`full_sync_and_show`)

This phase is handled by `full_sync_and_show()`, the batteries-included sync function for user-facing command paths. It delegates to `cleanup_stale_and_migrate()` + `sync_and_show()`:

1. **Cleanup** (`cleanup_stale_and_migrate`): detect and untrack stale bookmarks, migrate legacy filenames (see below).
2. **Sync to disk** (`sync_to_disk`): build the @-relative stack, fork into sync views + rendering, call `sync::sync()`.
3. **Show stack** (`show_plan_stack`): render the stack visualization to the terminal.

Callers that need cleanup without the single-stack display (e.g. `jj stack --all`, which uses its own multi-stack display) can call `cleanup_stale_and_migrate` + `sync_to_disk` directly.

### Composable prerequisite: `cleanup_stale_and_migrate`

`cleanup_stale_and_migrate(plan_dir, workspace, registry)` performs two imperative side effects that must run before sync:

1. **Detect stale bookmarks** (`find_stale_bookmarks`, pure): identify registry entries whose bookmarks no longer exist in jj (e.g. deleted by `jj abandon`). Untrack and save if any found.
2. **Migrate legacy filenames** (`plan_file::migrate_legacy_filenames`): rename old change-ID-based filenames to bookmark-named files.

This function is idempotent — safe to call multiple times (the second call finds nothing to do). It is not a sync tier itself, but a composable prerequisite that any caller can invoke before choosing which sync tier to use.

### Three-tier sync functions

`wrap.rs` provides three sync functions, each a strict superset of the one below:

| Function | Does | Used by |
|---|---|---|
| `sync_to_disk(plan_dir, workspace, registry) → Option<StackDisplayData>` | Build stack, sync plan files, return display data. No registry mutations or stderr output. | Internal paths needing display data (`auto_cleanup_merged_stacks`, `run_merge_async`, `run_done_single`, `dispatch_stack --all`) |
| `sync_and_show(plan_dir, workspace, registry, format)` | `sync_to_disk` + `show_plan_stack`. | Internal re-sync paths where stale-bookmark detection is not needed (`run_stack_untrack`, `run_untrack`) |
| `full_sync_and_show(plan_dir, workspace, registry, format)` | `cleanup_stale_and_migrate` + `sync_and_show`. | User-facing command paths after mutations (`wrap`, `dispatch_stack`, `plan_next/prev/go`, `run_done`, `run_new`, `run_track`) |

### Sync internals (`src/sync.rs`)

**Direction:** jj description → plan file.

Uses a **gather → plan → execute** architecture:

1. **Gather**: Read `.jj-plan/` to build a `CurrentPlanState` (file list, bookmark-to-filename map).
2. **Plan** (pure): Compare current state with the stack from jj-lib. Produce a `SyncPlan`:
   - Files to remove (bookmarks no longer in stack).
   - Files to rename (same bookmark, different index).
   - Files to write (description changed in jj).
   - File summary for `stack.md` (received as opaque `Option<&str>` from the caller — sync writes but does not generate this content).
3. **Execute**: Apply the plan — remove, rename, write, write `stack.md`. Also cleans up any stale `current.md` from older versions.

Note: `sync_to_disk` in `wrap.rs` builds the @-relative stack once via `build_current_stack()` and forks it into two consumers: `stack_to_sync_changes()` for plan file sync and `build_column_from_stack()` for rendering. The rendered `stack.md` content is passed to `sync::sync()` as opaque content. The returned `StackDisplayData` is reused by `show_plan_stack` — no second traversal is needed. `build_multi_stack()` is never called on this path.

`SyncChangeView` remains thin — it stores only the raw description string. Plan-awareness (metadata parsing, `is_done` checks) is accessed on demand via `PlanDocument::parse()` at consumer boundaries, not pre-cached on adapter types. This avoids sync hazards where a cached `metadata` field could desync from the `description` after mutations.

### Phase 5: Show stack

Display the plan stack using pre-gathered `StackDisplayData` from `sync_to_disk`. `show_plan_stack` delegates rendering to `render_to_stderr` (a shared helper in `stack_render.rs`) instead of doing render → format → eprintln inline. It accepts `Option<&StackDisplayData>` and a `StackFormat` parameter — it never touches disk or the repo.

Both terminal and file output are rendered from the same `Vec<Vec<Span>>` model produced by `render_stack(columns, options)` in `stack_render.rs`. `render_stack` takes `&RenderOptions` (a struct with `format: StackFormat` and `show_paths: bool`) instead of a bare `StackFormat`. `sync_to_disk` passes `show_paths: false` for `stack.md`; terminal call sites pass `show_paths: true`. The `StackFormat` enum controls layout density:

- **`Compact`** (default for terminal): 1 line per plan — description appended inline on the node line. No `│` connector lines, no blank spacers, no leading blank line.
- **`Regular`** (default for `stack.md`): 3 lines per plan — node line, `│` description line, blank spacer. This is the original pre-compact format.

`sync_to_disk` hardcodes `StackFormat::Regular` for its `stack.md` rendering call (with `show_paths: false`). The format parameter is threaded from `main.rs` (where `resolved_stack_format()` reads `JJ_PLAN_STACK_FORMAT` once) through `wrap`, `dispatch_plan`, `dispatch_stack`, and all sub-command functions to `show_plan_stack` and `render_stack`. `jj stack --format=compact|regular` overrides the env-derived default for that invocation via `parse_stack_dispatch_args`.

The `RenderOptions` struct and `render_to_stderr` helper centralize format + path display decisions so callers don't need to thread individual booleans:

```
pub struct RenderOptions {
    pub format: StackFormat,
    pub show_paths: bool,
}
```

`render_to_stderr` takes `Option<&StackDisplayData>`, `&RenderOptions`, and the plan directory name, calls `render_stack` → `format_ansi`/`format_plain`, and writes to stderr.

#### Terminal view (printed to stderr)

Graph visualization with semantic indicators. **Compact format** (default):

```
  ◉ kpqxywon (@, ~) feat-api | Add API endpoints → .jj-plan/03-feat-api.md
  ○ mtzrlpvq (~) feat-session | Implement session management → .jj-plan/02-feat-session.md
  ○ lonpswlw (✓) feat-auth | Extract auth module → .jj-plan/01-feat-auth.md
  ◆ trunk()
```

**Regular format** (`JJ_PLAN_STACK_FORMAT=regular` or `jj stack --format=regular`):

```
  ◉ kpqxywon feat-api → .jj-plan/03-feat-api.md
  │ (@, ~) Add API endpoints
  │
  ○ mtzrlpvq feat-session → .jj-plan/02-feat-session.md
  │ (~) Implement session management
  │
  ○ lonpswlw feat-auth → .jj-plan/01-feat-auth.md
  │ (✓) Extract auth module
  │
  ◆ trunk()
```

- `◉` = working copy, `○` = other node, `◆` = trunk.
- Indicators: `@` (working copy), `✓` (done), `~` (has file changes), `synced`, `PR #N`.
- `✓` supersedes `~` — a done change does not show `~`.
- `→ .jj-plan/NN-bookmark.md` = plan file relative path (terminal only, gated by `RenderOptions::show_paths`).
- Multi-stack mode adds column gutters and `stack:` headers with per-column rainbow colors.
- Each column's gutter `│`, node markers (`○`), and header are rendered in a distinct color from a rotating 6-color palette (cyan, yellow, magenta, blue, green, bright red). The working copy marker `◉` stays bold green regardless of column.
- Column gutters only appear once a column starts rendering — unstarted columns show spaces instead of `│`, reducing horizontal noise.
- The `Style` enum includes `ColumnConnector(usize)` and `ColumnHeader(usize)` variants that carry the column index for palette lookup. `format_plain` and `format_markdown` treat these as plain text.
- Implicit stacks (no `stack/*` base bookmark) are labeled `"Stack 1"`, `"Stack 2"`, etc. instead of borrowing a bookmark name. Explicit stacks keep their human-chosen name from the `stack/*` base bookmark.

#### File view (`stack.md`, written to disk)

Graph visualization with markdown links on bookmark names, generated by `format_markdown` in `stack_render.rs`. Plan file paths are NOT shown in `stack.md` — gated off via `show_paths: false` in the `RenderOptions` passed by `sync_to_disk`:

```
<!-- generated by jj-plan — do not edit -->

  ◉ kpqxywon [feat-api](./03-feat-api.md)
  │ (@, ~) Add API endpoints
  │
  ○ mtzrlpvq [feat-session](./02-feat-session.md)
  │ (~) Implement session management
  │
  ○ lonpswlw [feat-auth](./01-feat-auth.md)
  │ (✓) Extract auth module
  │
  ◆ trunk()

```

The file view uses the same Span model as the terminal view but applies `format_markdown` instead of `format_ansi`/`format_plain`. Spans with a `link_target` are wrapped in markdown link syntax `[text](target)`. Since `stack.md` is always rendered from the single @-relative stack, plan file links are always present and correct. (In `jj stack --all` mode, `plan_filename` is cleared because per-group indices don't match global plan file indices — but `--all` output goes to the terminal only, not to `stack.md`.)

The `StackColumn` struct includes a `plan_dir_name` field (e.g. `".jj-plan"`) so the renderer can compose full relative paths at render time from `row.plan_filename`. For example, if `plan_dir_name` is `".jj-plan"` and `row.plan_filename` is `"03-feat-api.md"`, the renderer produces `→ .jj-plan/03-feat-api.md`. This keeps path construction out of callers and centralizes it in the render layer.

---

## File Naming (`src/plan_file.rs`)

Plan files are named `NN-BOOKMARKNAME.md`:

- `NN` = zero-padded position in the stack (01 = closest to trunk).
- `BOOKMARKNAME` = bookmark name with `/` encoded as `--`.

Examples:
- `feat-auth` → `01-feat-auth.md`
- `stack/auth` → `02-stack--auth.md`
- `user/feature/login` → `03-user--feature--login.md`

### Registry-authoritative resolution

Filenames are **never decoded** back to bookmark names via string replacement. Instead, when reading the plan directory, each `NN-ENCODED.md` file is matched against the `PlanRegistry` to find the canonical bookmark name:

1. Parse `NN-ENCODED.md` to extract the encoded portion.
2. Call `registry.resolve_encoded(ENCODED)` — this finds the registry entry whose `encode_bookmark_for_filename(entry.name)` matches.
3. If found → the bookmark name comes from the registry (the source of truth).
4. If not found → the raw encoded string is used (orphan/legacy file).

This eliminates the old `decode_bookmark_from_filename()` function, which was a lossy inverse of a non-injective encoding (`--` → `/` was ambiguous: a bookmark literally named `feat--auth` and one named `feat/auth` both encode to the same filename).

### Collision detection

At registration time (`jj plan new`, `jj plan track`), the `PlanRegistry::would_collide()` method checks whether the new bookmark's encoded filename would collide with any existing registry entry. For example, `feat--auth` and `feat/auth` both encode to filename portion `feat--auth` — registering one when the other already exists is rejected with an error message identifying both names and the shared encoding.

### Legacy migration

Files matching the old `NN-CHANGEID.md` pattern (8+ chars of `[k-z]` reverse-hex) are automatically renamed to the new format during `full_sync_and_show()`.

---

## PlanRegistry (`src/plan_registry.rs`)

Persistent state stored at `.jj/repo/jj-plan/plans.toml`:

```toml
version = 2

[[bookmarks]]
name = "feat-auth"
change_id = "aabbccddee..."
planned_at = "2025-01-15T10:30:00Z"
stack = "aabbccddee..."

[[bookmarks]]
name = "feat-session"
change_id = "ffeeddccbb..."
planned_at = "2025-01-15T11:00:00Z"
stack = "aabbccddee..."
```

### Version history

- **v1**: Initial format (`name`, `change_id`, `remote`, `planned_at`).
- **v2**: Added optional `stack` field for multi-stack grouping. Backward-compatible: v1 files load with `stack = None` on all entries via `#[serde(default)]`. Old binaries reading v2 files silently ignore the unknown `stack` key.

### Stack field

The `stack` field is `Option<String>` — the standard-hex change ID of the stack's base bookmark. `None` (or absent for v1 compat) means "implicit trunk stack." Plans with the same `stack` value belong to the same logical stack. The value is a change ID (stable across rebase), not a bookmark name.

`PlanRegistry::plans_in_stack(stack_id: Option<&str>)` returns all bookmarks matching a given stack value. When `stack_id` is `None`, returns all implicit trunk-stack plans.

### Workspace indirection

The registry handles jj workspace indirection — in child workspaces (created via `jj workspace add`), `.jj/repo` is a text file pointing to the parent's repo directory. `resolve_repo_path()` reads this pointer transparently.

### Expanded role in filename resolution

Beyond tracking which bookmarks are plans, the registry is the **authoritative source** for mapping filenames back to bookmark names. Key methods:

- **`resolve_encoded(encoded) → Option<&str>`** — Given the encoded portion of a filename, finds the registry entry whose encoded name matches and returns its canonical bookmark name. Used by `collect_plan_files()` in the flush and sync gather phases.
- **`would_collide(new_name) → Option<&str>`** — Checks whether registering `new_name` would produce a filename that collides with an existing entry. Returns the colliding entry's name, or `None`. Used by `run_new()` and `run_track()` before registration.

This design means the registry is loaded once per command and threaded through all call sites. Commands that mutate the registry (`new`, `track`, `untrack`, `merge`) use a pre-mutation registry for flush (resolving existing files) and a post-mutation registry for sync (generating files for newly tracked bookmarks).

---

## Platform Layer (`src/platform/`)

### `PlatformService` trait

An async trait (via `async_trait`) with 12 methods covering the full PR lifecycle for GitHub, GitLab, and Gitea. All methods are now wired into the CLI:

| Method | Purpose | Wired via |
|---|---|---|
| `find_existing_pr(head)` | Find open PR for a branch | `create_submission_plan` |
| `create_pr_with_options(head, base, title, body, draft)` | Create a PR | `execute_submission` → `CreatePr` step |
| `update_pr_base(number, new_base)` | Retarget a PR | `execute_submission` → `UpdateBase` step |
| `update_pr_description(number, title, body)` | Update PR title/body | `execute_submission` → `UpdateDescription` step |
| `publish_pr(number)` | Convert draft → ready | `execute_submission` → `PublishPr` step |
| `list_pr_comments(number)` | List comments | Comment pass in `run_submit_async` |
| `create_pr_comment(number, body)` | Post comment | `execute_submission` → `AddStackComment` step |
| `update_pr_comment(number, comment_id, body)` | Edit comment | `execute_submission` → `AddStackComment` step |
| `config()` | Get platform config | Reserved (forward-looking) |
| `get_pr_details(number)` | Extended details for merge/description comparison (returns `PullRequestDetails`, which includes `head_sha: Option<String>` — used by GitHub's `check_merge_readiness` to query check runs; other platforms leave it as `None`) | `create_submission_plan` (when `--update-descriptions`) |
| `check_merge_readiness(number)` | Approval + CI + conflict checks | `run_merge_async` |
| `merge_pr(number, method)` | Perform merge | `run_merge_async` |

### GitHub implementation (`src/platform/github.rs`)

Uses `octocrab` (typed GitHub client) for REST and GraphQL operations:

- **`publish_pr`**: Uses a GraphQL mutation (`markPullRequestReadyForReview`) since the REST API cannot clear draft status. Fetches the PR's `node_id` via REST, then issues the mutation. Falls back to `gh pr ready` subprocess if GraphQL fails (e.g., classic tokens without GraphQL permissions).
- **`update_pr_description`**: Uses octocrab's `pulls().update(number).title(...).body(...).send()`.
- **`convert_pr`**: Maps octocrab PR model to our `PullRequest` type. Uses `base.ref_field` (bare branch name) rather than `base.label` (`owner:branch` format) to ensure correct comparison in the submit plan phase.
- **`check_merge_readiness`**: Now queries the reviews API (`GET /pulls/{number}/reviews`) and check runs API (`GET /commits/{sha}/check-runs`) instead of hardcoding `is_approved: false` / `ci_passed: false`. Reviews are approved if any has state `APPROVED` and none has `CHANGES_REQUESTED`. CI passes if no check runs exist or all completed runs have `conclusion` of `success`, `skipped`, or `neutral`. Failures to query either API result in a permissive fallback with an uncertainty note.

### GitLab implementation (`src/platform/gitlab.rs`)

Uses raw `reqwest` against the GitLab v4 REST API with `PRIVATE-TOKEN` authentication. The project path is URL-encoded for nested groups (e.g., `group%2Fsubgroup%2Frepo`).

- **`publish_pr`**: Uses `PUT /merge_requests/:iid { "draft": false }` (supported since GitLab 15.0).
- **`update_pr_description`**: Uses `PUT /merge_requests/:iid { "title": ..., "description": ... }`.

### Gitea implementation (`src/platform/gitea.rs`)

Uses raw `reqwest` against the Gitea v1 REST API with `Authorization: token {TOKEN}` authentication.

Key Gitea-specific patterns:
- **Auth header**: `Authorization: token {TOKEN}` (not `PRIVATE-TOKEN` like GitLab).
- **PR endpoints**: `/repos/{owner}/{repo}/pulls` and `/repos/{owner}/{repo}/pulls/{index}`.
- **Comment endpoints**: `/repos/{owner}/{repo}/issues/{index}/comments` (create/list) and `/repos/{owner}/{repo}/issues/comments/{id}` (update).
- **`merge_pr`**: `POST /pulls/{index}/merge` with `{"Do": "squash"}` — note uppercase `Do`. Returns an empty response body; the implementation GETs the PR afterwards to confirm `merged: true` and retrieve `merge_commit_sha`.
- **`update_pr_base`**: `PATCH /pulls/{index}` with `{"base": "new_branch"}`.
- **`publish_pr`**: `PATCH /pulls/{index}` with `{"draft": false}` — returns updated PR directly (unlike GitHub's GraphQL requirement).
- **`check_merge_readiness`**: Single-shot observation (no polling). Queries `/pulls/{index}/reviews` for approval status. If no reviews exist, treats as approved (self-hosted Gitea typically has no required reviews). CI status is assumed passing with an uncertainty note, since Gitea Actions status is not easily available per-PR. Polling for transient `mergeable == false` states (forge still recomputing after retargets/merges) is handled generically by the merge executor's `poll_until_ready`.
- **`mergeable`**: Gitea reports this synchronously in the PR response, but the value may be stale immediately after graph-changing events. The executor's transient-state polling handles this.
- **Branch refs**: `base.label` / `head.label` are bare branch names (not `owner:branch` like GitHub).

### Platform detection (`src/platform/detection.rs`)

Detects GitHub, GitLab, or Gitea from git remote URLs using a URL-parser-first approach with SCP-style fallback. Scheme-based URLs (`ssh://`, `https://`, `git://`) are parsed by `url::Url::parse`, which correctly separates host, port, and path. SCP-style URLs (`git@host:owner/repo.git`) — which are not valid RFC 3986 URIs — fall back to string splitting on the first `:` after `git@`. This handles `ssh://` URLs with non-standard ports (common for self-hosted Gitea, Forgejo, and Gerrit instances) without misinterpreting the port number as part of the repository path. Self-hosted instances are supported via `GH_HOST`, `GITLAB_HOST`, and `GITEA_HOST` environment variables.

Gitea detection supports `GITEA_HOST` env var and recognises `codeberg.org` as a well-known Gitea instance. For unknown hostnames that don't match any configured platform, `StackContext::new` performs an async probe of `GET /api/v1/version` — if it returns a JSON object with a `"version"` field, the host is classified as Gitea. The `parse_repo_info_as_gitea()` function handles URL parsing for probe-detected instances.

---

## Authentication (`src/auth/`)

Token resolution follows a priority chain:

| Platform | Priority |
|---|---|
| GitHub | `gh auth token` → `$GITHUB_TOKEN` → `$GH_TOKEN` |
| GitLab | `glab auth token --hostname <host>` → `$GITLAB_TOKEN` → `$GL_TOKEN` |
| Gitea   | `$GITEA_TOKEN` |

CLI tool invocations use `tokio::process::Command` (async subprocess). Token tests validate the token against the platform API (`/user` endpoint for GitLab, octocrab's current user for GitHub).

Gitea has no widely-adopted CLI tool fallback. Host is resolved from the `host` parameter, `GITEA_HOST` env var, or the probe-detected hostname.

---

## Submit Engine (`src/submit/`)

Two-pass pipeline with six `ExecutionStep` variants:

### Phase 1: Analysis (`analysis.rs`)

- Takes a `Stack` and `PlanRegistry`, narrows to one-bookmark-per-segment via `narrow_segments()`.
- Identifies the target bookmark (explicit or default to tip-most).
- Provides `get_base_branch()`: previous bookmark name or default branch.
- **Plan-to-PR bridge**: `plan_file_to_pr_content_from_entries()` in `stack_cmd.rs` reads plan files, first line = PR title, remainder = PR body with `[scratch]` stripped and `plan-status: ✅` removed.

### Phase 2: Planning (`plan.rs`)

For each segment in the chain:
1. Push the bookmark (always).
2. Check for an existing PR via the platform API.
3. If no PR → `CreatePr` step with plan-derived title/body.
4. If PR exists with wrong base → `UpdateBase` step.
5. If `--update-descriptions` and title/body differ → `UpdateDescription` step (requires `get_pr_details` for body comparison).
6. If `--publish` and PR is draft → `PublishPr` step.

`ExecutionStep` variants: `Push`, `CreatePr`, `UpdateBase`, `UpdateDescription`, `PublishPr`, `AddStackComment`.

### Phase 3: Execution (`execute.rs`)

**Pre-push fetch.** Before executing the plan, `run_submit_async` collects all bookmark names from `Push` steps and calls `workspace.git_fetch_bookmarks(remote, &bookmarks)`. This refreshes the local remote-tracking refs so that `git_push`'s `--force-with-lease` check uses up-to-date expectations. Fetch failure is non-fatal (logged as a warning) — the push may still succeed if tracking state happens to be correct.

Processes steps sequentially:
- `Push`: `workspace.git_push(bookmark, remote)` — returns `PushOutcome`. On `Rejected` or `RemoteRejected`, the error is reported via `PushStatus::Failed` with an actionable message (e.g., "try `jj git fetch` to refresh tracking state") and added to `result.errors`. The tracking ref is not corrupted.
- `CreatePr`: `platform.create_pr_with_options(...)`
- `UpdateBase`: `platform.update_pr_base(number, new_base)`
- `UpdateDescription`: `platform.update_pr_description(number, title, body)`
- `PublishPr`: `platform.publish_pr(number)`
- `AddStackComment`: `platform.create_pr_comment(...)` or `platform.update_pr_comment(...)` depending on whether an existing comment ID is present.

Supports `dry_run` mode (logs steps without executing). Uses `NoopProgress` for dry-run (silent) and `CliProgress` for real execution. The `ProgressCallback` trait provides hooks for CLI output.

### Stack Comments (`comments.rs`)

Pure function module for generating and detecting stack navigation comments:

- **`STACK_COMMENT_MARKER`**: `<!-- jj-plan stack -->` — HTML comment used to identify jj-plan comments for idempotent update.
- **`generate_stack_comment(chain, current_bookmark)`**: Produces a markdown table showing all PRs in the stack with the current PR highlighted in bold with a 👈 indicator.
- **`find_existing_comment(comments)`**: Scans comment bodies for the marker, returns the comment ID if found.

### Two-Pass Execution Model

Stack comments depend on PR numbers from freshly-created PRs, which aren't known until execution completes. The `run_submit_async` orchestrator uses a two-pass approach:

1. **Pass 1**: Execute the main plan (Push, CreatePr, UpdateBase, UpdateDescription, PublishPr).
2. **Pass 2**: Build `AddStackComment` steps from the now-known PR numbers (from cache + freshly created), then execute them.

Pass 2 is skipped for single-PR stacks (no navigation needed) or when `--no-comments` is specified.

---

## Merge Engine (`src/merge/`)

The merge engine follows the FC/IS (Functional Core / Imperative Shell) pattern:
- **Planner** (pure function): produces the *intended* merge sequence.
- **Executor** (imperative shell): owns timing, readiness polling, and failure handling.

This separation is critical because forges (Gitea, GitHub, GitLab) recompute PR mergeable status **asynchronously** after any graph-changing event (PR creation, base retarget, preceding merge). An upfront readiness snapshot goes stale as the executor mutates the graph. The executor therefore assesses readiness **just-in-time** before each merge step.

### Merge planning (`plan.rs`)

Pure function. Accepts `MergeCandidate` pairs (bookmark name + PR number) — no readiness data. Produces the intended sequence:

1. For each candidate (bottom of stack first), emit a `Merge` step.
2. After each `Merge` step (except the last), emit a `RetargetBase` step for the next PR (retarget to trunk, since the merged branch no longer exists on the forge).

The planner does **not** decide feasibility — that changes between planning and execution. There is no `Skip` step; the executor stops on hard blocks at runtime.

### Merge execution (`execute.rs`)

Processes steps sequentially. Before each `Merge` step:

1. Calls `check_merge_readiness` (single-shot observation — no polling in the platform service itself).
2. Classifies the result via `classify_readiness` into one of:
   - **`Ready`** — proceed to merge.
   - **`Transient`** — only `mergeable == false` with no other blockers (draft, approval, CI). Likely the forge is still recomputing. Polls at 1-second intervals for up to 15 attempts.
   - **`Blocked`** — real blockers exist (draft, changes requested, CI failure). Stops execution immediately.
3. If `Transient` polling exhausts retries, attempts the merge anyway (the forge's merge endpoint is the final arbiter).

Retarget failures are non-fatal (warning only) — the next `Merge` step's readiness poll detects if the retarget didn't take effect.

All platform `check_merge_readiness` implementations are single-shot observations. Polling logic lives exclusively in the executor, making it generic across all forges.

### Post-merge cleanup (in `stack_cmd.rs`)

After successful merges, `run_merge_async` performs cleanup in two passes to avoid interleaving registry mutation with workspace mutation:

1. **Registry + cache pass**: For each merged bookmark, `pr_cache.remove()` + `registry_mut.untrack()`. Save registry and cache once after the loop.
2. **Bookmark deletion pass**: For each merged bookmark, `workspace.delete_bookmark()`.
3. **Fetch**: `workspace.git_fetch()` to get updated trunk.
4. **Sync**: `sync_to_disk()` to remove orphaned plan files — plan file cleanup is delegated to `sync::sync()` rather than done manually. This matches the pattern used by `auto_cleanup_merged_stacks`.

---

## PR Cache (`src/pr_cache.rs`)

Stored at `.jj/repo/jj-plan/pr-cache.toml`:

```toml
version = 1

[[prs]]
bookmark = "feat-auth"
number = 42
url = "https://github.com/owner/repo/pull/42"
remote = "origin"
updated_at = "2025-01-15T12:00:00Z"
```

- **Populated** by `jj stack submit` after creating or finding PRs.
- **Consulted** by `jj stack submit` (create vs update decision), `jj stack merge` (PR number lookup), and `jj stack` visualization (PR status display).
- **Cleaned** by `jj stack merge` (removes entries for merged bookmarks).
- **Safe to delete** — rebuilt on next submit.

Uses `resolve_repo_path()` from `plan_registry.rs` for workspace indirection.

---

## Async Boundary

The jj-plan binary is fundamentally synchronous. Async code is confined to `jj stack` commands:

```
dispatch_stack()
  → run_submit() / run_sync() / run_merge() / run_auth()
    → tokio::runtime::Builder::new_current_thread().enable_all().build()
      → rt.block_on(async { ... })
        → StackContext::new()          // async: auth + platform detection
        → create_submission_plan()     // async: platform.find_existing_pr()
        → execute_submission()         // async: platform.create_pr() etc.
```

A single-threaded tokio runtime is created per `jj stack` command invocation. The runtime is dropped when the command completes. This avoids adding the `rt-multi-thread` feature to tokio (binary size savings).

**Why async?** `octocrab` (the GitHub client library) is async-only. Blocking alternatives would require replacing it with raw HTTP calls, losing the typed API. The tokio overhead is acceptable (~1MB binary size increase).

---

## Markdown Processing (`src/markdown.rs`)

### Callout metadata format

Plan descriptions use an Obsidian-style callout block for metadata, keeping the commit summary on line 1 (as jj/git expect):

```
feat: my feature          ← line 1: always the title (shown in jj log, git log, PR titles)

> [!plan]                 ← callout opener (case-insensitive on "plan")
> status: 🔴              ← metadata key: value lines (optional)
> issue: MERC-123

# Background              ← body content
```

Parsing rules:
- Line 1 is always the title — never metadata.
- Scan all lines (after title) for `> [!plan]` (the callout opener, case-insensitive).
- Read subsequent `> key: value` lines where key matches `^[a-z][a-z0-9_-]*: ` after stripping the `> ` prefix. The block ends at the first line that doesn't start with `> ` or doesn't match the key pattern.
- Blank lines before, after, or around the callout block do not affect parsing (blank-line tolerant).
- Body is everything outside the title line and the callout block lines. A `---` in the body is just a CommonMark thematic break — no special handling needed.
- `set_metadata_field` finds the callout block and replaces/appends the key. If no callout block exists, inserts one after the title line.

This format replaced an earlier `---`-delimited "summary-first" format that was position-sensitive (metadata had to start on line 2, no blank lines allowed) and used `---` as both metadata separator and CommonMark thematic break. The callout format is unambiguous, blank-line tolerant, and renders correctly in both plain text and markdown renderers.

Free functions: `parse_metadata()`, `set_metadata_field()`, `remove_metadata()`, `extract_headings()`, `strip_scratch_sections()`.

### `PlanDocument` — unified parse-and-transform facade

`PlanDocument` parses a description string once and provides both read accessors and transform methods. It owns its data (cloned from the input) and should be constructed at consumer boundaries (display, done, submit), not stored on long-lived types.

```rust
let doc = PlanDocument::parse(&description);

// Read accessors (borrow from owned data)
doc.title()      // → &str — line 1, the commit summary
doc.is_done()    // → bool — metadata status == ✅
doc.metadata()   // → &BTreeMap<String, String>
doc.body()       // → &str — everything outside title and callout block
doc.raw()        // → &str — original input
doc.headings()   // → Vec<HeadingInfo> — all headings with line numbers

// Transform methods (allocate new Strings)
doc.as_done(keep_scratch)  // → String — strip scratch + set status: ✅ in callout
doc.pr_parts()             // → Option<(String, String)> — (title, body) for PR
doc.body_sans_scratch()    // → String — body with [scratch] sections removed
```

Four consumer patterns:
1. **Done path** (`done.rs`): `PlanDocument::parse(desc).as_done(keep_scratch)` → `jj describe -m`.
2. **Submit path** (`stack_cmd.rs`): `PlanDocument::parse(&content).pr_parts()` → PR title + body.
3. **Display path** (`stack_render.rs`): `PlanDocument::parse(&tip.description)` → `is_done`, `title`, `metadata` indicators.
4. **Summary path** (`summary.rs`): `PlanDocument::parse(&desc)` → `title`, `metadata`, `body`, `headings()` for outline extraction.

`description_is_done()` and `description_first_line()` in `types.rs` remain as thin wrappers for plan-agnostic call sites (`LogEntry`, `SyncChangeView`) that don't need a full `PlanDocument`.

### Heading extraction

`extract_headings(input: &str) -> Vec<HeadingInfo>` uses `pulldown-cmark` with `into_offset_iter()` for CommonMark-compliant heading detection (ATX and setext headings, code fence immunity). Each `HeadingInfo` contains `level` (1–6), `text` (inline markup stripped), `byte_offset`, and `line` (1-based, computed from byte offset). This is the shared foundation for both scratch-section stripping and outline extraction.

`PlanDocument::headings()` is a convenience accessor that calls `extract_headings(&self.raw)` on demand (not cached), consistent with other on-demand methods like `body_sans_scratch()`.

### Scratch section stripping

`strip_scratch_sections()` is a consumer of `extract_headings()` — it filters for headings containing `[scratch]` (case-insensitive) and slices them out using byte offsets, preserving all original formatting byte-for-byte in non-scratch regions.

Edge cases handled: multiple scratch sections, nested headings, setext headings, code fences, empty input, entire document as scratch, callout metadata preservation, `---` thematic breaks in body.

---

## Plan Templates (`src/template.rs`)

Resolution chain:
1. `$JJ_PLAN_TEMPLATE` environment variable (path to file).
2. `.jj-plan/template.md` in the repository.
3. Built-in default (minimal summary line).

Interpolation: `{{CHANGE_ID}}` → short reverse-hex change ID, `{{BOOKMARK}}` → bookmark name. If no `{{CHANGE_ID}}` in a custom template, a self-referencing HTML comment is injected as the second line.

---

## Describe Interception (`src/commands/describe.rs`)

The `desc` alias is caught in dispatch (`main.rs`) alongside `describe` — both route to `handle_describe`.

### Guard behavior

When `jj describe -m "..."` or `jj describe --stdin` targets a **tracked plan**, the command is **blocked** with an educational error message. This prevents LLMs (and humans) from accidentally replacing a rich, multi-line plan document with a one-liner.

The guard logic follows GATHER → PLAN → EXECUTE:

1. **GATHER**: Parse args (messages, revision, `--stdin`, `--override-plan-protocol`, positional revsets). Resolve the target via `resolve_plan_bookmark_at`. Look up the plan file entry via `collect_plan_files`.
2. **PLAN**: Call `plan_describe_action(parsed, plan_file_path)` — a pure decision function that returns one of:
   - `EditorPassthrough` — no `-m`/`--stdin`, pass through to wrap unchanged.
   - `Allow` — `-m`/`--stdin` targets a non-plan change, delegate to wrap.
   - `AllowOverride` — `-m`/`--stdin` targets a tracked plan WITH `--override-plan-protocol`. Write message to plan file, strip the flag, then wrap.
   - `Block` — `-m`/`--stdin` targets a tracked plan WITHOUT override. Print error, return exit code 1.
3. **EXECUTE**: Match on the action and carry it out.

### Editor-mode describe

Editor-mode describe (no `-m`/`--stdin`) is **unguarded** — the user opens an editor with the full plan content and can make informed edits. The guard is specifically for non-interactive replacement.

### `--override-plan-protocol` flag

Adding `--override-plan-protocol` to a blocked describe allows it to proceed. The flag is intentionally long to prevent accidental use. It is stripped from args before delegation to jj (which would reject the unknown flag).

### Positional revset parsing

`jj describe` accepts `[REVSETS]...` as positional arguments (with `-r` as an alias). The parser handles both forms: explicit `-r`/`--revision` flags take precedence, and the first non-flag positional arg is used as a fallback when no explicit revision flag is given. Known jj global options that take values (`--repository`, `--at-operation`, `--color`, `--config`, `--config-file`, `-R`) are skipped during positional collection to avoid misidentifying their values as revsets.

---

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DEBUG` | Enable diagnostic logging to stderr (any value) | unset |
| `JJ_PLAN_DIR` | Override plan directory path | `.jj-plan/` → `.jj-plans/` |
| `JJ_PLAN_MAX` | Max stack size before refusing to sync | `50` |
| `JJ_PLAN_STACK_FORMAT` | Terminal stack format: `compact` or `regular` | `compact` |
| `JJ_PLAN_STACK_PREFIX` | Prefix for stack base bookmarks | `stack/` |
| `JJ_PLAN_TEMPLATE` | Override plan template file path | `.jj-plan/template.md` → built-in |
| `GITHUB_TOKEN` / `GH_TOKEN` | GitHub personal access token | — |
| `GITLAB_TOKEN` / `GL_TOKEN` | GitLab personal access token | — |
| `GITEA_TOKEN`              | Gitea personal access token          | —         |
| `GH_HOST` | GitHub Enterprise hostname | `github.com` |
| `GITLAB_HOST` | Self-hosted GitLab hostname | `gitlab.com` |
| `GITEA_HOST`               | Gitea instance hostname              | —         |
| `GITEA_INTEGRATION`        | Enable Gitea integration tests       | unset     |

---

## Performance

### Binary size

The HTTP/TLS dependencies (reqwest, octocrab, rustls) add ~5–10MB to the binary compared to the plan-only version. Users who don't use `jj stack` commands still carry this cost. A future cargo feature could make the PR layer optional.

### Command latency

| Operation | Latency | Bottleneck |
|---|---|---|
| Read-only passthrough (`jj log`) | ~0ms overhead | `exec` replaces process |
| Mutating wrap (`jj new`, `jj edit`) | ~20–50ms overhead | jj-lib repo reload + sync |
| `jj stack submit --dry-run` | ~100–200ms | Workspace load + stack build + analysis |
| `jj stack submit` (real) | 2–10s per PR | Network I/O (git push + API calls) |
| `jj stack merge` | 2–5s per merge | Network I/O (API calls) |

### Build time

The proc-macro-heavy dependencies (octocrab, serde, tokio) increase build time significantly. Consider `[profile.dev] opt-level = 1` for dependencies if build time becomes painful.

---

## Testing

### Unit tests (`cargo test`)

389 tests covering:

| Module | Tests | Covers |
|---|---|---|
| `commands/` | 105 | Dispatch, describe interception & guard, navigation, new/track/untrack, stack visualization, WC adoption |
| `plan_file.rs` | 30 | Filename parsing, bookmark encoding, registry-based resolution, legacy detection |
| `stack_builder.rs` | 26 | Stack construction, gap detection, registry filtering, `collect_submission_chain`, multi-stack grouping |
| `types.rs` | 23 | `LogEntry` methods, `PlanRegistry` CRUD, `resolve_encoded`, `would_collide`, TOML roundtrip, v1→v2 compat, `plans_in_stack` |
| `markdown.rs` | 20 | Scratch stripping, code fence immunity, edge cases |
| `template.rs` | 16 | Resolution chain, interpolation, bookmark placeholders, fallback |
| `sync.rs` | 14 | Gather/plan/execute phases, symlink targeting, edge cases |
| `plan_dir.rs` | 8 | Directory resolution, plan max |
| `pr_cache.rs` | 7 | TOML roundtrip, upsert/remove, path resolution |
| `plan_registry.rs` | 6 | Load/save, workspace indirection, directory creation |
| `flush.rs` | 6 | Description comparison, bookmark-based resolution |
| `platform/detection.rs` | 18 | URL parsing, platform detection |

### Bats integration tests (`./test.sh`)

126 behavioral tests using [bats-core](https://github.com/bats-core/bats-core). A template jj repo with `.jj-plan/` is created once per run; each test gets an isolated `cp -r` copy. Tests run in parallel with GNU `parallel`.

### PR integration tests

Full integration tests for GitHub/GitLab API calls are deferred — they require either a real account or a mock server. The `--dry-run` flag validates the full local pipeline (stack building → analysis → planning) without network calls.

### Gitea integration tests

Full lifecycle tests against a real Gitea instance, gated behind `GITEA_INTEGRATION=1`:

```sh
GITEA_INTEGRATION=1 GITEA_HOST=code.halecraft.org GITEA_TOKEN=xxx \
  cargo test --test gitea_integration -- --test-threads=1
```

Tests cover: submit lifecycle (create, find, retarget, comments), merge lifecycle (readiness, squash merge, retarget, merge), draft publish lifecycle, and description updates. Each test creates a throwaway private repo and deletes it on completion.

---

## Dependencies

### Core

| Crate | Version | Purpose |
|---|---|---|
| `jj-lib` | 0.38 | In-process jj repository access |
| `gix` | 0.78 | Git remote HEAD detection for `default_branch()` |
| `chrono` | 0.4 | Timestamps in `PlannedBookmark`, `CachedPr` |
| `serde` / `serde_json` | 1 | Serialization for registry, PR cache, API responses |
| `toml` / `toml_edit` | 0.8 / 0.24 | TOML persistence |
| `thiserror` | 2 | Error derive macros |

### Platform / networking

| Crate | Version | Purpose |
|---|---|---|
| `tokio` | 1 | Async runtime for `jj stack` commands |
| `octocrab` | 0.47 | Typed GitHub API client |
| `reqwest` | 0.12 | HTTP client for GitLab API |
| `async-trait` | 0.1 | Async trait support for `PlatformService` |
| `url` | 2 | URL parsing for platform detection |
| `urlencoding` | 2 | GitLab project path encoding |
| `base64` | 0.22 | Stack comment encoding |

### CLI / UX

| Crate | Version | Purpose |
|---|---|---|
| `owo-colors` | 4 | Terminal color support |
| `anstream` | 0.6 | ANSI-aware output streams |
| `dialoguer` | 0.11 | Interactive prompts |
| `indicatif` | 0.17 | Progress spinners |