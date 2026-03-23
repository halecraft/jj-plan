//! Platform detection from remote URLs.

use crate::error::{JjPlanError, Result};
use crate::types::{Platform, PlatformConfig};
use std::env;

pub fn detect_platform(url: &str) -> Option<Platform> {
    let gh_host = env::var("GH_HOST").ok();
    let gitlab_host = env::var("GITLAB_HOST").ok();
    let gitea_host = env::var("GITEA_HOST").ok();
    let hostname = extract_hostname(url)?;

    if hostname == "github.com"
        || hostname.ends_with(".github.com")
        || gh_host.as_ref().is_some_and(|h| hostname == *h)
    {
        return Some(Platform::GitHub);
    }

    if hostname == "gitlab.com"
        || hostname.ends_with(".gitlab.com")
        || gitlab_host.as_ref().is_some_and(|h| hostname == *h)
    {
        return Some(Platform::GitLab);
    }

    // Gitea check comes after GitHub/GitLab so explicit GH_HOST/GITLAB_HOST always win.
    if hostname == "codeberg.org"
        || gitea_host.as_ref().is_some_and(|h| hostname == *h)
    {
        return Some(Platform::Gitea);
    }

    None
}

pub fn parse_repo_info(url: &str) -> Result<PlatformConfig> {
    let url = url.trim_end_matches('/');
    let platform = detect_platform(url).ok_or(JjPlanError::NoSupportedRemotes)?;
    let hostname = extract_hostname(url);

    let (owner, repo) = extract_owner_repo(url)
        .ok_or_else(|| JjPlanError::Parse(format!("cannot parse remote URL: {url}")))?;

    let host = match platform {
        Platform::GitHub => {
            if hostname.as_ref().is_some_and(|h| h != "github.com") {
                hostname
            } else {
                None
            }
        }
        Platform::GitLab => {
            if hostname.as_ref().is_some_and(|h| h != "gitlab.com") {
                hostname
            } else {
                None
            }
        }
        Platform::Gitea => {
            // Elide codeberg.org as the well-known default, similar to github.com/gitlab.com.
            // All other Gitea hosts are always included.
            if hostname.as_ref().is_some_and(|h| h != "codeberg.org") {
                hostname
            } else {
                None
            }
        }
    };

    Ok(PlatformConfig {
        platform,
        owner,
        repo,
        host,
    })
}

/// Parse repo info assuming the remote is a Gitea instance.
///
/// Used by `StackContext::new` as a fallback when `detect_platform` returns
/// `None` and an async probe of `/api/v1/version` confirms the host is Gitea.
/// Skips platform detection entirely — the caller is responsible for having
/// already verified that the host is Gitea.
pub fn parse_repo_info_as_gitea(url: &str) -> Result<PlatformConfig> {
    let url = url.trim_end_matches('/');
    let hostname = extract_hostname(url);

    let (owner, repo) = extract_owner_repo(url)
        .ok_or_else(|| JjPlanError::Parse(format!("cannot parse remote URL: {url}")))?;

    Ok(PlatformConfig {
        platform: Platform::Gitea,
        owner,
        repo,
        host: hostname, // Always include host for probe-detected Gitea instances
    })
}

/// Extract hostname from a git remote URL.
///
/// Handles both SCP-style (`git@host:path`) and scheme-based URLs
/// (`ssh://`, `https://`, `git://`, etc.) by branching on the `git@` prefix.
pub fn extract_hostname(url: &str) -> Option<String> {
    if url.starts_with("git@") && !url.contains("://") {
        // SCP-style: git@host:path — the colon separates host from path.
        return url
            .strip_prefix("git@")
            .and_then(|s| s.split(':').next())
            .map(ToString::to_string);
    }
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(ToString::to_string))
}

/// Extract `(owner, repo)` from any git remote URL.
///
/// Uses a URL-parser-first approach with SCP-style fallback:
///
/// 1. Try `url::Url::parse`. If it succeeds (scheme-based URL like `ssh://`,
///    `https://`, `git://`): take `.path()`, strip leading `/` and trailing
///    `.git`, split on `/`, last segment = repo, everything before = owner.
/// 2. If parsing fails (SCP-style `git@host:path`): split on the first `:`
///    after `git@`, take the part after `:`, strip trailing `.git`, split on
///    `/`, same logic.
///
/// Returns `None` if the URL cannot be parsed or the path has fewer than 2
/// segments (owner + repo).
fn extract_owner_repo(url: &str) -> Option<(String, String)> {
    let path = if let Ok(parsed) = url::Url::parse(url) {
        // Scheme-based URL: ssh://, https://, git://, etc.
        let p = parsed.path();
        p.strip_prefix('/').unwrap_or(p).to_string()
    } else if url.starts_with("git@") {
        // SCP-style: git@host:owner/repo.git
        // The colon after the hostname separates host from path.
        let after_at = url.strip_prefix("git@")?;
        let colon_pos = after_at.find(':')?;
        after_at[colon_pos + 1..].to_string()
    } else {
        return None;
    };

    // Strip trailing `.git` suffix and any trailing slashes.
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return None;
    }

    let repo = (*parts.last()?).to_string();
    let owner = parts[..parts.len() - 1].join("/");

    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some((owner, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Existing tests ──────────────────────────────────────────────────

    #[test]
    fn test_detect_github_https() {
        assert_eq!(
            detect_platform("https://github.com/owner/repo.git"),
            Some(Platform::GitHub)
        );
    }

    #[test]
    fn test_detect_github_ssh() {
        assert_eq!(
            detect_platform("git@github.com:owner/repo.git"),
            Some(Platform::GitHub)
        );
    }

    #[test]
    fn test_detect_gitlab_https() {
        assert_eq!(
            detect_platform("https://gitlab.com/owner/repo.git"),
            Some(Platform::GitLab)
        );
    }

    #[test]
    fn test_parse_github_repo() {
        let config = parse_repo_info("https://github.com/owner/repo.git").unwrap();
        assert_eq!(config.platform, Platform::GitHub);
        assert_eq!(config.owner, "owner");
        assert_eq!(config.repo, "repo");
        assert!(config.host.is_none());
    }

    #[test]
    fn test_parse_gitlab_nested_groups() {
        let config = parse_repo_info("https://gitlab.com/group/subgroup/repo.git").unwrap();
        assert_eq!(config.platform, Platform::GitLab);
        assert_eq!(config.owner, "group/subgroup");
        assert_eq!(config.repo, "repo");
    }

    #[test]
    fn test_detect_codeberg_https() {
        assert_eq!(
            detect_platform("https://codeberg.org/owner/repo.git"),
            Some(Platform::Gitea)
        );
    }

    #[test]
    fn test_detect_codeberg_ssh() {
        assert_eq!(
            detect_platform("git@codeberg.org:owner/repo.git"),
            Some(Platform::Gitea)
        );
    }

    /// This test sets `GITEA_HOST` via `std::env::set_var`, which is unsafe in
    /// edition 2024. Because `GITEA_HOST` is not read by any other test, parallel
    /// execution is safe here. If additional tests ever read this env var, they
    /// must be serialised (e.g. via `serial_test`).
    #[test]
    fn test_detect_gitea_via_env_var() {
        // SAFETY: No other test reads GITEA_HOST concurrently.
        unsafe {
            env::set_var("GITEA_HOST", "gitea.example.com");
        }
        let result = detect_platform("https://gitea.example.com/owner/repo.git");
        // SAFETY: Cleaning up.
        unsafe {
            env::remove_var("GITEA_HOST");
        }
        assert_eq!(result, Some(Platform::Gitea));
    }

    #[test]
    fn test_parse_gitea_codeberg_repo() {
        let config = parse_repo_info("https://codeberg.org/owner/repo.git").unwrap();
        assert_eq!(config.platform, Platform::Gitea);
        assert_eq!(config.owner, "owner");
        assert_eq!(config.repo, "repo");
        // codeberg.org is the well-known host and should be elided.
        assert!(config.host.is_none());
    }

    /// Verify that a custom Gitea host (via `GITEA_HOST`) is preserved in the
    /// `host` field of `PlatformConfig`.
    #[test]
    fn test_parse_gitea_custom_host() {
        // SAFETY: No other test reads GITEA_HOST concurrently.
        unsafe {
            env::set_var("GITEA_HOST", "git.mycompany.com");
        }
        let result = parse_repo_info("https://git.mycompany.com/team/project.git");
        // SAFETY: Cleaning up.
        unsafe {
            env::remove_var("GITEA_HOST");
        }
        let config = result.unwrap();
        assert_eq!(config.platform, Platform::Gitea);
        assert_eq!(config.owner, "team");
        assert_eq!(config.repo, "project");
        assert_eq!(config.host.as_deref(), Some("git.mycompany.com"));
    }

    // ── Bug 1 fix: ssh:// with port (wrong owner/repo) ─────────────────

    #[test]
    fn test_parse_ssh_scheme_with_port() {
        // SAFETY: No other test reads GITEA_HOST concurrently.
        unsafe {
            env::set_var("GITEA_HOST", "code.halecraft.org");
        }
        let result = parse_repo_info("ssh://git@code.halecraft.org:29418/duane/jj-plan.git");
        unsafe {
            env::remove_var("GITEA_HOST");
        }
        let config = result.unwrap();
        assert_eq!(config.platform, Platform::Gitea);
        assert_eq!(config.owner, "duane");
        assert_eq!(config.repo, "jj-plan");
        assert_eq!(config.host.as_deref(), Some("code.halecraft.org"));
    }

    #[test]
    fn test_parse_ssh_scheme_nested_groups() {
        // SAFETY: No other test reads GITLAB_HOST concurrently.
        unsafe {
            env::set_var("GITLAB_HOST", "gitlab.example.com");
        }
        let result =
            parse_repo_info("ssh://git@gitlab.example.com:2222/group/subgroup/repo.git");
        unsafe {
            env::remove_var("GITLAB_HOST");
        }
        let config = result.unwrap();
        assert_eq!(config.platform, Platform::GitLab);
        assert_eq!(config.owner, "group/subgroup");
        assert_eq!(config.repo, "repo");
    }

    // ── Bug 2 fix: ssh:// without port / git:// (total parse failure) ──

    #[test]
    fn test_parse_ssh_scheme_without_port() {
        // SAFETY: No other test reads GITEA_HOST concurrently.
        unsafe {
            env::set_var("GITEA_HOST", "code.halecraft.org");
        }
        let result = parse_repo_info("ssh://git@code.halecraft.org/duane/jj-plan.git");
        unsafe {
            env::remove_var("GITEA_HOST");
        }
        let config = result.unwrap();
        assert_eq!(config.platform, Platform::Gitea);
        assert_eq!(config.owner, "duane");
        assert_eq!(config.repo, "jj-plan");
    }

    #[test]
    fn test_parse_git_scheme() {
        // git:// URLs won't match any platform without env var config,
        // so test via extract_owner_repo directly.
        let result = extract_owner_repo("git://code.halecraft.org/duane/jj-plan.git");
        assert_eq!(
            result,
            Some(("duane".to_string(), "jj-plan".to_string()))
        );
    }

    // ── Regression guards ───────────────────────────────────────────────

    #[test]
    fn test_parse_scp_style_still_works() {
        let config = parse_repo_info("git@github.com:owner/repo.git").unwrap();
        assert_eq!(config.platform, Platform::GitHub);
        assert_eq!(config.owner, "owner");
        assert_eq!(config.repo, "repo");
    }

    #[test]
    fn test_parse_scp_style_nested_groups() {
        let config = parse_repo_info("git@gitlab.com:group/subgroup/repo.git").unwrap();
        assert_eq!(config.platform, Platform::GitLab);
        assert_eq!(config.owner, "group/subgroup");
        assert_eq!(config.repo, "repo");
    }

    // ── Platform detection end-to-end with ssh:// ───────────────────────

    #[test]
    fn test_detect_platform_ssh_scheme_with_port() {
        // SAFETY: No other test reads GITEA_HOST concurrently.
        unsafe {
            env::set_var("GITEA_HOST", "code.halecraft.org");
        }
        let result =
            detect_platform("ssh://git@code.halecraft.org:29418/duane/jj-plan.git");
        unsafe {
            env::remove_var("GITEA_HOST");
        }
        assert_eq!(result, Some(Platform::Gitea));
    }

    // ── extract_owner_repo edge cases ───────────────────────────────────

    #[test]
    fn test_extract_owner_repo_edge_cases() {
        // Trailing slash on URL
        assert_eq!(
            extract_owner_repo("https://github.com/owner/repo.git/"),
            Some(("owner".to_string(), "repo".to_string()))
        );

        // No .git suffix
        assert_eq!(
            extract_owner_repo("https://github.com/owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );

        // Bare git:// scheme
        assert_eq!(
            extract_owner_repo("git://host/owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );

        // ssh:// without userinfo
        assert_eq!(
            extract_owner_repo("ssh://host/owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );

        // Too few path segments (no owner)
        assert_eq!(extract_owner_repo("https://github.com/repo.git"), None);

        // Empty path
        assert_eq!(extract_owner_repo("https://github.com/"), None);

        // SCP-style no .git suffix
        assert_eq!(
            extract_owner_repo("git@github.com:owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );

        // SCP-style nested groups
        assert_eq!(
            extract_owner_repo("git@gitlab.com:group/subgroup/repo.git"),
            Some(("group/subgroup".to_string(), "repo".to_string()))
        );

        // ssh:// with port, nested groups
        assert_eq!(
            extract_owner_repo("ssh://git@gitlab.example.com:2222/group/subgroup/repo.git"),
            Some(("group/subgroup".to_string(), "repo".to_string()))
        );
    }
}