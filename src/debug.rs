//! Debug logging for jj-plan, gated on `JJ_PLAN_DEBUG` env var.
//!
//! When `JJ_PLAN_DEBUG=1` (or any value) is set, diagnostic messages are
//! printed to stderr with a `jj-plan: [debug]` prefix. When unset, the
//! env var check is cached via `OnceLock` so there is exactly one syscall
//! per process, not per log line.

use std::sync::OnceLock;

/// Returns `true` if `JJ_PLAN_DEBUG` is set in the environment.
pub fn debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("JJ_PLAN_DEBUG").is_ok())
}

/// Log a debug message to stderr, prefixed with `jj-plan: [debug]`.
///
/// Gated on `JJ_PLAN_DEBUG` env var — zero cost (one cached bool check)
/// when unset.
///
/// Supports `format!`-style arguments:
///
/// ```ignore
/// debug_log!("evaluate_revset({:?})", expr);
/// debug_log!("  parse: OK");
/// ```
#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if $crate::debug::debug_enabled() {
            eprintln!("jj-plan: [debug] {}", format!($($arg)*));
        }
    };
}