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
| `src/main.rs` | 149 | Entry point, command dispatch |
| `src/error.rs` | 110 | `JjPlanError` enum (~25 variants) |
| `src/types.rs` | 850 | All domain types: `Bookmark`, `LogEntry`, `Stack`, `PlanRegistry`, PR/platform types, `description_first_line`/`description_is_done` free functions |
| `src/workspace.rs` | 1045 | Unified jj-lib wrapper: reads + git write operations |
| `src/stack_render.rs` | 1075 | Pure stack rendering: Span/Style model, multi-column layout, ANSI/plain/markdown formatting |
| `src/stack_builder.rs` | 1477 | Stack construction, gap detection, `collect_submission_chain()` |
| `src/wrap.rs` | 407 | Wrap lifecycle, `resolve_and_sync()`, `resolve_sync_and_show()`, `StackDisplayData`, `SyncChangeView` |
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
| `src/platform/detection.rs` | 137 | URL → platform detection |
| `src/platform/factory.rs` | 31 | Service construction with auth |
| `src/auth/mod.rs` | 17 | `AuthSource` enum |
| `src/auth/github.rs` | 88 | gh CLI + env var token resolution |
| `src/auth/gitlab.rs` | 110 | glab CLI + env var token resolution |
| `src/submit/mod.rs` | 13 | Submit engine re-exports |
| `src/submit/analysis.rs` | 127 | Submission analysis, plan-to-PR content bridge |
| `src/submit/plan.rs` | 133 | Execution step planning (push/create/retarget) |
| `src/submit/execute.rs` | 144 | Step execution with progress callbacks |
| `src/submit/progress.rs` | 85 | `ProgressCallback` trait, `NoopProgress` |
| `src/merge/mod.rs` | 9 | Merge engine re-exports |
| `src/merge/plan.rs` | 171 | Pure merge planning (two-pass algorithm) |
| `src/merge/execute.rs` | 75 | Merge execution via platform API |
| `src/commands/stack_cmd.rs` | 1041 | `jj stack` dispatch, submit/sync/merge/auth CLI |
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
| `git_push(bookmark, remote)` | Export refs + push + update tracking ref |
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

### Multi-stack view (visualization)

`build_multi_stack()` discovers ALL registered plan bookmarks across the repo, regardless of working copy position. It uses a two-pass approach:

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

`build_multi_stack()` is used only by `jj stack` visualization and `jj stack untrack`. The sync/flush pipeline continues to use `build_stack()`. This separation ensures zero coupling between multi-stack awareness and the critical sync path.

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

After every mutating command (via `wrap()`), `auto_cleanup_merged_stacks()` scans for explicit stacks whose base change ID is an ancestor of `trunk()`. These stacks have been fully merged — their plans are untracked from the registry and their base bookmarks are deleted automatically.

**Important**: Change IDs stored in the registry (`PlannedBookmark.change_id`) use standard hex encoding (`commit.change_id().hex()`). jj revsets require reverse-hex encoding. Any code embedding registry change IDs into revsets must convert via `Workspace::short_change_id_from_hex()` first.

### Key functions

| Function | Signature | Purpose |
|---|---|---|
| `build_stack` | `(&Workspace, Option<&PlanRegistry>) → StackResult` | Build the @-relative stack (sync/flush) |
| `build_multi_stack` | `(&Workspace, &PlanRegistry) → MultiStack` | Build all stacks (visualization) |
| `group_bookmarks_by_ancestry` | `(&[(String, String)], &HashMap) → HashMap` | Union-find grouping by DAG topology |
| `find_submit_target` | `(&Stack) → Option<&BookmarkSegment>` | Find the segment nearest to `@` |
| `narrow_segments` | `(&Stack, &PlanRegistry) → Vec<NarrowedBookmarkSegment>` | One bookmark per segment |
| `collect_submission_chain` | `(&Stack, &str) → Result<SubmissionChain, String>` | Trunk-to-target chain with gaps |

---

## Command Dispatch (`src/main.rs`)

```
args[0] match:
  "plan"      → commands::dispatch_plan()
  "stack"     → commands::stack_cmd::dispatch_stack()
  "abandon"   → commands::abandon::run_abandon()
  "describe"  → commands::describe::handle_describe()
  read-only?  → exec(jj, args)     // zero overhead
  other       → wrap::wrap()        // flush → run → reload → sync → show
```

Before dispatch:
1. Resolve the real jj binary.
2. Check for `plan --help` early.
3. Find repo root and plan directory.
4. Open `Workspace` via jj-lib. If loading fails, degrade to passthrough.

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

The wrap lifecycle is the core mechanism that keeps plan files and jj descriptions in sync.

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

### Phase 4: Sync (`src/sync.rs`)

**Direction:** jj description → plan file.

Uses a **gather → plan → execute** architecture:

1. **Gather**: Read `.jj-plan/` to build a `CurrentPlanState` (file list, bookmark-to-filename map).
2. **Plan** (pure): Compare current state with the stack from jj-lib. Produce a `SyncPlan`:
   - Files to remove (bookmarks no longer in stack).
   - Files to rename (same bookmark, different index).
   - Files to write (description changed in jj).
   - Symlink target (which file `current.md` points to).
   - File summary for `stack.md` (received as opaque `Option<&str>` from the caller — sync writes but does not generate this content).
3. **Execute**: Apply the plan — remove, rename, write, update symlink, write `stack.md`.

Note: `resolve_and_sync` in `wrap.rs` performs a single GATHER that serves both the sync pipeline (plan file I/O) and the rendering pipeline (terminal + markdown output). It builds the multi-stack, generates `stack.md` content via `format_markdown_with_header`, and passes it to `sync::sync()` as opaque content. The returned `StackDisplayData` is reused by `show_plan_stack` — no second GATHER traversal is needed.

### Phase 5: Show stack

Display the plan stack using pre-gathered `StackDisplayData` from `resolve_and_sync`. `show_plan_stack` accepts `Option<&StackDisplayData>` — it runs PLAN (`render_stack`) and EXECUTE (`format_ansi`/`format_plain` + `eprintln!`) but never touches disk or the repo.

Both terminal and file output are rendered from the same `Vec<Vec<Span>>` model produced by `render_stack` in `stack_render.rs`:

#### Terminal view (printed to stderr)

Graph visualization with semantic indicators:

```
  ◉ feat-api kpqxywon (@, ~)
  │ Add API endpoints
  │
  ○ feat-session mtzrlpvq (~)
  │ Implement session management
  │
  ○ feat-auth lonpswlw (✓)
  │ Extract auth module
  │
  ◆ trunk()
```

- `◉` = working copy, `○` = other node, `◆` = trunk.
- Indicators: `@` (working copy), `✓` (done), `~` (has file changes), `synced`, `PR #N`.
- `✓` supersedes `~` — a done change does not show `~`.
- Multi-stack mode adds column gutters and `stack:` headers.

#### File view (`stack.md`, written to disk)

Graph visualization with markdown links on bookmark names, generated by `format_markdown` in `stack_render.rs`:

```
<!-- generated by jj-plan — do not edit -->

  ◉ [feat-api](./03-feat-api.md) kpqxywon (@, ~)
  │ Add API endpoints
  │
  ○ [feat-session](./02-feat-session.md) mtzrlpvq (~)
  │ Implement session management
  │
  ○ [feat-auth](./01-feat-auth.md) lonpswlw (✓)
  │ Extract auth module
  │
  ◆ trunk()

```

The file view uses the same Span model as the terminal view but applies `format_markdown` instead of `format_ansi`/`format_plain`. Spans with a `link_target` are wrapped in markdown link syntax `[text](target)`. In multi-stack mode, `plan_filename` is cleared (links are absent) because per-group indices don't match global plan file indices.

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

Files matching the old `NN-CHANGEID.md` pattern (8+ chars of `[k-z]` reverse-hex) are automatically renamed to the new format during `resolve_and_sync()`.

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

An async trait (via `async_trait`) with 12 methods covering the full PR lifecycle. All methods are now wired into the CLI:

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
| `get_pr_details(number)` | Extended details for merge/description comparison | `create_submission_plan` (when `--update-descriptions`) |
| `check_merge_readiness(number)` | Approval + CI + conflict checks | `run_merge_async` |
| `merge_pr(number, method)` | Perform merge | `run_merge_async` |

### GitHub implementation (`src/platform/github.rs`)

Uses `octocrab` (typed GitHub client) for REST and GraphQL operations:

- **`publish_pr`**: Uses a GraphQL mutation (`markPullRequestReadyForReview`) since the REST API cannot clear draft status. Fetches the PR's `node_id` via REST, then issues the mutation. Falls back to `gh pr ready` subprocess if GraphQL fails (e.g., classic tokens without GraphQL permissions).
- **`update_pr_description`**: Uses octocrab's `pulls().update(number).title(...).body(...).send()`.
- **`convert_pr`**: Maps octocrab PR model to our `PullRequest` type. Uses `base.ref_field` (bare branch name) rather than `base.label` (`owner:branch` format) to ensure correct comparison in the submit plan phase.

### GitLab implementation (`src/platform/gitlab.rs`)

Uses raw `reqwest` against the GitLab v4 REST API with `PRIVATE-TOKEN` authentication. The project path is URL-encoded for nested groups (e.g., `group%2Fsubgroup%2Frepo`).

- **`publish_pr`**: Uses `PUT /merge_requests/:iid { "draft": false }` (supported since GitLab 15.0).
- **`update_pr_description`**: Uses `PUT /merge_requests/:iid { "title": ..., "description": ... }`.

### Platform detection (`src/platform/detection.rs`)

Detects GitHub or GitLab from git remote URLs via regex matching on SSH (`git@host:owner/repo.git`) and HTTPS (`https://host/owner/repo.git`) formats. Self-hosted instances are supported via `GH_HOST` and `GITLAB_HOST` environment variables.

---

## Authentication (`src/auth/`)

Token resolution follows a priority chain:

| Platform | Priority |
|---|---|
| GitHub | `gh auth token` → `$GITHUB_TOKEN` → `$GH_TOKEN` |
| GitLab | `glab auth token --hostname <host>` → `$GITLAB_TOKEN` → `$GL_TOKEN` |

CLI tool invocations use `tokio::process::Command` (async subprocess). Token tests validate the token against the platform API (`/user` endpoint for GitLab, octocrab's current user for GitHub).

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

Processes steps sequentially:
- `Push`: `workspace.git_push(bookmark, remote)`
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

### Merge planning (`plan.rs`)

Pure function. Two-pass algorithm:

1. **Pass 1**: Walk segments bottom-to-top. For each:
   - If PR is approved, CI passing, not draft, no conflicts → `Merge` step.
   - Otherwise → `Skip` step. All subsequent segments are also skipped (can't merge out of order).
2. **Pass 2**: After each `Merge` step, insert a `RetargetBase` step for the next PR (retarget to trunk, since the merged PR's branch no longer exists).

### Merge execution (`execute.rs`)

Processes steps sequentially, stops on first failure or skip. Retarget failures are non-fatal (warning only).

### Post-merge cleanup (in `stack_cmd.rs`)

After successful merges:
1. Remove merged bookmarks from PR cache.
2. Delete merged local bookmarks via `workspace.delete_bookmark()`.
3. Remove merged plan files from `.jj-plan/`.
4. Fetch updated trunk via `workspace.git_fetch()`.

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

The `strip_scratch_sections()` function removes `[scratch]`-annotated heading sections:

1. Track code fence state (backtick and tilde fences).
2. Detect ATX headings (`#` through `######`).
3. When a heading contains `[scratch]` (case-insensitive), start stripping.
4. Stop stripping when a heading of the same or higher level is encountered.
5. Headings inside code fences are never treated as section boundaries.

Edge cases handled: multiple scratch sections, nested headings, fence char matching, empty input, entire document as scratch.

---

## Plan Templates (`src/template.rs`)

Resolution chain:
1. `$JJ_PLAN_TEMPLATE` environment variable (path to file).
2. `.jj-plan/template.md` in the repository.
3. Built-in default (minimal summary line).

Interpolation: `{{CHANGE_ID}}` → short reverse-hex change ID, `{{BOOKMARK}}` → bookmark name. If no `{{CHANGE_ID}}` in a custom template, a self-referencing HTML comment is injected as the second line.

---

## Describe Interception (`src/commands/describe.rs`)

When `jj describe -m "..."` is used:
1. Parse `-m`/`--message` and `-r`/`--revision` from args (supports all jj argument forms).
2. Write the message to the plan file for the target change.
3. Delegate to `wrap::wrap()` for the standard lifecycle.

This ensures the plan file is always the source of truth, even when users type `jj describe -m "..."` directly.

---

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DEBUG` | Enable diagnostic logging to stderr (any value) | unset |
| `JJ_PLAN_DIR` | Override plan directory path | `.jj-plan/` → `.jj-plans/` |
| `JJ_PLAN_MAX` | Max stack size before refusing to sync | `50` |
| `JJ_PLAN_STACK_PREFIX` | Prefix for stack base bookmarks | `stack/` |
| `JJ_PLAN_TEMPLATE` | Override plan template file path | `.jj-plan/template.md` → built-in |
| `GITHUB_TOKEN` / `GH_TOKEN` | GitHub personal access token | — |
| `GITLAB_TOKEN` / `GL_TOKEN` | GitLab personal access token | — |
| `GH_HOST` | GitHub Enterprise hostname | `github.com` |
| `GITLAB_HOST` | Self-hosted GitLab hostname | `gitlab.com` |

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

198 tests covering:

| Module | Tests | Covers |
|---|---|---|
| `commands/` | 51 | Dispatch, describe interception, navigation, new/track/untrack, stack visualization, WC adoption |
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
| `platform/detection.rs` | 5 | URL parsing, platform detection |

### Bats integration tests (`./test.sh`)

126 behavioral tests using [bats-core](https://github.com/bats-core/bats-core). A template jj repo with `.jj-plan/` is created once per run; each test gets an isolated `cp -r` copy. Tests run in parallel with GNU `parallel`.

### PR integration tests

Full integration tests for GitHub/GitLab API calls are deferred — they require either a real account or a mock server. The `--dry-run` flag validates the full local pipeline (stack building → analysis → planning) without network calls.

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
| `regex` | 1 | Remote URL pattern matching |
| `base64` | 0.22 | Stack comment encoding |

### CLI / UX

| Crate | Version | Purpose |
|---|---|---|
| `owo-colors` | 4 | Terminal color support |
| `anstream` | 0.6 | ANSI-aware output streams |
| `dialoguer` | 0.11 | Interactive prompts |
| `indicatif` | 0.17 | Progress spinners |