# jj-plan: Plan-Oriented Programming

> Plans as VCS Artifacts

**TL;DR**: In plan-oriented programming, the plan is the primary artifact — reviewed, debated, and validated by humans, AI agents, designers, and PMs *before any code is written*. By storing plans as Jujutsu change descriptions (jj is a VCS, like git), they become permanent, addressable nodes in version history. A thin shim makes them co-editable as markdown files in your editor.

---

## The Paradigm: Plan-Oriented Programming

Most software workflows treat code as the artifact and plans as ephemeral scaffolding. A plan lives in a Google Doc or a Notion page, gets discussed in Slack, maybe referenced in a PR description, and is forgotten the moment the code lands.

Plan-oriented programming inverts this. **The plan is the artifact.** Code is its expression.

The workflow:

1. **Draft** — A human or AI agent writes an implementation plan: background, constraints, rationale, rejected alternatives, concrete steps.
2. **Cross-check** — A second prompt, or AI agent, or human reviews the plan for correctness, feasibility, and completeness. Designers and PMs weigh in on intent and scope.
3. **Validate** — The plan is iterated on. Steps are refined. Edge cases are surfaced. This happens *before a single line of code exists*.
4. **Implement** — Only now does code get written, guided by the plan. The plan is the specification, the context, and the historical record.
5. **Archive** — The plan remains in version history, permanently linked to the code it produced.

This is not waterfall. Plans are living documents that evolve during implementation. But the key shift is that **planning is a first-class phase with its own artifact**, not a mental prelude to typing code.

## Why Plans Need a Home in the VCS

Plans stored outside the VCS (wikis, docs, chat) suffer from three problems:

- **Drift**: The plan says one thing, the code does another, and no one updates either.
- **Disconnection**: Six months later, `git blame` shows "refactor auth middleware" and the reasoning is gone.
- **Inaccessibility**: The AI agent writing code can't read your Google Doc. The new team member can't find the Slack thread.

When the plan lives *in* the VCS — as a change description — it travels with the code. It's versioned. It's queryable--tools can index every line of code against the plan it came from (i.e. `git blame`). It's right where you need it.

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
jj plan new                              # Add a plan change to the stack (with placeholder)
jj plan new --first                      # Insert a plan change at the start of the stack
jj plan new --last                       # Insert a plan change at the end of the stack
```

Or without a name: `jj plan stack` sets a bare `stack` bookmark. You can also root a stack off a specific revision: `jj plan stack -r main my-feature`.

The plan lives in the change description — a markdown document with background, constraints, alternatives considered, and step-by-step approach. The `stack` bookmark marks the first change in the stack, and it is **included** as a stack member.

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

A shim intercepts `jj` commands and maintains a `.jj-plan/` directory:

```
.jj-plan/
  current.md          → symlink to active change's plan
  .stack              → one-line summary of the full stack
  01-kpqxywon.md      — stack bookmark (first member)
  02-mtzrlpvq.md
  03-ykvsnxrl.md      — tip
```

You and an AI agent both edit these markdown files in the editor — no `jj describe` clobbering, no modal editor sessions. The shim flushes edits to jj descriptions automatically. `jj status` shows the stack at a glance:

```
Plan stack (.jj-plan/; *=here ✓=done ~=has changes):
  ✓ 01-kpqxywon :: Refactor auth middleware
  ~ 02-mtzrlpvq :: Extract auth module
*   03-ykvsnxrl :: Implement JWT strategy
    04-abcdefgh :: Add API key support
```

## Stack Bookmarks

The shim uses `stack` / `stack/*` bookmarks to determine which changes belong to the current plan stack.

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

When multiple `stack` / `stack/*` bookmarks are ancestors of `@`, the **nearest** one wins automatically. This means you can have multiple stacks in your history and the shim always picks the right one:

```
○ stack/phase1 (old, farther from @)
○ phase 1 work
○ stack/phase2 (nearer to @, wins)
○ phase 2 work
@ current
```

If two `stack/*` bookmarks are equidistant siblings (e.g., a merge of two branches each with their own bookmark), the shim produces an error and asks you to advance or remove one.

### Fallback to `trunk()`

If no `stack` / `stack/*` bookmark is an ancestor of `@`, the shim falls back to `trunk()` (if it resolves to something other than `root()`). The trunk fallback uses an **exclusive** range — the trunk commit is not part of your stack, since you don't own it.

If neither a stack bookmark nor `trunk()` can be resolved, no sync occurs.

### "Done" workflow

When you finish a stack, you don't need to move or delete any bookmarks. Just start a new stack:

```sh
jj plan stack next-task                # atomic: creates change + sets stack/next-task bookmark
```

Or without a name: `jj plan stack`. Or rooted off a specific revision: `jj plan stack -r main next-task`.

The old `stack/old-task` bookmark stays where it is — it's historical. The shim automatically picks the new, nearer bookmark as the active stack base. The old stack's plan files are replaced by the new stack's.

## The AI Collaboration Loop

An AI agent generating code needs *intent*, not just a task. "Implement JWT auth" is a task. A plan change contains the background (why JWT, not session tokens), the constraints (must support RS256 and EdDSA), the rejected alternatives (API keys were considered but don't support rotation), and the step-by-step approach (extract middleware first, then strategy pattern, then wire up).

The collaboration loop:

```
Human writes draft plan
  → AI cross-checks: "Step 3 has a race condition if..."
    → Human revises plan
      → PM confirms scope: "Phase 2 can wait for Q4"
        → AI implements, guided by the finalized plan
          → Plan is marked done, lives in history forever
            → Code links back via jj:CHANGE_ID
```

When the agent reads `.jj-plan/current.md` before writing code, it has the full decision record. When it's done, the plan — annotated with completion status — becomes the permanent historical record. The code links back to it. The loop is closed:

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
| Plans are co-editable (human + AI) | `.jj-plan/` shim syncs markdown files ↔ descriptions |
| Plans co-exist with code review | Plan changes have rich descriptions — they *are* the review |
| Plans split across PRs | New plan changes reference originals by change ID |
| Plans have status tracking | `plan-status: ✅` in description; inferred from empty/non-empty |
| Stack is always visible | `jj status` appends the plan stack summary |
| Multiple concurrent stacks | `stack/*` bookmarks with nearest-ancestor resolution |
| Stack bookmark is intuitive | Bookmark is ON the first change, not before it (inclusive) |

## Getting Started

1. Use [jj](https://github.com/jj-vcs/jj) (Jujutsu) as your VCS.
2. Install [jj-plan](jj-plan.zsh) in your `$PATH`.
3. Add `.jj-plan` to your global gitignore.
4. In a repo: `mkdir .jj-plan` to activate.
5. Start a stack: `jj plan stack` (bare) or `jj plan stack my-feature` (named). Use `-r REV` to root it off a specific revision (e.g. `jj plan stack -r main my-feature`). The bookmarked change is the first member. Or rely on `trunk()` with a remote — no bookmark needed.
6. Start planning: edit `.jj-plan/current.md`, review with your team and AI, then `jj plan new` to add plan changes to the stack (each gets a self-referencing `jj:CHANGE_ID` placeholder). Use `--first` or `--last` to insert at stack boundaries. Run `jj plan --help` for all options.

## Environment Variables

| Variable | Purpose | Default |
|---|---|---|
| `JJ_PLAN_DIR` | Override plan directory path (absolute or relative) | Auto-resolved: `.jj-plan/` → `.jj-plans/` |
| `JJ_PLAN_MAX` | Maximum stack size before refusing to sync | `50` |

## Migration from jj-pop

- Rename `jj-pop.zsh` to `jj-plan.zsh` in your `$PATH` (or update your symlink).
- Replace `jj stack new` with `jj plan stack` in your workflow.
- Existing `.jj-plans/` directories continue to work (silent fallback). Create `.jj-plan/` when ready to migrate — it takes precedence.
- Replace `JJ_PLANS_MAX` with `JJ_PLAN_MAX` in your shell profile (old env var is no longer recognized).

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
```
