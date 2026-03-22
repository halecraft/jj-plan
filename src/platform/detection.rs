//! Platform detection from remote URLs.

use crate::error::{JjPlanError, Result};
use crate::types::{Platform, PlatformConfig};
use regex::Regex;
use std::env;
use std::sync::LazyLock;

static RE_SSH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git@[^:]+:(.+?)(?:\.git)?$").unwrap());

static RE_HTTPS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^/]+/(.+?)(?:\.git)?$").unwrap());

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

    let path = RE_SSH
        .captures(url)
        .or_else(|| RE_HTTPS.captures(url))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .ok_or_else(|| JjPlanError::Parse(format!("cannot parse remote URL: {url}")))?;

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return Err(JjPlanError::Parse(format!("invalid repo path: {path}")));
    }

    let repo = (*parts.last().unwrap()).to_string();
    let owner = parts[..parts.len() - 1].join("/");

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

    let path = RE_SSH
        .captures(url)
        .or_else(|| RE_HTTPS.captures(url))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .ok_or_else(|| JjPlanError::Parse(format!("cannot parse remote URL: {url}")))?;

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 {
        return Err(JjPlanError::Parse(format!("invalid repo path: {path}")));
    }

    let repo = (*parts.last().unwrap()).to_string();
    let owner = parts[..parts.len() - 1].join("/");

    Ok(PlatformConfig {
        platform: Platform::Gitea,
        owner,
        repo,
        host: hostname, // Always include host for probe-detected Gitea instances
    })
}

fn extract_hostname(url: &str) -> Option<String> {
    if url.starts_with("git@") {
        return url
            .strip_prefix("git@")
            .and_then(|s| s.split(':').next())
            .map(ToString::to_string);
    }
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(ToString::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}