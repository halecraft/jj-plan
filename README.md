# jj-plan: Plan-Oriented Programming

> Plans as VCS Artifacts

**TL;DR**: In plan-oriented programming, the plan is the primary artifact — reviewed, debated, and validated by humans, AI agents, designers, and PMs *before any code is written*. By storing plans as Jujutsu change descriptions (jj is a VCS, like git), they become permanent, addressable nodes in version history. A Rust binary makes them co-editable as markdown files in your editor.

---

## The Paradigm: Plan-Oriented Programming

Most software workflows treat code as the artifact and plans as ephemeral scaffolding. A plan lives in a Google Doc or a Notion page, gets discussed in Slack, maybe referenced in a PR description, and is forgotten the moment the code lands.

Plan-oriented programming inverts this. **The plan is the artifact.** Code is its expression.

The workflow:

1. **Draft** — A human or AI agent writes an implementation plan: background, constraints, rationale, rejected alternatives, concrete steps.
2. **Cross-check** — A second prompt, or AI agent, or human reviews the plan for correctness, feasibility, and completeness. Designers and PMs weigh in on intent and scope.
3. **Validate** — The plan is iterated on. Steps are refined. Edge cases are surfaced. This happens *before a single line of code exists*.
4. **Implement** — Only now does code get written, guided by the plan. The plan is the specification, the context, and the historical record.
5. **Complete** — `jj plan done` marks the plan as finished, strips working memory (`[scratch]` sections), and archives the clean plan in version history. `jj evolog` recovers the full scratch content if needed.
6. **Archive** — The plan remains in version history, permanently linked to the code it produced.

This is not waterfall. Plans are living documents that evolve during implementation. But the key shift is that **planning is a first-class phase with its own artifact**, not a mental prelude to typing code.

## Why Plans Need a Home in the VCS

Plans stored outside the VCS (wikis, docs, chat) suffer from three problems:

- **Drift**: The plan says one thing, the code does another, and no one updates either.
- **Disconnection**: Six months later, `git blame` shows "refactor auth middleware" and the reasoning is gone.
- **Inaccessibility**: The AI agent writing code can't read your Google Doc. The new team member can't find the Slack thread.

When the plan lives *in* the VCS — as a change description — it travels with the code. It's versioned. It's queryable — tools can index every line of code against the plan it came from (i.e. `git blame`). It's right where you need it.

## The jj Insight: Stable Change IDs

This workflow requires one thing that git cannot provide: **stable references to changes**.

Git commit hashes are content-addressable. Rebase, amend, or cherry-pick, and the hash changes. Any reference to it — in a comment, a doc, another commit message — becomes a dead link.

jj (Jujutsu) change IDs are assigned at creation and **never change**, regardless of rebases, amends, or rewrites. This means:

1. A change ID is a **permanent address** for a plan.
2. Code comments can **link back** to plans with `jj:CHANGE_ID`.
3. Anyone can run `jj show CHANGE_ID` to retrieve full context — forever.
4. Plans can **reference each other** by change ID, forming a navigable knowledge graph.

Plans aren't metadata *about* changes. Plans *are* changes.

## The Method

### 1. A plan is a change with a rich description

Each change in a stack is one unit of work: the description *is* the plan, the diff *is* the implementation.

```
jj plan stack my-feature                 # Start a new named stack (creates change + bookmark)
# Edit .jj-plan/current.md — write the plan
jj plan new                              # Add a plan change to the stack (templated)
jj plan new --first                      # Insert a plan change at the start of the stack
jj plan new --last                       # Insert a plan change at the end of the stack
jj plan done                             # Mark current plan as done, strip working memory
```

Or without a name: `jj plan stack` sets a bare `stack` bookmark. You can also root a stack off a specific revision: `jj plan stack -r main my-feature`.

The plan lives in the change description — a markdown document with background, constraints, alternatives considered, and step-by-step approach. New plans are created with a structured template that includes Background, Approach, Tasks, and a Scratchpad section. The `stack` bookmark marks the first change in the stack, and it is **included** as a stack member.

### 2. Plans are reviewed before code exists

The plan change is the review surface. Collaborators read the description and respond:

- A **designer** validates that the UX implications are understood.
- A **PM** confirms scope and priority alignment.
- An **AI agent** cross-checks feasibility, identifies missing edge cases, or proposes alternative approaches.
- A **peer engineer** challenges assumptions and checks for architectural fit.

This review happens on the plan change itself — the description is edited, refined, and iterated on until the group has confidence. Only then does implementation begin.

### 3. Code references plans by change ID

```rust
// We bypass the cache here for consistency during concurrent writes.
// Context: jj:kpqxywon
```

This is a permanent, resolvable link. `jj show kpqxywon` retrieves the full plan — the *why* behind the code. The comment is terse; the plan is deep. Anyone doing code archaeology can follow the link and recover full context.

### 4. Plans split naturally across PRs

When a plan grows beyond one PR, create new plan changes referencing the original:

```
jj plan stack phase2
# Edit .jj-plan/current.md — "Phase 2 — API key support (continues jj:kpqxywon)"
```

The lineage is preserved through change ID references. Each phase can be reviewed, implemented, and landed independently while maintaining a navigable thread back to the original intent.

### 5. A `.jj-plan/` directory makes plans co-editable

The jj-plan binary intercepts `jj` commands and maintains a `.jj-plan/` directory:

```
.jj-plan/
  current.md          → symlink to active change's plan
  .stack              → one-line summary of the full stack
  01-kpqxywon.md      — stack bookmark (first member)
  02-mtzrlpvq.md
  03-ykvsnxrl.md      — tip
  template.md         — optional: custom plan template
```

You and an AI agent both edit these markdown files in the editor — no `jj describe` clobbering, no modal editor sessions. The binary flushes edits to jj descriptions automatically. `jj describe -m "..."` is also intercepted and routed through the plan file, so the file is always the source of truth. `jj status` shows the stack at a glance:

```
Plan stack (.jj-plan/; *=here ✓=done ~=has changes):
  ✓ 01-kpqxywon :: Refactor auth middleware
  ~ 02-mtzrlpvq :: Extract auth module
*   03-ykvsnxrl :: Implement JWT strategy
    04-abcdefgh :: Add API key support
```

### 6. Plans have working memory with a cleanup lifecycle

Any heading in a plan can be marked `[scratch]` to designate it as working memory:

```markdown
## Analysis [scratch]

Tried approach A — failed because of X.
Approach B works but needs Y from the API.
```

Working memory lives near the content it supports. When the plan is complete, `jj plan done` strips all `[scratch]` sections from the description, cleaning the archival record while preserving conclusions. The full working memory is always recoverable via `jj evolog` (jj's evolution log).

This gives plans a natural lifecycle: draft with scratch notes → implement → clean up → archive.

## Stack Bookmarks

The binary uses `stack` / `stack/*` bookmarks to determine which changes belong to the current plan stack.

### Inclusive model

The bookmark is placed **on the first change in the stack**, not before it. The bookmarked change is a stack member. This is the natural instinct — you mark the work itself:

```
○ landed work
○ feat: SchemaRef          ← stack/typed-interpret (first member)
○ feat: typed interpret
○ refactor: remove casts
@ docs: update TECHNICAL
```

### Bare `stack` vs named `stack/*`

- **`stack`** (bare) — a quick unnamed stack. Use when you only have one stack at a time.
- **`stack/my-feature`** (named) — a named stack. Use when you have concurrent work. `jj bookmark list stack/*` shows all active stacks.

### Nearest-ancestor resolution

When multiple `stack` / `stack/*` bookmarks are ancestors of `@`, the **nearest** one wins automatically. This means you can have multiple stacks in your history and the binary always picks the right one:

```
○ stack/phase1 (old, farther from @)
○ phase 1 work
○ stack/phase2 (nearer to @, wins)
○ phase 2 work
@ current
```

If two `stack/*` bookmarks are equidistant siblings (e.g., a merge of two branches each with their own bookmark), an error is produced asking you to advance or remove one.

### Fallback to `trunk()`

If no `stack` / `stack/*` bookmark is an ancestor of `@`, the binary falls back to `trunk()` (if it resolves to something other than `root()`). The trunk fallback uses an **exclusive** range — the trunk commit is not part of your stack, since you don't own it.

If neither a stack bookmark nor `trunk()` can be resolved, no sync occurs.

### "Done" workflow

When you finish a stack, you don't need to move or delete any bookmarks. Just start a new stack:

```sh
jj plan stack next-task                # atomic: creates change + sets stack/next-task bookmark
```

Or without a name: `jj plan stack`. Or rooted off a specific revision: `jj plan stack -r main next-task`.

The old `stack/old-task` bookmark stays where it is — it's historical. The binary automatically picks the new, nearer bookmark as the active stack base. The old stack's plan files are replaced by the new stack's.

## The AI Collaboration Loop

An AI agent generating code needs *intent*, not just a task. "Implement JWT auth" is a task. A plan change contains the background (why JWT, not session tokens), the constraints (must support RS256 and EdDSA), the rejected alternatives (API keys were considered but don't support rotation), and the step-by-step approach (extract middleware first, then strategy pattern, then wire up).

The collaboration loop:

```
Human writes draft plan
  → AI cross-checks: "Step 3 has a race condition if..."
    → Human revises plan
      → PM confirms scope: "Phase 2 can wait for Q4"
        → AI implements, guided by the finalized plan
          → jj plan done strips scratch, archives the clean plan
            → Code links back via jj:CHANGE_ID
```

`[scratch]` sections in plans serve as the shared working memory surface between human and AI. The AI can write analysis, alternatives explored, and debugging notes in scratch sections during implementation. `jj plan done` is the cleanup boundary: reasoning is archived in `jj evolog`, conclusions persist in the final description.

When the agent reads `.jj-plan/current.md` before writing code, it has the full decision record. When it's done, the plan — cleaned of scratch, annotated with completion status — becomes the permanent historical record. The code links back to it. The loop is closed:

```
Plan (jj description) → Code (references jj:CHANGE_ID) → Archaeology (jj show → full plan)
```

No context is lost. No documentation rots. The VCS *is* the documentation.

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

## Getting Started

1. Use [jj](https://github.com/jj-vcs/jj) (Jujutsu) as your VCS.
2. Build and install jj-plan:
   ```sh
   cargo build --release
   cp target/release/jj-plan ~/.local/bin/jj
   ```
   Ensure `~/.local/bin` is in your `$PATH` ahead of the real jj binary.
3. Add `.jj-plan` to your global gitignore.
4. In a repo: `mkdir .jj-plan` to activate.
5. Start a stack: `jj plan stack` (bare) or `jj plan stack my-feature` (named). Use `-r REV` to root it off a specific revision. The bookmarked change is the first member. Or rely on `trunk()` with a remote — no bookmark needed.
6. Start planning: edit `.jj-plan/current.md`. New plans (`jj plan new`) are seeded with a structured template. Use `--first` or `--last` to insert at stack boundaries. Mark `[scratch]` headings for working memory.
7. Navigate: `jj plan next`/`prev` to move through the stack, `jj plan go N` to jump by index.
8. Complete: `jj plan done` marks the current plan as done, strips `[scratch]` sections, and advances to the next undone plan.
9. Introspect: `jj plan config` shows resolved configuration and stack info. `jj plan --help` for all options.

## Development

### Running tests

The recommended way to build and test:

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

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DIR` | Override plan directory path (absolute or relative) | Auto-resolved: `.jj-plan/` → `.jj-plans/` |
| `JJ_PLAN_MAX` | Maximum stack size before refusing to sync | `50` |
| `JJ_PLAN_TEMPLATE` | Override plan template file path | `.jj-plan/template.md` → built-in default |

## Migration from zsh shim

If you were using the zsh shim (`jj-plan.zsh`):

1. **Prerequisites**: Rust toolchain (1.89+).
2. **Build**: `cargo build --release` in the jj-plan repo.
3. **Install**: Remove the old symlink and copy the binary:
   ```sh
   rm ~/.local/bin/jj                           # remove symlink to jj-plan.zsh
   cp target/release/jj-plan ~/.local/bin/jj    # install Rust binary
   ```
4. **Verify**: `jj plan config` — `shim path:` should point to the new binary (not `.zsh`).
5. **Behavioral changes**:
   - `jj describe -m "..."` is now intercepted and routed through plan files automatically. The "NEVER call `jj describe` directly" rule is no longer needed.
   - `jj plan new` and `jj plan stack` now seed descriptions with a structured template instead of `(placeholder: jj:CHANGE_ID)`. The template starts with `(plan: jj:CHANGE_ID)`.
   - New commands available: `done`, `next`, `prev`, `go`.
   - `[scratch]` convention: mark any heading with `[scratch]` for working memory that gets cleaned up on `jj plan done`.
6. The zsh shim (`jj-plan.zsh`) remains in the repo as a reference but is no longer maintained.

## Querying

```sh
# Find all implementation work descended from a plan
jj log -r 'descendants(CHANGE_ID) & ~empty()'

# Find plan references in code
grep -roh 'jj:[a-z]\+' src/ | sort -u

# Resolve a plan reference to its full context
jj show kpqxywon

# List all active stack bookmarks
jj bookmark list stack/*

# Show resolved configuration and stack info
jj plan config

# Navigate the stack by position
jj plan next                             # advance to next plan
jj plan prev                             # go to previous plan
jj plan go 3                             # jump to plan #3
```
