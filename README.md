# jj-plan: Plan-Oriented Programming

> Plans are VCS artifacts — reviewed before code exists, permanently linked to the code they produce. Plans become PR descriptions automatically.

**jj-plan** stores implementation plans as [Jujutsu](https://github.com/jj-vcs/jj) change descriptions and syncs them to markdown files in your editor. Plans are drafted, reviewed, and validated *before* code is written. When you're ready, `jj stack submit` pushes your stack as PRs — with the plan as the PR description.

```
  ◉ feat-session mtzrlpvq (@)
  │ Implement session management
  │
  ○ feat-auth kpqxywon (synced, PR #42)
  │ Extract auth module
  │
  ◆ trunk()
```

---

## Quick Start

### Install

```sh
# Install to ~/.local/bin by default
./install.sh

# Or choose a different destination directory
./install.sh --bin-dir /usr/local/bin

# Add .jj-plan to your global gitignore
echo '.jj-plan' >> ~/.config/git/ignore
```

### The workflow

```sh
# Activate in any jj repo
mkdir .jj-plan

# Create a plan — makes a jj change + bookmark + plan file
jj plan new feat-auth

# Write the plan (your editor, an AI agent, or both)
$EDITOR .jj-plan/current.md

# Implement — every jj command syncs plan files automatically
# Edit code... jj status shows your plan stack on every invocation

# Add more plans to the stack
jj plan new feat-session

# Navigate the stack
jj plan next                   # advance to next plan
jj plan prev                   # go back
jj plan go 2                   # jump to plan #2

# Submit as stacked PRs — plan content becomes the PR description
jj stack submit

# Mark done — strips [scratch] working memory, advances to next
jj plan done

# Sync with remote (fetch + re-submit)
jj stack sync

# Merge approved PRs from the bottom of the stack
jj stack merge
```

Every `jj` command you run — `status`, `new`, `edit`, `rebase` — automatically syncs the `.jj-plan/` directory with change descriptions. Plan files are always the source of truth.

### What you see

`jj status` appends the plan stack:

```
Working copy  (@) : ykvsnxrl 3a7b2c1d Implement session management
Parent commit (@-): mtzrlpvq 8f2e4a6b Extract auth module

Plan stack (.jj-plan/; *=here ✓=done ~=has changes):
  ✓ 01-feat-auth kpqxywon :: Extract auth module
  ~ 02-feat-session mtzrlpvq :: Implement session management
*   03-feat-api ykvsnxrl :: Add API endpoints
```

`jj stack` shows the PR-aware visualization:

```
  ◉ feat-api ykvsnxrl (@)
  │ Add API endpoints
  │
  ○ feat-session mtzrlpvq (synced, PR #43)
  │ Implement session management
  │
  ○ feat-auth kpqxywon (synced, PR #42)
  │ Extract auth module
  │
  ◆ trunk()
```

---

## AI-Native Workflow

An AI agent generating code needs *intent*, not just a task. "Implement JWT auth" is a task. A plan contains the background (why JWT, not session tokens), the constraints (must support RS256 and EdDSA), the rejected alternatives (API keys don't support rotation), and the step-by-step approach.

### The collaboration loop

```
Human writes draft plan
  → AI cross-checks: "Step 3 has a race condition if..."
    → Human revises plan
      → PM confirms scope: "Phase 2 can wait for Q4"
        → AI implements, guided by the finalized plan
          → jj plan done strips scratch, archives the clean plan
            → jj stack submit — plan becomes the PR description
              → Code links back via jj:CHANGE_ID
```

### Plans become PR descriptions

When you run `jj stack submit`, the plan file content becomes the PR title and body:

- **PR title** = first line of the plan file
- **PR body** = the rest, with `[scratch]` sections stripped and `plan-status: ✅` lines removed

This is the plan-oriented programming payoff — the plan *is* the PR description. Update the plan, re-submit, and the PR description updates too.

### Shared working memory

`[scratch]` sections in plans are shared working memory between human and AI:

```markdown
## Analysis [scratch]

Tried approach A — failed because of X.
Approach B works but needs Y from the API.
```

The AI writes analysis, alternatives explored, and debugging notes in scratch sections during implementation. `jj plan done` strips these from the archival record. The full working memory is always recoverable via `jj evolog`.

### The closed loop

When the agent reads `.jj-plan/current.md`, it has the full decision record. When it's done, the clean plan becomes the permanent historical record. The code links back to it:

```
Plan (jj description) → Code (references jj:CHANGE_ID) → PR (plan = description) → Archaeology (jj show → full plan)
```

No context is lost. No documentation rots.

---

## Why Plan-Oriented Programming?

Most workflows treat code as the artifact and plans as ephemeral scaffolding — a Google Doc, a Notion page, a Slack discussion, forgotten the moment the code lands.

Plan-oriented programming inverts this. **The plan is the artifact.** Code is its expression. PRs are its review surface.

The workflow:

1. **Draft** — Write an implementation plan: background, constraints, rationale, rejected alternatives, concrete steps.
2. **Cross-check** — A second human, AI agent, designer, or PM reviews the plan.
3. **Validate** — Iterate until the group has confidence. This happens *before a single line of code exists*.
4. **Implement** — Code is written, guided by the plan.
5. **Submit** — `jj stack submit` pushes the stack as PRs, with plan content as descriptions.
6. **Complete** — `jj plan done` strips working memory, archives the clean plan in version history.

Plans stored outside the VCS suffer from three problems:

- **Drift** — The plan says one thing, the code does another, and no one updates either.
- **Disconnection** — Six months later, `git blame` shows "refactor auth middleware" and the reasoning is gone.
- **Inaccessibility** — The AI agent writing code can't read your Google Doc. The new team member can't find the Slack thread.

When the plan lives *in* the VCS, it travels with the code. It's versioned. It's queryable. It's right where you need it. And when it's time for review, the plan *is* the PR description.

## The jj Advantage

This workflow requires one thing git cannot provide: **stable references to changes**.

Git commit hashes are content-addressable — rebase, amend, or cherry-pick, and the hash changes. Any reference to it becomes a dead link.

[jj](https://github.com/jj-vcs/jj) (Jujutsu) change IDs are assigned at creation and **never change**, regardless of rebases, amends, or rewrites:

1. A change ID is a **permanent address** for a plan.
2. Code comments can **link back** to plans with `jj:CHANGE_ID`.
3. Anyone can run `jj show CHANGE_ID` to retrieve full context — forever.
4. Plans can **reference each other** by change ID, forming a navigable knowledge graph.

```rust
// We bypass the cache here for consistency during concurrent writes.
// Context: jj:kpqxywon
```

Plans aren't metadata *about* changes. Plans *are* changes.

---

## How It Works

### Plans are changes with rich descriptions

Each bookmarked change in a stack is one unit of work: the description *is* the plan, the diff *is* the implementation, and the bookmark name *is* the PR branch.

```sh
jj plan new feat-auth          # create a change + bookmark + plan file
# Edit .jj-plan/current.md — write the plan
jj plan new feat-session       # add another plan to the stack
jj plan done                   # mark current plan done
jj stack submit                # push as stacked PRs
```

New plans are seeded with a minimal self-referencing summary line. For structured sections (Background, Tasks, etc.), create a `.jj-plan/template.md` or set the `JJ_PLAN_TEMPLATE` environment variable.

### Plans are reviewed before code exists

The plan change is the review surface. Collaborators read the description and respond:

- A **designer** validates UX implications.
- A **PM** confirms scope and priority.
- An **AI agent** cross-checks feasibility and identifies edge cases.
- A **peer engineer** challenges assumptions and checks architectural fit.

### Code references plans by change ID

```rust
// Context: jj:kpqxywon
```

`jj show kpqxywon` retrieves the full plan — the *why* behind the code. The comment is terse; the plan is deep. Anyone doing code archaeology can follow the link and recover full context.

### Stacked PRs

Each bookmarked plan becomes a PR. `jj stack submit` pushes the entire stack:

```sh
jj stack submit                # submit the full stack as PRs
jj stack submit feat-auth      # submit up to a specific bookmark
jj stack submit --dry-run      # preview without making changes
jj stack submit --draft        # create PRs as drafts
```

The base branch of each PR is automatically set to the previous bookmark (or the default branch for the first). When you update a plan and re-submit, existing PRs are updated in place.

### The `.jj-plan/` directory

The binary maintains a directory of markdown files synced with change descriptions:

```
.jj-plan/
  current.md          → symlink to active change's plan
  .stack              → one-line summary of the full stack
  01-feat-auth.md     — first plan (closest to trunk)
  02-feat-session.md
  03-feat-api.md      — tip
  template.md         — optional: custom plan template
```

Files are named `NN-BOOKMARKNAME.md` where `NN` is the position in the stack. Bookmarks containing `/` have slashes encoded as `--` in filenames (e.g., `stack/auth` → `01-stack--auth.md`).

You and an AI agent both edit these markdown files — no `jj describe` clobbering, no modal editor sessions. The binary flushes edits to jj descriptions automatically. `jj describe -m "..."` is also intercepted and routed through the plan file, so the file is always the source of truth.

### Working memory has a cleanup lifecycle

Any heading marked `[scratch]` is working memory. `jj plan done` strips all scratch sections, cleaning the archival record while preserving conclusions. The full working memory is always recoverable via `jj evolog`.

---

## Key Properties

| Property | Mechanism |
|---|---|
| Plans are reviewed before code | Plan changes are the review surface |
| Plans survive rebase/amend | jj change IDs are stable |
| Plans are permanently addressable | `jj show CHANGE_ID` from any code comment |
| Plans are co-editable (human + AI) | `.jj-plan/` syncs markdown files ↔ descriptions |
| Plans become PR descriptions | `jj stack submit` uses plan content as PR title + body |
| Stacked PRs | `jj stack submit/sync/merge` for full PR lifecycle |
| Gap detection at submit time | Unbookmarked changes flagged; `--allow-gaps` to override |
| Plans have status tracking | `plan-status: ✅` in description; inferred from empty/non-empty |
| Plans have working memory | `[scratch]` sections for drafts; `jj plan done` strips them |
| `jj describe` works naturally | `-m` mode intercepted and routed through plan files |
| Change IDs visible in stack | `.stack` and `jj stack` show short reverse-hex IDs — copy-paste into `jj show`, `jj edit`, or `jj:` references |
| Stack is always visible | `jj status` appends the plan stack summary |
| Stack is navigable | `jj plan next`/`prev`/`go` for index, change ID, or bookmark-based movement |
| New plans are structured | Configurable templates with `{{CHANGE_ID}}` and `{{BOOKMARK}}` interpolation |
| GitHub and GitLab support | Platform auto-detected from git remote URLs |

## Stack Model

The stack is everything between `trunk()` and your working copy (including descendants): `trunk()..(@  | descendants(@))`. Bookmarks mark PR boundaries — each bookmark = one plan = one PR.

- **`jj plan new <bookmark>`** creates a new change with a bookmark and registers it as a plan.
- **`jj plan track <bookmark>`** registers an existing bookmarked change as a plan.
- **`jj plan untrack <bookmark>`** removes a bookmark from plan tracking (the bookmark itself remains).

Unbookmarked changes between bookmarks are *not* plans — they're free-form work (WIP commits, experiments). At submit time, `jj stack submit` flags these as **gaps**:

```
Error: unbookmarked changes detected between bookmarks.

  change xyzw (between feat-auth and feat-session)
    "wip: debugging auth flow"

Options:
  - Squash into adjacent bookmark: jj squash --from xyzw --into feat-auth
  - Give it its own bookmark:      jj bookmark create <name> -r xyzw
  - Allow gaps explicitly:          jj stack submit --allow-gaps
```

This ensures every PR has a clean, intentional scope.

## Development

### Running tests

```sh
cargo test                     # unit tests (<1s)
./test.sh                      # build + run bats integration tests (parallel)
./test.sh --jobs=4             # fewer parallel jobs
./test.sh --no-build           # skip cargo build, just run tests
```

Or manually:

```sh
cargo build --release
bats jj-plan.bats --jobs 8     # parallel (requires: brew install parallel)
bats jj-plan.bats              # sequential
```

### Test architecture

- **Template repo**: A jj repo with `.jj-plan/` is created once per run. Each test gets an isolated `cp -r` copy (~2ms).
- **Direct bats style**: Tests run commands inline — no wrapper functions, no subshells.
- **Parallel-safe**: Every test operates in its own temp directory. No shared mutable state.
- **Unit tests**: 198 Rust tests covering types, stack builder, plan registry, PR cache, sync, flush, markdown processing, plan file operations, and platform detection.
- **Integration tests**: 125 bats tests covering end-to-end CLI behavior, plan file sync, stack display, navigation, abandon recovery, config, templates, and encoded bookmark names.

## Documentation

Use `jj plan --help` for the compact terminal summary. Use the docs below when you want the full reference or implementation details.

| Document | Audience | Content |
|---|---|---|
| [MANUAL.md](MANUAL.md) | Users | Exhaustive command reference for `jj plan` and `jj stack`, best practices, recipes |
| [TECHNICAL.md](TECHNICAL.md) | Contributors | Architecture, internals (workspace, stack builder, platform layer, submit/merge engines), performance, testing |

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DEBUG` | Enable diagnostic logging to stderr (any value) | unset |
| `JJ_PLAN_DIR` | Override plan directory path (absolute or relative) | Auto-resolved: `.jj-plan/` → `.jj-plans/` |
| `JJ_PLAN_MAX` | Maximum stack size before refusing to sync | `50` |
| `JJ_PLAN_TEMPLATE` | Override plan template file path | `.jj-plan/template.md` → built-in default |
| `GITHUB_TOKEN` | GitHub personal access token (fallback for `gh` CLI) | — |
| `GH_TOKEN` | GitHub token (alternative to `GITHUB_TOKEN`) | — |
| `GITLAB_TOKEN` | GitLab personal access token (fallback for `glab` CLI) | — |
| `GL_TOKEN` | GitLab token (alternative to `GITLAB_TOKEN`) | — |
| `GH_HOST` | GitHub Enterprise hostname | `github.com` |
| `GITLAB_HOST` | Self-hosted GitLab hostname | `gitlab.com` |