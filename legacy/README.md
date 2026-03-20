# Legacy

This directory contains historical artifacts that are no longer actively maintained.

## `jj-plan.zsh`

The original zsh shim that preceded the Rust binary. It was the first implementation of jj-plan, providing plan file synchronization via shell functions.

The Rust binary (`src/main.rs`) has fully replaced it with:

- In-process jj-lib reads (no subprocess overhead for repo queries)
- `jj describe -m` interception (eliminates the "never call jj describe directly" rule)
- PlanRegistry-based bookmark tracking (replaces `stack`/`stack/*` naming convention)
- Stacked PR support via `jj stack submit/sync/merge`
- Platform layer for GitHub and GitLab

The zsh shim is preserved here for historical reference. The full evolution is also available via `jj log` and `jj show`.