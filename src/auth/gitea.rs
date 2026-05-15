//! Gitea authentication.

use crate::auth::AuthSource;
use crate::error::{JjPlanError, Result};
use crate::platform::error::{Operation, checked_response};
use crate::types::Platform;
use reqwest::Client;
use serde::Deserialize;
use std::env;

/// Gitea authentication configuration.
#[derive(Debug, Clone)]
pub struct GiteaAuthConfig {
    pub token: String,
    pub source: AuthSource,
    pub host: String,
}

/// Get Gitea authentication.
///
/// Priority:
/// 1. `GITEA_TOKEN` environment variable
///
/// Host resolution: `host` parameter → `GITEA_HOST` env var → error (no default).
pub async fn get_gitea_auth(host: Option<&str>) -> Result<GiteaAuthConfig> {
    let host = host
        .map(String::from)
        .or_else(|| env::var("GITEA_HOST").ok())
        .ok_or_else(|| {
            JjPlanError::Auth(
                "No Gitea host configured. Set GITEA_HOST or use a remote with a known Gitea hostname."
                    .to_string(),
            )
        })?;

    if let Ok(token) = env::var("GITEA_TOKEN") {
        return Ok(GiteaAuthConfig {
            token,
            source: AuthSource::EnvVar,
            host,
        });
    }

    Err(JjPlanError::Auth(
        "No Gitea authentication found. Set GITEA_TOKEN environment variable.".to_string(),
    ))
}

#[derive(Deserialize)]
struct GiteaUser {
    login: String,
}

/// Test Gitea authentication.
pub async fn test_gitea_auth(config: &GiteaAuthConfig) -> Result<String> {
    let url = format!("https://{}/api/v1/user", config.host);

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| JjPlanError::Config(format!("failed to build Gitea HTTP client: {e}")))?;

    let response = client
        .get(&url)
        .header("Authorization", format!("token {}", config.token))
        .send()
        .await?;

    let user: GiteaUser = checked_response(
        response,
        Platform::Gitea,
        Operation::TestAuth,
        Some(config.host.clone()),
    )
    .await?;

    Ok(user.login)
}
