# jj-plan: Plan-Oriented Programming

> Plans are VCS artifacts — reviewed before code exists, permanently linked to the code they produce.

**jj-plan** stores implementation plans as [Jujutsu](https://github.com/jj-vcs/jj) change descriptions and syncs them to markdown files in your editor. Plans are drafted, reviewed, and validated *before* code is written. Code links back to plans via stable change IDs. The VCS *is* the documentation.

```
Plan stack (.jj-plan/; *=here ✓=done ~=has changes):
  ✓ 01-kpqxywon :: Refactor auth middleware
  ~ 02-mtzrlpvq :: Extract auth module
*   03-ykvsnxrl :: Implement JWT strategy
    04-abcdefgh :: Add API key support
```

---

## Quick Start

### Install

```sh
# 1. Install to ~/.local/bin by default
./install.sh

# 2. Or choose a different destination directory
./install.sh --bin-dir /usr/local/bin

# 3. Add .jj-plan to your global gitignore
echo '.jj-plan' >> ~/.config/git/ignore
```

### The workflow

```sh
# Activate in any jj repo
mkdir .jj-plan

# Start a named stack — creates a change + bookmark
jj plan stack my-feature

# Write the plan (your editor, an AI agent, or both)
$EDITOR .jj-plan/current.md

# Add more plans to the stack
jj plan new                    # insert after current
jj plan new --last             # append to end

# Implement — every jj command syncs the plan files automatically
# Edit code... jj status shows your plan stack on every invocation

# Navigate the stack
jj plan next                   # advance to next plan
jj plan prev                   # go back
jj plan go 2                   # jump to plan #2

# Mark done — strips [scratch] working memory, advances to next undone plan
jj plan done

# Mark entire stack done
jj plan done --stack
```

Every `jj` command you run — `status`, `new`, `edit`, `rebase` — automatically syncs the `.jj-plan/` directory with change descriptions. The plan files are always the source of truth.

### What you see

`jj status` appends the plan stack to its output:

```
Working copy  (@) : ykvsnxrl 3a7b2c1d Implement JWT strategy
Parent commit (@-): mtzrlpvq 8f2e4a6b Extract auth module

Plan stack (.jj-plan/; *=here ✓=done ~=has changes):
  ✓ 01-kpqxywon :: Refactor auth middleware
  ~ 02-mtzrlpvq :: Extract auth module
*   03-ykvsnxrl :: Implement JWT strategy
    04-abcdefgh :: Add API key support
```

Status markers: `*` = you are here, `✓` = done, `~` = has file changes.

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
            → Code links back via jj:CHANGE_ID
```

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
Plan (jj description) → Code (references jj:CHANGE_ID) → Archaeology (jj show → full plan)
```

No context is lost. No documentation rots.

Because plans are structured markdown documents stored in version history, your implementation history becomes a queryable library of decisions, post-implementation reflections, alternatives considered, and patterns. Humans and AI can pull the relevant section from prior work without rereading entire plans.

---

## Why Plan-Oriented Programming?

Most workflows treat code as the artifact and plans as ephemeral scaffolding — a Google Doc, a Notion page, a Slack discussion, forgotten the moment the code lands.

Plan-oriented programming inverts this. **The plan is the artifact.** Code is its expression.

The workflow:

1. **Draft** — Write an implementation plan: background, constraints, rationale, rejected alternatives, concrete steps.
2. **Cross-check** — A second human, AI agent, designer, or PM reviews the plan for correctness and completeness.
3. **Validate** — Iterate until the group has confidence. This happens *before a single line of code exists*.
4. **Implement** — Code is written, guided by the plan.
5. **Complete** — `jj plan done` strips working memory, archives the clean plan in version history.
6. **Archive** — The plan remains in version history, permanently linked to the code it produced.

Plans stored outside the VCS suffer from three problems:

- **Drift** — The plan says one thing, the code does another, and no one updates either.
- **Disconnection** — Six months later, `git blame` shows "refactor auth middleware" and the reasoning is gone.
- **Inaccessibility** — The AI agent writing code can't read your Google Doc. The new team member can't find the Slack thread.

When the plan lives *in* the VCS, it travels with the code. It's versioned. It's queryable. It's right where you need it.

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

Each change in a stack is one unit of work: the description *is* the plan, the diff *is* the implementation.

```sh
jj plan stack my-feature       # start a named stack (creates change + bookmark)
# Edit .jj-plan/current.md — write the plan
jj plan new                    # add another plan change (templated)
jj plan done                   # mark current plan done, strip working memory
```

New plans are seeded with a structured template (Background, Approach, Tasks, Scratchpad). The template is customizable via `.jj-plan/template.md` or the `JJ_PLAN_TEMPLATE` environment variable.

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

### Plans split naturally across PRs

When a plan grows beyond one PR, create new plan changes referencing the original:

```sh
jj plan stack phase2
# Edit .jj-plan/current.md — "Phase 2 — continues jj:kpqxywon"
```

The lineage is preserved through change ID references.

### The `.jj-plan/` directory

The binary maintains a directory of markdown files synced with change descriptions:

```
.jj-plan/
  current.md          → symlink to active change's plan
  .stack              → one-line summary of the full stack
  01-kpqxywon.md      — first stack member
  02-mtzrlpvq.md
  03-ykvsnxrl.md      — tip
  template.md         — optional: custom plan template
```

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
| Plans co-exist with code review | Plan changes have rich descriptions — they *are* the review |
| Plans split across PRs | New plan changes reference originals by change ID |
| Plans have status tracking | `plan-status: ✅` in description; inferred from empty/non-empty |
| Plans have working memory | `[scratch]` sections for drafts; `jj plan done` strips them |
| `jj describe` works naturally | `-m` mode intercepted and routed through plan files |
| Stack is always visible | `jj status` appends the plan stack summary |
| Stack is navigable | `jj plan next`/`prev`/`go` for index-based movement |
| Multiple concurrent stacks | `stack/*` bookmarks with nearest-ancestor resolution |
| Stack bookmark is intuitive | Bookmark is ON the first change, not before it (inclusive) |
| New plans are structured | Configurable templates with `{{CHANGE_ID}}` interpolation |

## Stack Bookmarks

The binary uses `stack` / `stack/*` bookmarks to identify which changes belong to the current plan stack. The bookmark is placed **on the first change in the stack** — the bookmarked change is included as a stack member.

- **`stack`** (bare) — a quick unnamed stack. Use when you have one stack at a time.
- **`stack/my-feature`** (named) — a named stack for concurrent work. `jj bookmark list stack/*` shows all active stacks.

When you finish a stack, just start a new one:

```sh
jj plan stack next-task
```

The old bookmark stays — the binary automatically picks the nearer bookmark as the active stack base. No cleanup needed.

If no `stack` bookmark is an ancestor of `@`, the binary falls back to `trunk()` with an exclusive range (the trunk commit is not part of your stack).

For details on the resolution algorithm (nearest-ancestor selection, ambiguous bookmark handling, trunk fallback), see [TECHNICAL.md](TECHNICAL.md#stack-bookmarks).

## Development

### Running tests

```sh
./test.sh              # build + run 138 bats tests (8 parallel jobs)
./test.sh --jobs=4     # fewer parallel jobs (if system is constrained)
./test.sh --no-build   # skip cargo build, just run tests
```

Or manually:

```sh
cargo build --release
bats jj-plan.bats --jobs 8     # parallel (requires: brew install parallel)
bats jj-plan.bats              # sequential (~54s)
cargo test                     # 87 unit tests (<1s)
```

Parallel execution requires GNU `parallel`:

```sh
brew install parallel          # macOS
```

### Test architecture

- **Template repo**: A jj repo with `stack` bookmark + `.jj-plan/` is created once per run. Each test gets an isolated `cp -r` copy (~2ms).
- **Direct bats style**: Tests run commands inline — no wrapper functions, no subshells. Assertions check values directly (`[[ "$(cat file)" == "expected" ]]`).
- **Parallel-safe**: Every test operates in its own temp directory. No shared mutable state.

## Documentation

Use `jj plan --help` for the compact terminal summary. Use the docs below when you want either the full user reference or the implementation details.

| Document | Audience | Content |
|---|---|---|
| [MANUAL.md](MANUAL.md) | Users | Exhaustive command reference, best practices, recipes |
| [TECHNICAL.md](TECHNICAL.md) | Contributors | Architecture, internals, performance, testing |

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DIR` | Override plan directory path (absolute or relative) | Auto-resolved: `.jj-plan/` → `.jj-plans/` |
| `JJ_PLAN_MAX` | Maximum stack size before refusing to sync | `50` |
| `JJ_PLAN_TEMPLATE` | Override plan template file path | `.jj-plan/template.md` → built-in default |