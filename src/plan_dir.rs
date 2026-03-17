use std::path::{Path, PathBuf};

/// How the plan directory was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDirSource {
    /// Resolved from the `JJ_PLAN_DIR` environment variable.
    EnvVar,
    /// Resolved from `.jj-plan/` in the repo root.
    JjPlan,
    /// Resolved from `.jj-plans/` in the repo root (legacy fallback).
    JjPlansLegacy,
}

impl std::fmt::Display for PlanDirSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvVar => write!(f, "env var"),
            Self::JjPlan => write!(f, ".jj-plan"),
            Self::JjPlansLegacy => write!(f, ".jj-plans (legacy)"),
        }
    }
}

/// Resolved plan directory and its resolution source.
#[derive(Debug, Clone)]
pub struct PlanDir {
    pub path: PathBuf,
    pub source: PlanDirSource,
}

impl PlanDir {
    /// The directory name (last component), used in display strings like
    /// `Plan stack (.jj-plan/; ...)`.
    pub fn dir_name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(".jj-plan")
    }
}

/// Resolve the plan directory using the standard fallback chain:
///
/// 1. `JJ_PLAN_DIR` env var — if set, use as-is (no fallback)
/// 2. `.jj-plan/` in repo root — preferred default
/// 3. `.jj-plans/` in repo root — legacy fallback
/// 4. None — not activated
///
/// `repo_root` may be `None` if we're not in a jj repo. In that case,
/// only the env var path is checked.
pub fn resolve_plan_dir(repo_root: Option<&Path>) -> Option<PlanDir> {
    // 1. JJ_PLAN_DIR env var
    if let Ok(env_dir) = std::env::var("JJ_PLAN_DIR") {
        if !env_dir.is_empty() {
            let path = PathBuf::from(&env_dir);
            // The env var is used as-is — no existence check, no fallback.
            // This matches the zsh shim behavior where the env var is
            // trusted unconditionally.
            return Some(PlanDir {
                path,
                source: PlanDirSource::EnvVar,
            });
        }
    }

    let repo_root = repo_root?;

    // 2. .jj-plan/ in repo root
    let jj_plan = repo_root.join(".jj-plan");
    if jj_plan.is_dir() {
        return Some(PlanDir {
            path: jj_plan,
            source: PlanDirSource::JjPlan,
        });
    }

    // 3. .jj-plans/ in repo root (legacy fallback)
    let jj_plans = repo_root.join(".jj-plans");
    if jj_plans.is_dir() {
        return Some(PlanDir {
            path: jj_plans,
            source: PlanDirSource::JjPlansLegacy,
        });
    }

    // 4. Not activated
    None
}

/// Read JJ_PLAN_MAX from the environment, defaulting to 50.
pub fn plan_max() -> usize {
    std::env::var("JJ_PLAN_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolve_jj_plan_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj-plan")).unwrap();

        let result = resolve_plan_dir(Some(root));
        assert!(result.is_some());
        let plan_dir = result.unwrap();
        assert_eq!(plan_dir.source, PlanDirSource::JjPlan);
        assert_eq!(plan_dir.path, root.join(".jj-plan"));
    }

    #[test]
    fn resolve_legacy_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj-plans")).unwrap();

        let result = resolve_plan_dir(Some(root));
        assert!(result.is_some());
        let plan_dir = result.unwrap();
        assert_eq!(plan_dir.source, PlanDirSource::JjPlansLegacy);
        assert_eq!(plan_dir.path, root.join(".jj-plans"));
    }

    #[test]
    fn jj_plan_takes_precedence_over_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".jj-plan")).unwrap();
        fs::create_dir(root.join(".jj-plans")).unwrap();

        let result = resolve_plan_dir(Some(root));
        assert!(result.is_some());
        let plan_dir = result.unwrap();
        assert_eq!(plan_dir.source, PlanDirSource::JjPlan);
    }

    #[test]
    fn no_plan_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_plan_dir(Some(tmp.path()));
        assert!(result.is_none());
    }

    #[test]
    fn no_repo_root_returns_none() {
        // With no env var set, no repo root means no plan dir
        // (env var tested separately since it requires env manipulation)
        let result = resolve_plan_dir(None);
        assert!(result.is_none());
    }

    #[test]
    fn plan_max_default() {
        // When JJ_PLAN_MAX is not set, default is 50
        // (Can't reliably test env override without env manipulation)
        // Just verify the function doesn't panic
        let _max = plan_max();
    }
}