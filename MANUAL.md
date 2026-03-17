# jj-plan Manual

> Exhaustive command reference, best practices, and recipes.

For the project philosophy and quick start, see [README.md](README.md).
For architecture and internals, see [TECHNICAL.md](TECHNICAL.md).

---

## Overview

jj-plan is a Rust binary that shadows the real `jj` binary on your `$PATH`. It intercepts mutating commands to keep a `.jj-plan/` directory in sync with the current stack's change descriptions. Read-only commands (`log`, `diff`, `show`, etc.) pass through with zero overhead.

Plans are stored as jj change descriptions — rich markdown documents that serve as the specification, context, and historical record for each unit of work. The `.jj-plan/` directory makes them co-editable as ordinary files.

---

## Concepts

### Plans

A **plan** is a jj change whose description is the primary artifact. The description contains background, constraints, alternatives considered, and a step-by-step approach. The diff on that change is the implementation.

Plans are reviewed, debated, and validated *before* code is written. Once implementation is complete, `jj plan done` cleans working memory from the description and archives the plan in version history.

### Stacks

A **stack** is a sequence of related plan changes. A stack is bounded by a `stack` or `stack/*` bookmark placed **on the first change** (inclusive — the bookmarked change is a stack member). Changes from the bookmark through `@` and its descendants form the stack.

If no `stack` bookmark exists, the binary falls back to `trunk()` as the base (exclusive — the trunk commit is not part of your stack).

### The `.jj-plan/` Directory

The `.jj-plan/` directory is the sync surface between jj change descriptions and your filesystem. It must exist in the repo root to activate jj-plan features. Add it to your global gitignore.

| File | Description |
|---|---|
| `NN-CHANGEID.md` | Plan file for stack member at position NN (01 = closest to base). Content is synced bidirectionally with the jj change description. |
| `current.md` | Symlink → the active change's plan file (whichever `NN-CHANGEID.md` corresponds to `@`). |
| `.stack` | One-line-per-change summary of the full stack. Regenerated on every sync. |
| `error.md` | Transient file created during error states (stack too large, ambiguous bookmarks). `current.md` points here during errors. Auto-cleared when the condition resolves. |
| `template.md` | Optional custom plan template. Overrides the built-in default. See [Plan Templates](#plan-templates). |

#### Status markers in `.stack`

The `.stack` file uses single-character markers:

| Marker | Meaning |
|---|---|
| `*` | Working copy — you are here (`@`) |
| `✓` | Done — description contains `plan-status: ✅` |
| `~` | Has file changes (non-empty diff) |
| ` ` | Empty change, not done, not working copy |

Example:

```
  ✓ 01-kpqxywon :: Refactor auth middleware
  ~ 02-mtzrlpvq :: Extract auth module
*   03-ykvsnxrl :: Implement JWT strategy
    04-abcdefgh :: Add API key support
```

### Plan Status

A plan is considered **done** when its description contains `plan-status: ✅` on its own line. This marker is appended by `jj plan done` and is idempotent — running `done` on an already-done plan does not duplicate the marker.

### Working Memory (`[scratch]`)

Any markdown heading suffixed with `[scratch]` (case-insensitive) designates a **working memory** section:

```markdown
## Analysis [scratch]

Tried approach A — failed because of X.
Approach B works but needs Y from the API.

### Sub-analysis [scratch]

This is also working memory.
```

Working memory conventions:

- Use scratch sections for analysis, debugging notes, alternatives explored, open questions, and temporary context.
- Scratch sections are **stripped** by `jj plan done`, cleaning the archival record.
- The full pre-strip content is always recoverable via `jj evolog` (jj's evolution log).
- Scratch stripping respects heading levels: a `## Foo [scratch]` section includes everything until the next `##` or higher-level heading.
- Code fences inside scratch sections are handled correctly — headings inside fenced code blocks are not treated as section boundaries.
- Use `--keep-scratch` on `jj plan done` to preserve scratch sections if desired.

### Change ID References (`jj:CHANGE_ID`)

The `jj:CHANGE_ID` convention creates permanent, resolvable links between code and plans:

```rust
// We bypass the cache here for consistency during concurrent writes.
// Context: jj:kpqxywon
```

Anyone can run `jj show kpqxywon` to retrieve the full plan. Change IDs are stable across rebase, amend, and rewrite operations — unlike git commit hashes.

Use `jj:CHANGE_ID` in:

- **Code comments** — link implementation to rationale.
- **Plan descriptions** — reference other plans, forming a navigable knowledge graph.
- **PR descriptions** — point reviewers to the full plan context.

---

## Commands

### `jj plan stack`

Create a new stack with a single plan change.

**Synopsis:**

```
jj plan stack [NAME] [-r REV]
```

**Description:**

Creates a new jj change, sets a `stack` or `stack/NAME` bookmark on it, and seeds the description with a plan template. This is the entry point for starting a new unit of work.

**Options:**

| Option | Description |
|---|---|
| `NAME` | Optional stack name. Creates `stack/NAME` bookmark. Omit for a bare `stack` bookmark. |
| `-r REV` | Revision to create the new change after. Defaults to `@`. Use `-r main` to root a stack off the main branch. |

**Examples:**

```sh
# Start a bare unnamed stack after the current change
jj plan stack

# Start a named stack
jj plan stack auth-refactor

# Start a named stack rooted off main
jj plan stack -r main auth-refactor
```

**Notes:**

- The operation is atomic: if the bookmark set fails, the `jj new` is rolled back via `jj undo`.
- The bookmark is set with `-B` (allow backwards move), so you can reuse stack names.
- The new change's description is seeded with the plan template (see [Plan Templates](#plan-templates)).
- After creation, `.jj-plan/` is synced and the stack summary is displayed.

---

### `jj plan new`

Create a new plan change in the current stack.

**Synopsis:**

```
jj plan new [--first | --last] [jj-new-args...]
```

**Description:**

Creates a new jj change within the current stack, seeds it with a plan template, and syncs the plan directory. By default, the change is inserted after the current working copy (`@`), preserving stack linearity.

**Options:**

| Option | Description |
|---|---|
| `--first` | Insert before the first stack member. Moves the `stack`/`stack/*` bookmark to the new change. |
| `--last` | Insert after the last stack member. |
| `--insert-after`, `-A`, `--insert-before`, `-B`, `-r` | Forwarded to `jj new`. If any explicit positioning flag is present, the default `--insert-after @` is suppressed. |

**Examples:**

```sh
# Insert a new plan after the current change (default)
jj plan new

# Prepend a plan to the start of the stack
jj plan new --first

# Append a plan to the end of the stack
jj plan new --last
```

**Notes:**

- `--first` and `--last` are mutually exclusive.
- `--first` moves the stack bookmark to the newly created change (since it becomes the new first member). `--last` does not move the bookmark.
- The default `--insert-after @` ensures the new change is a child of `@`, keeping the stack linear. This prevents the "sibling instead of child" issue that raw `jj new` can cause.
- The new change's description is seeded with the plan template.
- All unrecognized arguments are forwarded to the underlying `jj new` command.

---

### `jj plan done`

Mark a plan as done, strip working memory, and advance.

**Synopsis:**

```
jj plan done [CHANGE_ID] [--stack] [--keep-scratch] [--dry-run]
```

**Description:**

Marks a plan as complete by appending `plan-status: ✅` to its description and stripping all `[scratch]` sections. By default, operates on the current working copy (`@`) and advances to the next undone plan in the stack.

**Options:**

| Option | Description |
|---|---|
| `CHANGE_ID` | Target a specific change instead of `@`. |
| `--stack` | Mark all plans in the stack as done. |
| `--keep-scratch` | Skip `[scratch]` section stripping. |
| `--dry-run` | Show what would be changed without modifying anything. |

**Examples:**

```sh
# Mark the current plan done and advance to next undone plan
jj plan done

# Preview what would be stripped
jj plan done --dry-run

# Mark done but keep scratch sections
jj plan done --keep-scratch

# Mark a specific plan done
jj plan done kpqxywon

# Mark the entire stack done
jj plan done --stack
```

**Behavior details:**

- **Scratch stripping**: All headings with `[scratch]` (case-insensitive) and their content are removed. Nested headings within the scratch section are also removed. Code fences inside scratch sections are handled correctly. The pre-strip content is preserved in `jj evolog`.
- **Done marker**: `plan-status: ✅` is appended on its own line. If already present, the marker is not duplicated (idempotent).
- **Auto-advance**: When marking the working copy (`@`) as done (the default), the binary automatically `jj edit`s the next undone plan in the stack. Search is forward-then-wraparound. If all plans are done, a message is printed instead.
- **`--stack` mode**: Marks every change in the stack as done. After completion, prints a suggestion to start a new stack. Does not auto-advance (since everything is done).
- **`--dry-run` mode**: Prints the change ID, which scratch sections would be stripped, and whether the done marker would be appended. Makes no modifications.

---

### `jj plan next`

Advance to the next plan in the stack.

**Synopsis:**

```
jj plan next
```

**Description:**

Moves the working copy (`@`) to the next change in the stack via `jj edit`. If `@` is already the last plan, prints "Already at the last plan in the stack" and stays put.

**Examples:**

```sh
jj plan next
```

---

### `jj plan prev`

Move to the previous plan in the stack.

**Synopsis:**

```
jj plan prev
```

**Description:**

Moves the working copy (`@`) to the previous change in the stack via `jj edit`. If `@` is already the first plan, prints "Already at the first plan in the stack" and stays put.

**Examples:**

```sh
jj plan prev
```

---

### `jj plan go`

Jump to a specific plan by index or change ID.

**Synopsis:**

```
jj plan go <N | CHANGE_ID>
```

**Description:**

Moves the working copy (`@`) to a specific plan. The target can be:

- A **1-based index** matching the `NN-CHANGEID.md` file numbering (e.g., `1` for the first plan, `3` for the third).
- A **change ID** (passed through to `jj edit -r`).

**Options:**

| Option | Description |
|---|---|
| `N` | 1-based plan index. Must be in range `1` to stack size. |
| `CHANGE_ID` | A jj change ID (or unique prefix). |

**Examples:**

```sh
# Jump to the third plan in the stack
jj plan go 3

# Jump to a specific change by ID
jj plan go kpqxywon
```

**Notes:**

- Index `0` is an error (1-based, not 0-based).
- Out-of-range indices produce an error with the valid range.

---

### `jj plan config`

Show resolved configuration and stack info.

**Synopsis:**

```
jj plan config
```

**Description:**

Read-only introspection command. Prints all resolved configuration as key-value pairs. No flush, no sync, no side effects.

**Output fields:**

| Field | Description |
|---|---|
| `shim path` | Absolute path to the jj-plan binary. |
| `real jj binary` | Absolute path to the real jj binary being shadowed. |
| `repo root` | Absolute path to the repository root. |
| `JJ_PLAN_DIR env` | Value of the `JJ_PLAN_DIR` environment variable, or `(not set)`. |
| `JJ_PLAN_MAX env` | Maximum stack size (default: 50). |
| `resolved dir` | Absolute path to the resolved plan directory. |
| `resolution source` | How the plan directory was resolved: `env var`, `.jj-plan`, or `.jj-plans (legacy)`. |
| `stack base` | Change ID or `trunk()` with range mode (`inclusive` or `exclusive`), or `(none)`. |
| `stack size` | Number of changes in the current stack. |

**Examples:**

```sh
jj plan config
```

Output:

```
jj-plan configuration:

  shim path:        /Users/you/.local/bin/jj
  real jj binary:   /opt/homebrew/bin/jj
  repo root:        /Users/you/project

  JJ_PLAN_DIR env:  (not set)
  JJ_PLAN_MAX env:  50

  resolved dir:     /Users/you/project/.jj-plan
  resolution source: .jj-plan

  stack base:       kpqxywon (inclusive)
  stack size:       4
```

---

### `jj plan --help`

Print a compact terminal summary of what jj-plan can do.

**Synopsis:**

```
jj plan --help
jj plan -h
jj plan --help --color <WHEN>
jj --color <WHEN> plan --help
```

**Description:**

Use `jj plan --help` when you want quick terminal orientation: the mental model, the happy-path workflow, the available subcommands, and where to go next in the docs.

This help screen is intentionally compact. It is the fast overview, not the exhaustive reference.

**Color behavior:**

`jj plan --help` follows jj-style color behavior:

- `--color always` forces ANSI styling
- `--color never` disables ANSI styling
- `--color auto` follows terminal-aware default behavior
- if no explicit `--color` flag is provided, jj-plan follows the resolved/default jj color mode

**Notes:**

- Use `jj plan --help` for the compact terminal summary.
- Use `MANUAL.md` when you want the full command reference, examples, and recipes.
- Use `README.md` for the project overview, philosophy, and quick start.
- Use `TECHNICAL.md` for architecture and implementation details.

---

## Intercepted Commands

jj-plan intercepts certain jj commands to provide plan-aware behavior. All other mutating commands go through the standard wrap lifecycle (flush pending edits → run command → sync plan files → show stack).

### `jj describe`

**Behavior:** When `jj describe -m "..."` is used with the `-m` / `--message` flag, jj-plan writes the message to the corresponding plan file *before* running the command. This ensures the plan file is always the source of truth.

- **`-m` / `--message` mode**: The message is written to the plan file for the target change (default: `@`). Multiple `-m` values are concatenated with newlines (matching jj behavior). The command then proceeds through the standard wrap lifecycle — flush picks up the file write, `jj describe` sets the same content (idempotent), and sync reads it back.
- **Editor mode** (no `-m`): Passes through to the standard wrap lifecycle without interception. Whatever you write in the editor is picked up by sync afterward.
- **`-r` / `--revision`**: Respected — the message is written to the plan file for the specified revision, not necessarily `@`.

**Notes:**

- This interception eliminates the old "NEVER call `jj describe` directly" rule from the zsh shim era.
- All `-m` / `-r` argument forms are supported: `-m VALUE`, `-mVALUE`, `--message VALUE`, `--message=VALUE`, `-r VALUE`, `-rVALUE`, `--revision VALUE`, `--revision=VALUE`.

### `jj abandon`

**Behavior:** jj-plan protects stack bookmarks from accidental loss during abandon operations.

1. **Before abandon**: Snapshots the bookmark state — which change holds it, whether it's the working copy, and its first child.
2. **Runs `jj abandon`** with all original arguments.
3. **After abandon**: Checks if the bookmark survived. If lost:
   - Tries moving the bookmark to the first child of the abandoned change.
   - If no child but the abandoned change was `@`, moves to the new `@`.
   - If recovery target found, moves the bookmark and prints a message.
   - If recovery fails, prints a WARNING with manual instructions: `jj bookmark set NAME -r <change>`.

**Flags:**

| Flag | Description |
|---|---|
| `--retain-bookmarks` | Bypasses the recovery handler entirely. |

### `jj status` / `jj st`

**Behavior:** `jj status` is not in the read-only passthrough list. It goes through the full wrap lifecycle (flush → command → sync → show), which means:

1. Pending plan file edits are flushed to jj descriptions.
2. `jj status` runs normally.
3. Plan files are synced from the current jj state.
4. The plan stack summary is appended to the output.

This ensures the plan stack display is always up-to-date.

### Read-only passthrough commands

The following commands bypass jj-plan entirely via Unix `exec` (zero overhead):

`log`, `diff`, `show`, `interdiff`, `evolog`, `file`, `config`, `help`, `version`, `root`, `tag`, `op`, `operation`, `util`, `git`, `gerrit`, `sign`, `unsign`, `workspace`

### All other mutating commands

Commands not listed above (`new`, `edit`, `rebase`, `squash`, `split`, `move`, `bookmark`, etc.) go through the standard wrap lifecycle:

1. Flush all pending plan file edits to jj descriptions.
2. Run the command with inherited stdio.
3. Reload the repository state.
4. Re-resolve the stack and sync plan files.
5. Display the plan stack summary.

---

## Plan Templates

New plan changes created by `jj plan stack` and `jj plan new` are seeded with a template.

### Built-in default template

```markdown
(plan: jj:{{CHANGE_ID}})
```

The built-in default is intentionally minimal — just the self-referencing summary line `(plan: jj:CHANGE_ID)`. This signals "this is an unedited plan" while embedding a stable self-reference. The binary does not impose any plan structure; developers who want sections (Background, Tasks, Scratchpad, etc.) should create a `.jj-plan/template.md` or set `JJ_PLAN_TEMPLATE`.

### Template resolution chain

Templates are resolved in order:

1. **`JJ_PLAN_TEMPLATE` environment variable** → read the file at that path.
2. **`.jj-plan/template.md`** → read the file if it exists.
3. **Built-in default** (shown above).

If an env var or file is empty or unreadable, the next source in the chain is tried.

### `{{CHANGE_ID}}` interpolation

All occurrences of `{{CHANGE_ID}}` in the template are replaced with the actual change ID of the new plan. If a custom template contains no `{{CHANGE_ID}}` placeholder, a self-referencing HTML comment `<!-- jj:CHANGE_ID -->` is injected as the second line, ensuring every plan has a self-reference.

### Custom template example

Create `.jj-plan/template.md`:

```markdown
{{CHANGE_ID}}: untitled

## Goal


## Design


## Checklist

- [ ]

## Notes [scratch]

```

---

## Environment Variables

### `JJ_PLAN_DIR`

Override the plan directory path.

| | |
|---|---|
| **Default** | Auto-resolved: `.jj-plan/` in repo root, then `.jj-plans/` (legacy fallback) |
| **Values** | Absolute or relative path to the plan directory |

When set, the env var is used as-is — no existence check, no fallback to `.jj-plan/` or `.jj-plans/`.

```sh
# Use a custom directory
export JJ_PLAN_DIR=/tmp/my-plans
```

### `JJ_PLAN_MAX`

Maximum number of changes in a stack before refusing to sync.

| | |
|---|---|
| **Default** | `50` |
| **Values** | Any positive integer |

When the stack exceeds this limit, an error state is set: `current.md` points to `error.md`, and sync is paused until the stack shrinks.

```sh
# Allow larger stacks
export JJ_PLAN_MAX=100

# Restrict to small stacks
export JJ_PLAN_MAX=5
```

### `JJ_PLAN_TEMPLATE`

Override the plan template file path.

| | |
|---|---|
| **Default** | `.jj-plan/template.md` in the plan directory, then the built-in default |
| **Values** | Path to a markdown file |

```sh
# Use a team-shared template
export JJ_PLAN_TEMPLATE=~/.config/jj-plan/template.md
```

---

## Recipes

### Find all work descended from a plan

```sh
jj log -r 'descendants(CHANGE_ID) & ~empty()'
```

### Find plan references in code

```sh
grep -roh 'jj:[a-z]\+' src/ | sort -u
```

### Resolve a plan reference to its full context

```sh
jj show kpqxywon
```

### List all active stack bookmarks

```sh
jj bookmark list stack/*
```

### Show what scratch content was stripped

After `jj plan done`, the pre-strip content is preserved in the evolution log:

```sh
# Show the evolution history of a change
jj evolog -r CHANGE_ID

# Show the diff between the last two versions of a description
jj evolog -r CHANGE_ID -p
```

### Extract the same section from prior plans

Because plans are structured markdown in version history, you can pull the same section from earlier plans to compare decisions, post-implementation reflections, alternatives considered, and patterns.

```sh
# Pull one named section from an earlier plan
jj show rtssrkto --git 2>&1 | grep -A5 "Post-implementation Reflections"

# Search for the same heading across multiple plans
jj log -r '::@' -T 'change_id.shortest() ++ " " ++ description.first_line() ++ "\n"' \
| grep '^[a-z]'
```

This is especially useful when your plans share consistent headings such as `## Alternatives Considered`, `## Post-implementation Reflections`, or `## Risks`. Humans and AI can reuse the relevant slice of prior reasoning without rereading the entire plan.

### Cross-reference plans

Reference other plans by change ID in your plan description:

```markdown
## Background

This continues the work started in jj:kpqxywon. The approach differs
from jj:mtzrlpvq because we discovered that...
```

Anyone can follow the link: `jj show kpqxywon`.

### Navigate the stack by position

```sh
jj plan next          # advance to next plan
jj plan prev          # go to previous plan
jj plan go 3          # jump to plan #3
jj plan go kpqxywon   # jump to a specific change
```

### Inspect resolved configuration

```sh
jj plan config
```

Shows the shim path, real jj binary, resolved plan directory, stack base, and stack size.

### Start a new stack after finishing

```sh
# The old stack's bookmark stays — no cleanup needed
jj plan stack next-task

# Or root it off main
jj plan stack -r main next-task
```

### Work with multiple concurrent stacks

```sh
# Create named stacks
jj plan stack auth-refactor
# ... work ...
jj plan stack -r main api-keys

# List all stacks
jj bookmark list stack/*

# The binary automatically picks the nearest ancestor bookmark
```

### Use plans without a stack bookmark

If your repo has a remote with `trunk()` configured (e.g., `main@origin`), you don't need a stack bookmark at all. The binary falls back to `trunk()` as an exclusive base — all changes between trunk and `@` form the stack.

---

## See Also

- [README.md](README.md) — Project overview, philosophy, and quick start.
- [TECHNICAL.md](TECHNICAL.md) — Architecture, internals, performance, and testing.