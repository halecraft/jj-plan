# jj-plan Manual

> Exhaustive command reference for `jj plan` and `jj stack`.

For the quick-start guide, see [README.md](README.md). For architecture and internals, see [TECHNICAL.md](TECHNICAL.md).

---

## Overview

**jj-plan** is a transparent shim around [jj](https://github.com/jj-vcs/jj) (Jujutsu) that adds plan-oriented programming and stacked PR support. Install it as `jj` and it intercepts commands to keep `.jj-plan/` markdown files synchronized with change descriptions, then delegates to the real `jj` binary.

The tool provides two command namespaces:

- **`jj plan`** — Create, navigate, and manage implementation plans.
- **`jj stack`** — Visualize, submit, sync, and merge stacked PRs on GitHub and GitLab.

All other `jj` commands pass through transparently with plan file synchronization.

---

## Concepts

### Plans

A **plan** is a jj change whose description contains an implementation plan. The change is bookmarked, and a corresponding markdown file lives in `.jj-plan/`. The file and the description are kept in sync — edits to either propagate automatically.

Plans are written *before* code exists. They contain background, constraints, rationale, rejected alternatives, and concrete tasks. When submitted as a PR, the plan content becomes the PR description.

### Stacks

A **stack** is the set of changes between `trunk()` and the working copy (including descendants): `trunk()..(@  | descendants(@))`. Within this range, bookmarked changes are **plan boundaries** — each bookmark = one plan = one PR.

Unbookmarked changes are free-form work (WIP commits, experiments). They are not managed by jj-plan and are flagged as **gaps** at submit time.

### The `.jj-plan/` Directory

Created by `mkdir .jj-plan` to activate jj-plan in a repository. The binary maintains this directory automatically:

```
.jj-plan/
  current.md          → symlink to the active change's plan file
  .stack              → one-line summary of the full stack
  01-feat-auth.md     — first plan (closest to trunk)
  02-feat-session.md  — second plan
  03-feat-api.md      — third plan (tip)
  template.md         — optional: custom plan template
```

Files are named `NN-BOOKMARKNAME.md` where `NN` is the 1-based position in the stack. Bookmarks containing `/` have slashes encoded as `--` in filenames (e.g., `stack/auth` → `01-stack--auth.md`).

#### Status markers in `.stack`

The `.stack` file uses these markers:

| Marker | Meaning |
|---|---|
| `*` | Working copy is here (`@`) |
| `✓` | Plan is done (`plan-status: ✅` in description) |
| `~` | Plan file has local changes (not yet flushed to description) |

Example:

```
Plan stack (.jj-plan/; *=here ✓=done ~=has changes):
  ✓ 01-feat-auth    :: Extract auth module
  ~ 02-feat-session  :: Implement session management
*   03-feat-api      :: Add API endpoints
```

### Plan Status

A plan is considered **done** when its description contains `plan-status: ✅` on its own line. This is set by `jj plan done` and displayed in the stack summary.

### Working Memory (`[scratch]`)

Any markdown heading containing `[scratch]` marks a **working memory** section. These sections are shared workspace for humans and AI agents during implementation.

```markdown
## Analysis [scratch]

Tried approach A — failed because of X.
Approach B works but needs Y from the API.

### Sub-analysis [scratch]

Deeper investigation of approach B...
```

`jj plan done` strips all `[scratch]` sections from the description, cleaning the archival record. Nested headings within scratch sections are also removed. The full working memory is always recoverable via `jj evolog`.

Scratch sections inside code fences are preserved (the `[scratch]` must be in an ATX heading, not in code).

### Change ID References (`jj:CHANGE_ID`)

Plans and code can reference other plans using `jj:CHANGE_ID`:

```rust
// We bypass the cache here for consistency during concurrent writes.
// Context: jj:kpqxywon
```

Anyone can run `jj show kpqxywon` to retrieve full context. Change IDs are stable across rebases and amends.

In plans, use `jj:CHANGE_ID` to reference sibling plans:

```markdown
## Background

This continues the work from jj:kpqxywon. See that plan for the
constraints on the auth token format.
```

---

## `jj plan` Commands

### `jj plan new <bookmark>`

Create a new plan: a jj change with a bookmark, registered in the PlanRegistry, with a templated plan file.

```
jj plan new feat-auth
jj plan new feat-session
```

**What it does:**

1. Runs `jj new` to create a new change after the current one.
2. Creates a bookmark with the given name on the new change.
3. Registers the bookmark in the PlanRegistry (`.jj/repo/jj-plan/plans.toml`).
4. Seeds the change description from the plan template.
5. Syncs the `.jj-plan/` directory (creates the plan file, updates `current.md`).

**Arguments:**

| Argument | Required | Description |
|---|---|---|
| `<bookmark>` | Yes | Name for the bookmark (e.g., `feat-auth`, `fix/login-bug`) |

**Flags:**

Any flags not consumed by `jj plan new` are passed through to `jj new`:

- `-r <revset>` / `-A <revset>` — insert after a specific change instead of `@`
- `-B <revset>` — insert before a specific change

**Errors:**

- No bookmark name provided → error with usage hint.
- Bookmark already exists → error (use `jj plan track` instead).

---

### `jj plan track <bookmark>`

Register an existing bookmarked change as a plan. Use this to adopt changes that were created outside of `jj plan new`.

```
jj plan track feat-auth
```

**What it does:**

1. Verifies the bookmark exists.
2. Registers it in the PlanRegistry.
3. Syncs the `.jj-plan/` directory.

---

### `jj plan untrack <bookmark>`

Remove a bookmark from plan tracking. The bookmark itself is not deleted — it just stops being managed by jj-plan.

```
jj plan untrack feat-auth
```

---

### `jj plan done`

Mark a plan as done. Strips `[scratch]` sections and appends `plan-status: ✅` to the description. Automatically advances to the next undone plan.

```
jj plan done                   # mark current plan (@) done
jj plan done kpqxywon          # mark a specific change done
jj plan done --stack            # mark all plans in the stack done
```

**Flags:**

| Flag | Description |
|---|---|
| `--stack` | Mark all plans in the stack as done |
| `--keep-scratch` | Don't strip `[scratch]` sections |
| `--dry-run` | Show what would change without writing |

**Behavior:**

1. Reads the plan's description.
2. Strips all `[scratch]` sections (unless `--keep-scratch`).
3. Appends `plan-status: ✅` if not already present.
4. Writes the cleaned description back via `jj describe`.
5. Advances `@` to the next undone plan (if any).

---

### `jj plan next`

Navigate to the next plan in the stack (toward the tip).

```
jj plan next
```

Runs `jj edit` on the next plan segment's tip change. Skips WIP (unbookmarked) changes.

---

### `jj plan prev`

Navigate to the previous plan in the stack (toward trunk).

```
jj plan prev
```

---

### `jj plan go <target>`

Jump to a specific plan by position number, bookmark name, or change ID.

```
jj plan go 2                   # jump to plan #2 (1-based index)
jj plan go feat-auth           # jump to the plan for feat-auth
jj plan go kpqxywon            # jump by change ID
```

---

### `jj plan config`

Show resolved configuration and plan state. Useful for debugging.

```
jj plan config
```

**Output includes:**

- Plan directory path
- PlanRegistry contents (tracked bookmarks)
- Stack state (segments, gaps)
- Template resolution (which template file is active)

---

### `jj plan --help`

Show the help screen with all plan commands, global options, and a brief mental model explanation.

```
jj plan --help
jj plan -h
```

---

## `jj stack` Commands

### `jj stack`

Show the stack visualization with bookmark structure, sync status, and PR status.

```
jj stack
```

**Output:**

```
  ◉ feat-api (@)
  │ Add API endpoints
  │
  ○ feat-session (synced, PR #43)
  │ Implement session management
  │
  ○ feat-auth (synced, PR #42)
  │ Extract auth module
  │
  ◆ trunk()
```

**Legend:**

| Symbol | Meaning |
|---|---|
| `◉` | Working copy (`@`) |
| `○` | Other bookmarked change |
| `◆` | Trunk (base of stack) |
| `@` | Working copy indicator |
| `✓` | Plan is done |
| `synced` | Bookmark is pushed to remote |
| `PR #N` | PR exists for this bookmark (from PR cache) |

If the stack has gaps (unbookmarked changes between bookmarks), a warning is shown.

---

### `jj stack submit [bookmark]`

Push bookmarks and create or update PRs on GitHub or GitLab.

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

The platform (GitHub or GitLab) is auto-detected from git remote URLs. Self-hosted instances are supported via `GH_HOST` and `GITLAB_HOST` environment variables.

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

Merge approved PRs from the bottom of the stack upward.

```
jj stack merge                 # merge all approved PRs
jj stack merge --dry-run       # preview the merge plan
jj stack merge --remote upstream
```

**What it does:**

1. Fetches PR details and merge readiness for all bookmarks in the stack.
2. Creates a merge plan: starting from the bottom, merge each PR that is approved and passing CI. Stop at the first non-mergeable PR.
3. Executes the merges via the platform API.
4. Post-merge cleanup:
   - Removes merged bookmarks from the PR cache.
   - Deletes merged local bookmarks.
   - Removes merged plan files from `.jj-plan/`.
   - Fetches the updated trunk.

**Merge readiness requirements:**

A PR is ready to merge when:

- It is approved (at least one approving review on GitHub, or approved on GitLab).
- CI is passing (commit statuses + check runs on GitHub, pipeline on GitLab).
- It is not a draft.
- It has no merge conflicts.

**Flags:**

| Flag | Description |
|---|---|
| `--dry-run` | Preview the merge plan without merging |
| `--remote <name>` | Specify the remote |

---

### `jj stack auth <platform> <action>`

Manage authentication for GitHub and GitLab.

```
jj stack auth github test      # test GitHub authentication
jj stack auth github setup     # show GitHub setup instructions
jj stack auth gitlab test      # test GitLab authentication
jj stack auth gitlab setup     # show GitLab setup instructions
```

**Token resolution order:**

| Platform | Priority |
|---|---|
| GitHub | `gh auth token` → `GITHUB_TOKEN` → `GH_TOKEN` |
| GitLab | `glab auth token` → `GITLAB_TOKEN` → `GL_TOKEN` |

---

## Intercepted Commands

The following `jj` commands receive special handling before being delegated to the real `jj` binary.

### `jj describe`

When invoked with `-m` / `--message`, the message is written to the plan file first, then the normal `jj describe` runs. This ensures the plan file remains the source of truth.

```sh
jj describe -m "Updated approach: use JWT instead of sessions"
# Writes to the plan file, THEN delegates to jj describe
```

Without `-m`, the command passes through normally (opens the editor on the raw description).

### `jj abandon`

Delegated to the real `jj` with post-mutation plan sync. If a plan's change is abandoned, the corresponding plan file is removed on the next sync.

### `jj status` / `jj st`

Runs the real `jj status`, then appends the plan stack summary from `.jj-plan/.stack`.

### Read-only passthrough commands

These commands get zero-overhead passthrough via `exec` (the process is replaced, no plan sync):

`log`, `diff`, `show`, `interdiff`, `evolog`, `file`, `config`, `help`, `version`, `root`, `tag`, `op`, `operation`, `util`, `git`, `gerrit`, `sign`, `unsign`, `workspace`

### All other mutating commands

Commands like `new`, `edit`, `rebase`, `squash`, `split`, `bookmark`, etc. go through the **wrap lifecycle**:

1. **Flush** — Write local plan file edits to jj descriptions.
2. **Run** — Execute the real `jj` command.
3. **Reload** — Refresh the in-process repo snapshot.
4. **Sync** — Update `.jj-plan/` files from the repo state.
5. **Show** — Display the updated stack summary.

---

## Plan Templates

### Built-in default template

New plans are seeded with a single self-referencing summary line:

```
feat: <brief summary>
```

### Template resolution chain

1. `$JJ_PLAN_TEMPLATE` environment variable (path to a file).
2. `.jj-plan/template.md` in the repository.
3. Built-in default (single summary line).

### `{{CHANGE_ID}}` and `{{BOOKMARK}}` interpolation

Templates can include `{{CHANGE_ID}}` (replaced with the new change's short ID) and `{{BOOKMARK}}` (replaced with the bookmark name):

```markdown
feat: {{BOOKMARK}}

## Background

Context: jj:{{CHANGE_ID}}

## Tasks

- [ ] ...

## Notes [scratch]
```

### Custom template example

Create `.jj-plan/template.md`:

```markdown
feat: {{BOOKMARK}}

## Goal

What should be true when this is done?

## Design

How will it work?

## Checklist

- [ ] Implementation
- [ ] Tests
- [ ] Documentation

## Notes [scratch]

Working memory goes here.
```

---

## Environment Variables

### `JJ_PLAN_DIR`

Override the plan directory path.

```sh
export JJ_PLAN_DIR=".plans"    # use .plans/ instead of .jj-plan/
```

**Resolution:** The binary checks for `.jj-plan/` first, then `.jj-plans/`. `JJ_PLAN_DIR` overrides both.

### `JJ_PLAN_MAX`

Maximum stack size before refusing to sync. Prevents accidental sync of enormous stacks.

```sh
export JJ_PLAN_MAX=100         # allow up to 100 plans (default: 50)
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

### Work with draft PRs

```sh
jj stack submit --draft            # create PRs as drafts
# Later, when ready for review:
jj stack submit --publish          # convert all draft PRs in the stack to ready-for-review
```

> **Note:** `--publish` uses GitHub's GraphQL `markPullRequestReadyForReview` mutation (with `gh pr ready` as fallback) and GitLab's `PUT /merge_requests/:iid { "draft": false }`.

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

### Preview submission without making changes

```sh
jj stack submit --dry-run
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