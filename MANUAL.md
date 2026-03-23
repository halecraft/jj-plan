# jj-plan Manual

> Exhaustive command reference for `jj plan` and `jj stack`.

For the quick-start guide, see [README.md](README.md). For architecture and internals, see [TECHNICAL.md](TECHNICAL.md).

---

## Overview

**jj-plan** is a transparent shim around [jj](https://github.com/jj-vcs/jj) (Jujutsu) that adds plan-oriented programming and stacked PR support. Install it as `jj` and it intercepts commands to keep `.jj-plan/` markdown files synchronized with change descriptions, then delegates to the real `jj` binary.

The tool provides two command namespaces:

- **`jj plan`** — Create, navigate, and manage implementation plans.
- **`jj stack`** — Visualize, submit, sync, merge, and authenticate against GitHub, GitLab, and Gitea.

All other `jj` commands pass through transparently with plan file synchronization.

---

## Concepts

### Plans

A **plan** is a jj change whose description contains an implementation plan. The change is bookmarked, and a corresponding markdown file lives in `.jj-plan/`. The file and the description are kept in sync — edits to either propagate automatically.

Plans are written *before* code exists. They contain background, constraints, rationale, rejected alternatives, and concrete tasks. When submitted as a PR, the plan content becomes the PR description.

### Stacks

A **stack** is the set of changes between `trunk()` and the working copy (including descendants): `trunk()..(@  | descendants(@))`. Within this range, bookmarked changes are **plan boundaries** — each bookmark = one plan = one PR.

Unbookmarked changes are free-form work (WIP commits, experiments). They are not managed by jj-plan and are flagged as **gaps** at submit time.

### Multi-stack awareness

jj-plan detects and visualizes multiple concurrent stacks. Stacks are discovered automatically from DAG topology (sibling branches from trunk) or explicitly via the `--stack` flag on `jj plan new`.

Explicit stacks create a `stack/<name>` bookmark as a visible boundary. The prefix is configurable via `JJ_PLAN_STACK_PREFIX` (default: `stack/`). Plans inherit their parent's stack automatically; use `--stack` to start a new one.

When stacks are merged to trunk, they are auto-cleaned from the registry.

### The `.jj-plan/` Directory

Created by `mkdir .jj-plan` to activate jj-plan in a repository. The binary maintains this directory automatically:

```
.jj-plan/
  current.md          → symlink to the active change's plan file
  stack.md            → browsable stack overview with clickable markdown links
  01-feat-auth.md     — first plan (closest to trunk)
  02-feat-session.md  — second plan
  03-feat-api.md      — third plan (tip)
  template.md         — optional: custom plan template
```

Files are named `NN-BOOKMARKNAME.md` where `NN` is the 1-based position in the stack. Bookmarks containing `/` have slashes encoded as `--` in filenames (e.g., `stack/auth` → `01-stack--auth.md`).

#### `stack.md`

The `stack.md` file is a rendered markdown document showing the full stack visualization. It is regenerated on every sync. For multi-stack repositories, it shows all stacks in a column layout with headers.

#### Status markers

The `jj status` output and `stack.md` use these markers:

| Marker | Meaning |
|---|---|
| `@` | Working copy is here |
| `✓` | Plan is done (`plan-status: ✅` in description) |
| `~` | Plan file has local changes (not yet flushed to description) |
| `synced` | Bookmark is pushed to remote |
| `PR #N` | PR exists for this bookmark (from PR cache) |

Example:

```
Plan stack (.jj-plan/):

  ◉ feat-api xqvzmzvn (@, ~)
  │ Add API endpoints
  │
  ○ feat-session mtzrlpvq (synced, PR #43)
  │ Implement session management
  │
  ○ feat-auth kpqxywon (synced, PR #42, ✓)
  │ Extract auth module
  │
  ◆ trunk()
```

### Plan Status

A plan is considered **done** when its description contains `plan-status: ✅` on its own line. This is set by `jj plan done` and displayed in the stack summary. Other status markers (`plan-status: 🔴`, `plan-status: 🟡`) can be used for tracking and are replaced in-place when `jj plan done` is run.

### Working Memory (`[scratch]`)

Any markdown heading annotated with `[scratch]` is working memory. `jj plan done` strips all scratch sections, cleaning the archival record while preserving conclusions. The full working memory is always recoverable via `jj evolog`.

```markdown
## Analysis [scratch]

This section is working memory. It will be stripped when you run `jj plan done`.

### Sub-analysis [scratch]

Everything under a `[scratch]` heading is removed, including nested headings.
```

Scratch sections are also stripped from PR bodies during `jj stack submit` — they never appear on the forge.

### Change ID References (`jj:CHANGE_ID`)

Plans can reference other plans by change ID using the `jj:CHANGE_ID` prefix. This is a convention (not enforced by tooling) that enables cross-referencing between plans:

```markdown
## Background

This builds on the auth module from jj:kpqxywon.
The session store design was validated in jj:mtzrlpvq.
```

Change IDs are visible in the stack display (`jj stack`, `stack.md`, and `jj status` output) for easy copy-paste.

---

## `jj plan` Commands

### `jj plan new <bookmark>`

Create a new plan — a jj change with a bookmark and a plan file.

```
jj plan new feat-auth                    # create a plan
jj plan new --stack auth auth-refactor   # create a plan in a new named stack
jj plan new auth-tests                   # inherits parent plan's stack
```

**What it does:**

1. Checks if the bookmark already exists (suggests `jj plan track` if so).
2. Creates a new jj change:
   - **Adopt behavior:** If the working copy (`@`) is empty, unbookmarked, and undescribed, the change is adopted in-place rather than creating a child. This avoids redundant empty changes (e.g., after a push made the previous working copy immutable and jj auto-created a new one).
   - Otherwise, runs `jj new` to create a child change.
3. Creates a bookmark on the new change.
4. Registers the bookmark in the plan registry.
5. If `--stack <name>` is provided, creates a `stack/<name>` base bookmark and associates the plan with that stack.
6. Applies the plan template and writes the initial plan file.
7. Updates `current.md` symlink to point at the new plan file.

**Arguments:**

| Argument | Required | Description |
|---|---|---|
| `<bookmark>` | Yes | Name for the bookmark (and plan file) |

**Flags:**

| Flag | Description |
|---|---|
| `--stack <name>` | Create a new named stack with a base bookmark |
| `--before <rev>` | Insert the plan before a specific revision |
| `--after <rev>` | Insert the plan after a specific revision |
| `--help`, `-h` | Show help |

### `jj plan track <bookmark>`

Register an existing bookmarked change as a plan.

```
jj plan track feat-auth
```

This is for adopting changes that already have bookmarks but weren't created via `jj plan new`. It creates the plan file and adds the bookmark to the registry.

### `jj plan untrack <bookmark>`

Remove a bookmark from plan tracking. The bookmark and change remain — only the plan registry entry and plan file are removed.

```
jj plan untrack feat-auth
```

### `jj plan done`

Mark one or all plans as done: strip `[scratch]` sections and set `plan-status: ✅`.

```
jj plan done                   # mark the working copy's plan as done
jj plan done <change_id>       # mark a specific plan as done
jj plan done --stack           # mark all plans in the stack as done
jj plan done --dry-run         # preview what would be changed
jj plan done --keep-scratch    # don't strip [scratch] sections
```

**What it does:**

1. Flushes pending plan file edits to jj descriptions.
2. Strips all `[scratch]`-annotated heading sections from the description (unless `--keep-scratch`).
3. Sets `plan-status: ✅` in the description:
   - If a `plan-status:` line already exists with a different value (e.g., `🔴`), it is replaced in-place.
   - If no `plan-status:` line exists, one is appended.
   - If `plan-status: ✅` is already present, no change is made.
4. If the target is the working copy (default), automatically advances to the next undone plan in the stack.

**Flags:**

| Flag | Description |
|---|---|
| `--stack` | Mark all changes in the stack as done |
| `--keep-scratch` | Don't strip `[scratch]` sections |
| `--dry-run` | Preview what would be changed without modifying anything |

### `jj plan next`

Advance to the next plan in the stack (toward the tip).

```
jj plan next
```

If already at the last plan, stays put.

### `jj plan prev`

Go back to the previous plan in the stack (toward trunk).

```
jj plan prev
```

If already at the first plan, stays put.

### `jj plan go <target>`

Jump to a specific plan by index, change ID, or bookmark name.

```
jj plan go 2                   # jump to plan #2 (1-based index)
jj plan go kpqx                # jump by change ID prefix
jj plan go feat-auth           # jump by bookmark name
```

### `jj plan config`

Show resolved jj-plan configuration: paths, registry contents, stack structure, and environment variable state.

```
jj plan config
```

**Output includes:**

- Shim path and repo root
- `JJ_PLAN_DIR` and `JJ_PLAN_MAX` values
- Plan directory path and resolution source
- Registry file path and all tracked bookmarks
- Stack model, segment count, and gap count
- Plans listed in stack order with descriptions

### `jj plan --help`

Show help for all `jj plan` subcommands.

```
jj plan --help
jj plan -h
```

---

## `jj stack` Commands

### `jj stack`

Show the stack visualization with bookmark structure, sync status, PR status, and change IDs.

```
jj stack
```

**Single-stack output:**

```
  ◉ feat-api ykvsnxrl (@)
  │ Add API endpoints
  │
  ○ feat-session mtzrlpvq (synced, PR #43)
  │ Implement session management
  │
  ○ feat-auth kpqxywon (synced, PR #42, ✓)
  │ Extract auth module
  │
  ◆ trunk()
```

**Multi-stack output (column layout):**

When multiple independent stacks exist (sibling branches from trunk), they are rendered side-by-side with a column gutter:

```
  auth                     │ dashboard
                           │
  ◉ auth-tests (@)         │ ○ dash-api (synced, PR #45)
  │ Add auth tests         │ │ Dashboard API endpoints
  │                        │ │
  ○ auth-refactor (PR #44) │ ◆ trunk()
  │ Refactor auth module   │
  │                        │
  ◆ trunk()                │
```

**Legend:**

| Symbol | Meaning |
|---|---|
| `◉` | Working copy (`@`) |
| `○` | Other bookmarked change |
| `◆` | Trunk (base of stack) |
| `@` | Working copy indicator |
| `✓` | Plan is done |
| `~` | Plan file has unsaved local changes |
| `synced` | Bookmark is pushed to remote |
| `PR #N` | PR exists for this bookmark (from PR cache) |

Each node line includes the short change ID (reverse-hex format) for use with `jj show`, `jj edit`, or `jj:` references in plan files.

If the stack has gaps (unbookmarked changes between bookmarks), a warning is shown.

---

### `jj stack submit [bookmark]`

Push bookmarks and create or update PRs on GitHub, GitLab, or Gitea.

```
jj stack submit                          # submit up to the tip-most bookmark near @
jj stack submit feat-auth                # submit up to a specific bookmark
jj stack submit --dry-run                # preview without making changes
jj stack submit --draft                  # create new PRs as drafts
jj stack submit --publish                # convert existing draft PRs to ready-for-review
jj stack submit --update-descriptions    # push current plan content to existing PR titles/bodies
jj stack submit --no-comments            # skip adding/updating stack navigation comments
jj stack submit --allow-gaps             # allow unbookmarked changes between bookmarks
jj stack submit --remote upstream        # specify the remote
```

**What it does:**

1. Builds the stack and analyzes what needs to be submitted.
2. Checks for gaps (unbookmarked changes between bookmarks).
3. For each bookmark in the submission chain:
   - Pushes the bookmark to the remote.
   - Creates a new PR if none exists, or updates an existing PR's base branch if needed.
   - If `--update-descriptions`: compares plan content against the existing PR title/body and updates if different.
   - If `--publish`: converts draft PRs to ready-for-review.
   - If `--draft`: creates new PRs as drafts (on Gitea, uses a `WIP:` title prefix for cross-version compatibility).
4. PR title and body come from the plan file content:
   - **Title** = first line of the plan file.
   - **Body** = remainder of the plan file, with `[scratch]` sections stripped and `plan-status: ✅` lines removed.
5. For multi-PR stacks (2+ PRs), adds or updates a **stack navigation comment** on each PR (unless `--no-comments`). The comment is a markdown table showing all PRs in the stack with a 👈 indicator on the current PR, identified by a `<!-- jj-plan stack -->` HTML comment marker for idempotent updates.
6. Updates the local PR cache (`.jj/repo/jj-plan/pr-cache.toml`).

**Arguments:**

| Argument | Required | Description |
|---|---|---|
| `[bookmark]` | No | Submit up to this bookmark. Default: tip-most bookmarked segment near `@`. |

**Flags:**

| Flag | Description |
|---|---|
| `--dry-run` | Preview what would be done without making changes |
| `--draft` | Create new PRs as drafts |
| `--publish` | Convert existing draft PRs to ready-for-review |
| `--update-descriptions` | Push current plan content to existing PR titles/bodies |
| `--no-comments` | Skip adding/updating stack navigation comments |
| `--allow-gaps` | Allow unbookmarked changes between bookmarks |
| `--remote <name>` | Specify the remote to push to (default: `origin`) |

> **Note:** `--draft` and `--publish` are mutually exclusive. `--publish` only affects PRs that were already drafts before this submit run — it does not affect newly-created PRs.

**Stack navigation comments:**

For multi-PR stacks (2 or more PRs), `jj stack submit` automatically posts a comment on each PR showing the full stack with navigation links. The comment looks like:

| | PR | Plan |
|---|---|---|
| 1 | #42 feat-auth | Extract auth module |
| **2** | **#43 feat-session** | **Implement session management** 👈 |
| 3 | #44 feat-api | Add API endpoints |

Comments are identified by a `<!-- jj-plan stack -->` HTML marker and updated in place on re-submit (idempotent). Use `--no-comments` to skip this step.

**Gap detection:**

If unbookmarked changes exist between bookmarked plans, submission is refused with an actionable error:

```
Error: unbookmarked changes detected between bookmarks.

  change xyzw (between feat-auth and feat-session)
    "wip: debugging auth flow"

Options:
  - Squash into adjacent bookmark: jj squash --from xyzw --into feat-auth
  - Give it its own bookmark:      jj bookmark create <name> -r xyzw
  - Allow gaps explicitly:          jj stack submit --allow-gaps
```

**Platform detection:**

The platform (GitHub, GitLab, or Gitea) is auto-detected from git remote URLs:

- **GitHub:** `github.com`, `*.github.com`, or hostname matching `GH_HOST`.
- **GitLab:** `gitlab.com`, `*.gitlab.com`, or hostname matching `GITLAB_HOST`.
- **Gitea:** `codeberg.org`, hostname matching `GITEA_HOST`, or any unknown host that responds to `GET /api/v1/version` with a JSON `{"version": "..."}` object.

---

### `jj stack sync`

Fetch from the remote and re-submit the stack. This is the combination of fetch + submit.

```
jj stack sync                  # fetch and re-submit
jj stack sync --dry-run        # preview without making changes
jj stack sync --remote upstream
```

**What it does:**

1. Fetches from the remote (updates tracking branches).
2. Reloads the workspace.
3. Runs the submit pipeline (push + create/update PRs).

**Flags:**

| Flag | Description |
|---|---|
| `--dry-run` | Preview what would be done |
| `--remote <name>` | Specify the remote |

---

### `jj stack merge`

Merge PRs from the bottom of the stack upward with just-in-time readiness assessment.

```
jj stack merge                 # merge all ready PRs
jj stack merge --dry-run       # preview the merge plan
jj stack merge --remote upstream
```

**What it does:**

1. Looks up PR numbers for all bookmarks in the stack (from PR cache or by searching the forge).
2. Creates a merge plan: the *intended* sequence of Merge + RetargetBase steps for all PRs, bottom-to-top.
3. Previews the plan (e.g., `• Merge #1 (feat-auth)`, `→ Retarget #2 base → main`, `• Merge #2 (feat-session)`).
4. Executes the plan with **just-in-time readiness polling**:
   - Before each merge step, calls `check_merge_readiness` and classifies the result:
     - **Ready** — proceed to merge.
     - **Transient** — only `mergeable == false` with no other blockers (the forge is likely still recomputing after a retarget or preceding merge). Polls at 1-second intervals for up to 15 seconds.
     - **Blocked** — real blockers exist (draft, changes requested, CI failure). Stops execution immediately.
   - After merging a PR, retargets the next PR's base to trunk so it becomes independently mergeable.
5. Post-merge cleanup:
   - Removes merged bookmarks from the PR cache.
   - Unregisters merged bookmarks from the plan registry.
   - Deletes merged local bookmarks.
   - Removes merged plan files from `.jj-plan/`.
   - Fetches the updated trunk.

**Merge readiness requirements:**

A PR is ready to merge when:

- It is not a draft.
- It is approved:
  - **GitHub:** At least one `APPROVED` review and no `CHANGES_REQUESTED` reviews. No reviews = approved (no required reviewers).
  - **GitLab:** The `approved` field from the approvals API.
  - **Gitea:** At least one `APPROVED` review and no `REQUEST_CHANGES` reviews. No reviews = approved (self-hosted Gitea typically has no required reviews).
- CI is passing:
  - **GitHub:** All check runs have `conclusion == "success"` (or `skipped`/`neutral`). No check runs = passing (no CI configured).
  - **GitLab:** Most recent pipeline has `status == "success"`. No pipeline = passing.
  - **Gitea:** CI status is assumed passing (with an uncertainty note — Gitea Actions status is not easily available per-PR).
- It has no merge conflicts (`mergeable == true`). A transient `false` right after a retarget is handled by polling.

**Flags:**

| Flag | Description |
|---|---|
| `--dry-run` | Preview the merge plan without merging (readiness is checked at execution time) |
| `--remote <name>` | Specify the remote |

---

### `jj stack untrack`

Unregister all plans in the current stack from the plan registry.

```
jj stack untrack               # untrack all plans in the current stack
jj stack untrack --dry-run     # preview what would be untracked
```

**What it does:**

1. Identifies all plans belonging to the current stack (based on the working copy's stack association).
2. Removes each plan from the plan registry.
3. If the stack has a base bookmark (e.g., `stack/auth`), deletes it.
4. Plan files are removed on the next sync cycle. Bookmarks and commit descriptions are **not** modified.

**Flags:**

| Flag | Description |
|---|---|
| `--dry-run` | Preview what would be untracked without making changes |

---

### `jj stack auth <platform> <action>`

Manage authentication for GitHub, GitLab, and Gitea.

```
jj stack auth github test      # test GitHub authentication
jj stack auth github setup     # show GitHub setup instructions
jj stack auth gitlab test      # test GitLab authentication
jj stack auth gitlab setup     # show GitLab setup instructions
jj stack auth gitea test       # test Gitea authentication
jj stack auth gitea setup      # show Gitea setup instructions
```

**Token resolution order:**

| Platform | Priority |
|---|---|
| GitHub | `gh auth token` → `GITHUB_TOKEN` → `GH_TOKEN` |
| GitLab | `glab auth token --hostname <host>` → `GITLAB_TOKEN` → `GL_TOKEN` |
| Gitea | `GITEA_TOKEN` |

Gitea has no widely-adopted CLI tool fallback. The host is resolved from the remote URL, `GITEA_HOST` env var, or the probe-detected hostname.

---

## Intercepted Commands

jj-plan intercepts certain jj commands to keep plan files synchronized. All intercepted commands still delegate to the real jj binary — the shim adds pre/post processing.

### `jj describe`

Intercepted to keep plan files and descriptions in sync.

- **`jj describe -m "message"`**: The message is written to the plan file first, then the plan file content is flushed to the jj description. This ensures the plan file is always the source of truth.
- **`jj describe` (no `-m`)**: Opens the editor on the jj description. After editing, the plan file is updated to match.
- **`jj describe -r <rev> -m "message"`**: If the target revision has a tracked plan, its plan file is updated.

### `jj abandon`

Intercepted to clean up plan files and registry entries for abandoned changes.

### `jj status` / `jj st`

Intercepted to append the plan stack summary after the normal `jj status` output. This is the primary way developers see their stack state.

### Read-only passthrough commands

Commands like `jj log`, `jj show`, `jj diff`, `jj evolog` pass through transparently. Plan files are synced before the command runs (pre-sync).

### All other mutating commands

Commands like `jj new`, `jj edit`, `jj rebase`, `jj squash`, `jj split`, etc. pass through to jj and then trigger a post-sync to update plan files. This ensures that any structural changes to the DAG are reflected in `.jj-plan/`.

---

## Plan Templates

### Built-in default template

When `jj plan new <bookmark>` creates a plan file, it applies a template:

```markdown
<bookmark>

<!-- Plan: jj:CHANGE_ID -->
```

### Template resolution chain

1. `JJ_PLAN_TEMPLATE` environment variable → path to a template file.
2. `.jj-plan/template.md` file in the repository.
3. Built-in default (above).

### `{{CHANGE_ID}}` and `{{BOOKMARK}}` interpolation

Templates support two placeholders:

- `{{CHANGE_ID}}` — replaced with the new change's ID.
- `{{BOOKMARK}}` — replaced with the bookmark name.

Example template:

```markdown
{{BOOKMARK}}

<!-- Plan: jj:{{CHANGE_ID}} -->

## Background

## Tasks

## Notes [scratch]
```

If a template contains neither placeholder, a `<!-- Plan: jj:CHANGE_ID -->` comment is injected automatically as a self-reference.

### Custom template example

Create `.jj-plan/template.md`:

```markdown
{{BOOKMARK}}

## Goal

## Design

## Checklist

- [ ] Implementation
- [ ] Tests
- [ ] Documentation

## Notes [scratch]
```

---

## Environment Variables

### `JJ_PLAN_DEBUG`

Enable diagnostic logging to stderr. Set to any value.

```sh
JJ_PLAN_DEBUG=1 jj status
```

### `JJ_PLAN_DIR`

Override the plan directory path.

```sh
export JJ_PLAN_DIR=".plans"    # use .plans/ instead of .jj-plan/
```

**Resolution:** The binary checks for `.jj-plan/` first, then `.jj-plans/`. `JJ_PLAN_DIR` overrides both. Can be absolute or relative.

### `JJ_PLAN_MAX`

Maximum stack size before refusing to sync. Prevents accidental sync of enormous stacks.

```sh
export JJ_PLAN_MAX=100         # allow up to 100 plans (default: 50)
```

### `JJ_PLAN_STACK_PREFIX`

Prefix for stack base bookmarks created by `jj plan new --stack`.

```sh
export JJ_PLAN_STACK_PREFIX="stacks/"   # default: "stack/"
```

### `JJ_PLAN_TEMPLATE`

Override the plan template file path.

```sh
export JJ_PLAN_TEMPLATE="$HOME/.config/jj-plan/template.md"
```

### `GITHUB_TOKEN` / `GH_TOKEN`

GitHub personal access token. Used as a fallback when the `gh` CLI is not available.

Required scope: `repo`

### `GITLAB_TOKEN` / `GL_TOKEN`

GitLab personal access token. Used as a fallback when the `glab` CLI is not available.

Required scope: `api`

### `GITEA_TOKEN`

Gitea personal access token. This is the only authentication method for Gitea (no CLI fallback).

Required permissions: repo (read/write)

```sh
export GITEA_TOKEN="your-token-here"
```

### `GH_HOST`

Hostname for GitHub Enterprise instances. Default: `github.com`.

```sh
export GH_HOST="github.mycompany.com"
```

### `GITLAB_HOST`

Hostname for self-hosted GitLab instances. Default: `gitlab.com`.

```sh
export GITLAB_HOST="gitlab.mycompany.com"
```

### `GITEA_HOST`

Hostname for Gitea instances. No default — required for self-hosted Gitea unless the remote URL matches `codeberg.org` or is auto-detected via the `/api/v1/version` probe.

```sh
export GITEA_HOST="gitea.mycompany.com"
```

### `GITEA_DRAFT_PREFIX`

Title prefix used to mark PRs as drafts on Gitea. Gitea versions before ~1.22 silently ignore the `draft` API field; the prefix is the reliable cross-version workaround.

```sh
export GITEA_DRAFT_PREFIX="Draft: "   # default: "WIP: "
```

### `GITEA_INTEGRATION`

Enable Gitea integration tests. Only relevant for development.

```sh
GITEA_INTEGRATION=1 GITEA_HOST=code.example.com GITEA_TOKEN=xxx \
  cargo test --test gitea_integration -- --test-threads=1
```

---

## Recipes

### Submit your first stacked PR

```sh
mkdir .jj-plan                     # activate jj-plan
jj plan new feat-auth              # create first plan
$EDITOR .jj-plan/current.md       # write the plan
# ... implement ...
jj plan new feat-session           # create second plan
$EDITOR .jj-plan/current.md       # write the plan
# ... implement ...
jj stack auth github test          # verify authentication
jj stack submit                    # push and create PRs
```

### Update PRs after making changes

```sh
# Edit code or plan files...
jj stack submit                    # re-push and update existing PRs (base branches only)
jj stack submit --update-descriptions  # also update PR titles/bodies from plan files
# OR
jj stack sync                      # fetch first, then re-submit
```

> **Note:** By default, re-submitting pushes new code and retargets base branches, but does not overwrite PR descriptions. Use `--update-descriptions` to explicitly push plan content to existing PRs. This is opt-in because a colleague may have edited the PR description directly on the platform.

### Merge approved PRs and rebase the stack

```sh
jj stack merge                     # merges from the bottom of the stack
# Automatically: deletes merged bookmarks, fetches trunk, cleans plan files
```

The merge engine polls readiness just-in-time before each merge, handling async forge recomputation after retargets automatically.

### Work with draft PRs

```sh
jj stack submit --draft            # create PRs as drafts
# Later, when ready for review:
jj stack submit --publish          # convert all draft PRs in the stack to ready-for-review
```

> **Platform notes:**
> - **GitHub:** `--publish` uses the GraphQL `markPullRequestReadyForReview` mutation (with `gh pr ready` as fallback).
> - **GitLab:** `--publish` uses `PUT /merge_requests/:iid { "draft": false }`.
> - **Gitea:** `--draft` uses a `WIP:` title prefix (cross-version reliable). `--publish` strips the prefix and sets `draft: false`.

### Handle gap warnings

When unbookmarked changes exist between bookmarks:

```sh
# Option 1: squash the WIP into an adjacent plan
jj squash --from <change> --into feat-auth

# Option 2: give it its own bookmark
jj bookmark create fix-typo -r <change>
jj plan track fix-typo

# Option 3: allow gaps (the WIP diff is absorbed into the next bookmark's PR)
jj stack submit --allow-gaps
```

### Adopt an existing change as a plan

```sh
jj bookmark create feat-search -r <change>   # create a bookmark on the change
jj plan track feat-search                     # register it as a plan
```

### Authenticate with GitHub Enterprise

```sh
export GH_HOST="github.mycompany.com"
gh auth login --hostname github.mycompany.com
jj stack auth github test          # verify it works
```

### Authenticate with self-hosted GitLab

```sh
export GITLAB_HOST="gitlab.mycompany.com"
glab auth login --hostname gitlab.mycompany.com
jj stack auth gitlab test          # verify it works
```

### Authenticate with Gitea

```sh
export GITEA_HOST="gitea.mycompany.com"
export GITEA_TOKEN="your-token-here"
jj stack auth gitea test           # verify it works
```

To create a token: visit `https://<your-host>/user/settings/applications` and create a token with repo read/write permissions.

### Preview submission without making changes

```sh
jj stack submit --dry-run
```

### Work with multiple stacks

```sh
jj plan new --stack auth auth-refactor    # start a new stack
jj plan new auth-tests                    # inherits the "auth" stack
jj edit -r trunk()
jj plan new --stack dashboard dash-api    # start a second stack
jj stack                                  # visualizes both stacks side-by-side
jj stack untrack                          # untrack the current stack
```

### Find all work descended from a plan

```sh
jj log -r 'descendants(kpqxywon)'
```

### Find plan references in code

```sh
grep -r 'jj:' src/ --include='*.rs'
```

### Resolve a plan reference to its full context

```sh
jj show kpqxywon                   # full plan description + diff
```

### List all tracked plan bookmarks

```sh
jj plan config                     # shows PlanRegistry contents
```

### Show what scratch content was stripped

```sh
jj evolog -r kpqxywon              # see all versions, including pre-done
jj show kpqxywon@<operation>       # view a specific version
```

### Navigate the stack by position

```sh
jj plan go 1                       # jump to the first plan (closest to trunk)
jj plan go 3                       # jump to the third plan
jj plan next                       # advance one plan toward tip
jj plan prev                       # go back one plan toward trunk
```

### Inspect resolved configuration

```sh
jj plan config                     # shows plan dir, registry, stack, template
```

### Start a new stack after finishing

```sh
jj plan done --stack               # mark everything done
jj new                             # start fresh on trunk
jj plan new next-feature           # new stack begins
```

### Cross-reference plans

Plans can reference each other by change ID:

```markdown
## Background

This builds on the auth module from jj:kpqxywon.
The session store design was validated in jj:mtzrlpvq.
```

---

## See Also

- [README.md](README.md) — Quick start and overview
- [TECHNICAL.md](TECHNICAL.md) — Architecture and internals