# jj-plan Technical Reference

> Architecture, internals, and implementation details.

For the quick-start guide, see [README.md](README.md). For the command reference, see [MANUAL.md](MANUAL.md).

---

## Overview

jj-plan is a Rust binary (~11,700 lines) that shadows the real `jj` binary. It provides two capabilities:

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
| `src/types.rs` | 575 | All domain types: `Bookmark`, `LogEntry`, `Stack`, `PlanRegistry`, PR/platform types |
| `src/workspace.rs` | 972 | Unified jj-lib wrapper: reads + git write operations |
| `src/stack_builder.rs` | 1079 | Stack construction, gap detection, `collect_submission_chain()` |
| `src/wrap.rs` | 194 | Wrap lifecycle, `resolve_and_sync()`, `SyncChangeView` |
| `src/flush.rs` | 294 | Plan file → jj description sync (file is authoritative) |
| `src/sync.rs` | 660 | jj description → plan file sync (jj is authoritative post-flush) |
| `src/plan_file.rs` | 655 | Plan file parsing, bookmark name encoding, legacy migration |
| `src/plan_dir.rs` | 208 | Repo root and plan directory resolution |
| `src/plan_registry.rs` | 229 | PlanRegistry persistence (`.jj/repo/jj-plan/plans.toml`) |
| `src/pr_cache.rs` | 252 | PR cache persistence (`.jj/repo/jj-plan/pr-cache.toml`) |
| `src/stack_context.rs` | 94 | Shared context for `jj stack` commands |
| `src/markdown.rs` | 633 | `strip_scratch_sections()` with code fence awareness |
| `src/template.rs` | 293 | Plan template resolution and interpolation |
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
| `src/commands/stack_cmd.rs` | 993 | `jj stack` dispatch, submit/sync/merge/auth CLI |
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

### Revset

The stack is everything between trunk and the working copy:

```
trunk()..(@  | descendants(@))
```

This range is evaluated via jj-lib's in-process revset engine. The result is walked in topological order (parents before children) and partitioned into segments.

### Segments and gaps

A **segment** is a contiguous run of changes ending at a bookmarked commit. The bookmark is at the tip. Segments are ordered trunk (index 0) to tip (last index).

A **gap** is a set of unbookmarked changes between two segments. Gaps are detected during stack construction and flagged at submit time.

### Merge commits

Merge commits (commits with two or more parents) in the `trunk()..@` range are handled gracefully. They are treated as ordinary unbookmarked entries and folded into the nearest segment or reported as gaps. The segment builder never inspects parent links — it walks a flat topologically-sorted array and groups by bookmarks, so linearity is not required.

### PlanRegistry filtering

When `build_stack()` receives a `PlanRegistry`, only bookmarks registered in the registry produce segments. Non-registered bookmarks are treated as if they don't exist — their changes are absorbed into adjacent segments or become gap material.

### Key functions

| Function | Signature | Purpose |
|---|---|---|
| `build_stack` | `(&Workspace, Option<&PlanRegistry>) → StackResult` | Build the full stack |
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
   - Stack summary content (for `.stack`).
3. **Execute**: Apply the plan — remove, rename, write, update symlink, write `.stack`.

### Phase 5: Show stack

Print the `.stack` file to stderr. Each line has the format:

```
{here} {status} {NN}-{bookmark_name} {change_id} :: {first_line}
```

- `{here}` = `*` if this is the working copy, blank otherwise.
- `{status}` = `✓` (done), `~` (has file changes), or blank (empty/not started).
- `{NN}` = zero-padded position (01 = closest to trunk).
- `{bookmark_name}` = the plan bookmark name.
- `{change_id}` = short reverse-hex change ID (the same form used in `jj log`, usable with `jj show`, `jj edit`, and `jj:` references in code comments).
- `{first_line}` = first line of the change description.

Example `.stack` output:

```
  ✓ 01-feat-auth kpqxywon :: Extract auth module
  ~ 02-feat-session mtzrlpvq :: Implement session management
*   03-feat-api ykvsnxrl :: Add API endpoints
```

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
version = 1

[[bookmarks]]
name = "feat-auth"
change_id = "aabbccddee..."
planned_at = "2025-01-15T10:30:00Z"

[[bookmarks]]
name = "feat-session"
change_id = "ffeeddccbb..."
planned_at = "2025-01-15T11:00:00Z"
```

The registry handles jj workspace indirection — in child workspaces (created via `jj workspace add`), `.jj/repo` is a text file pointing to the parent's repo directory. `resolve_repo_path()` reads this pointer transparently.

### Expanded role in filename resolution

Beyond tracking which bookmarks are plans, the registry is the **authoritative source** for mapping filenames back to bookmark names. Key methods:

- **`resolve_encoded(encoded) → Option<&str>`** — Given the encoded portion of a filename, finds the registry entry whose encoded name matches and returns its canonical bookmark name. Used by `collect_plan_files()` in the flush and sync gather phases.
- **`would_collide(new_name) → Option<&str>`** — Checks whether registering `new_name` would produce a filename that collides with an existing entry. Returns the colliding entry's name, or `None`. Used by `run_new()` and `run_track()` before registration.

This design means the registry is loaded once per command and threaded through all call sites. Commands that mutate the registry (`new`, `track`, `untrack`, `merge`) use a pre-mutation registry for flush (resolving existing files) and a post-mutation registry for sync (generating files for newly tracked bookmarks).

---

## Platform Layer (`src/platform/`)

### `PlatformService` trait

An async trait (via `async_trait`) with 12 methods covering the full PR lifecycle:

| Method | Purpose |
|---|---|
| `find_existing_pr(head)` | Find open PR for a branch |
| `create_pr_with_options(head, base, title, body, draft)` | Create a PR |
| `update_pr_base(number, new_base)` | Retarget a PR |
| `publish_pr(number)` | Convert draft → ready |
| `list_pr_comments(number)` | List comments |
| `create_pr_comment(number, body)` | Post comment |
| `update_pr_comment(number, comment_id, body)` | Edit comment |
| `config()` | Get platform config |
| `get_pr_details(number)` | Extended details for merge |
| `check_merge_readiness(number)` | Approval + CI + conflict checks |
| `merge_pr(number, method)` | Perform merge |

### GitHub implementation (`src/platform/github.rs`)

Uses `octocrab` (typed GitHub client) for REST and GraphQL operations. The `publish_pr` method uses a GraphQL mutation (`markPullRequestReadyForReview`) since the REST API doesn't support this. CI status is checked via both the legacy Combined Status API and the modern Check Runs API.

### GitLab implementation (`src/platform/gitlab.rs`)

Uses raw `reqwest` against the GitLab v4 REST API with `PRIVATE-TOKEN` authentication. The project path is URL-encoded for nested groups (e.g., `group%2Fsubgroup%2Frepo`).

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

Three-phase pipeline:

### Phase 1: Analysis (`analysis.rs`)

- Takes a `Stack` and `PlanRegistry`, narrows to one-bookmark-per-segment via `narrow_segments()`.
- Identifies the target bookmark (explicit or default to tip-most).
- Provides `get_base_branch()`: previous bookmark name or default branch.
- **Plan-to-PR bridge** (`plan_file_to_pr_content()`): reads the plan file, first line = PR title, remainder = PR body with `[scratch]` stripped and `plan-status: ✅` removed.

### Phase 2: Planning (`plan.rs`)

For each segment in the chain:
1. Push the bookmark (always).
2. Check for an existing PR via the platform API.
3. If no PR → `CreatePr` step with plan-derived title/body.
4. If PR exists with wrong base → `UpdateBase` step.

### Phase 3: Execution (`execute.rs`)

Processes steps sequentially:
- `Push`: `workspace.git_push(bookmark, remote)`
- `CreatePr`: `platform.create_pr_with_options(...)`
- `UpdateBase`: `platform.update_pr_base(number, new_base)`

Supports `dry_run` mode (logs steps without executing). The `ProgressCallback` trait provides hooks for CLI output.

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
| `types.rs` | 20 | `LogEntry` methods, `PlanRegistry` CRUD, `resolve_encoded`, `would_collide`, TOML roundtrip |
| `commands/` | 40 | Dispatch, describe interception, navigation, new/track/untrack, stack commands |
| `plan_file.rs` | 30 | Filename parsing, bookmark encoding, registry-based resolution, legacy detection |
| `stack_builder.rs` | 26 | Stack construction, gap detection, registry filtering, `collect_submission_chain` |
| `markdown.rs` | 20 | Scratch stripping, code fence immunity, edge cases |
| `template.rs` | 16 | Resolution chain, interpolation, bookmark placeholders, fallback |
| `sync.rs` | 14 | Gather/plan/execute phases, symlink targeting, edge cases |
| `plan_dir.rs` | 8 | Directory resolution, plan max |
| `pr_cache.rs` | 7 | TOML roundtrip, upsert/remove, path resolution |
| `plan_registry.rs` | 6 | Load/save, workspace indirection, directory creation |
| `flush.rs` | 6 | Description comparison, bookmark-based resolution |
| `platform/detection.rs` | 5 | URL parsing, platform detection |

### Bats integration tests (`./test.sh`)

125 behavioral tests using [bats-core](https://github.com/bats-core/bats-core). A template jj repo with `.jj-plan/` is created once per run; each test gets an isolated `cp -r` copy. Tests run in parallel with GNU `parallel`.

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