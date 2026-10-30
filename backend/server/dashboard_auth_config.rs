//! Maps Zero-G dashboard environment variables to the shared authentication library.

use axum_auth_gate::{Auth, AuthConfig, AuthMode};

/// Reads a required nonblank environment variable.
fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} must be set"))
}

/// Parses a comma-separated allowlist, dropping blank entries.
fn allowlist(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Loads and validates Zero-G configuration before discovering an OIDC provider.
pub(crate) async fn from_env() -> Result<Auth, String> {
    let mode = required("ZERO_G_DASHBOARD_AUTH")?;
    let public_origin = required("ZERO_G_DASHBOARD_ORIGIN")?;
    let mode = match mode.as_str() {
        "basic" => {
            let username = required("ZERO_G_DASHBOARD_USERNAME")?;
            let password = required("ZERO_G_DASHBOARD_PASSWORD")?;
            AuthMode::Basic {
                username,
                password,
                realm: "zero-g dashboard".into(),
            }
        }
        "oidc" => {
            let issuer = required("ZERO_G_OIDC_ISSUER")?;
            let client_id = required("ZERO_G_OIDC_CLIENT_ID")?;
            let client_secret = required("ZERO_G_OIDC_CLIENT_SECRET")?;
            let allowed_emails =
                allowlist(&std::env::var("ZERO_G_OIDC_ALLOWED_EMAILS").unwrap_or_default());
            let allowed_groups =
                allowlist(&std::env::var("ZERO_G_OIDC_ALLOWED_GROUPS").unwrap_or_default());
            let redirect_url = required("ZERO_G_OIDC_REDIRECT_URL")?;
            AuthMode::Oidc {
                issuer,
                client_id,
                client_secret,
                redirect_url,
                allowed_emails,
                allowed_groups,
            }
        }
        _ => return Err("ZERO_G_DASHBOARD_AUTH must be basic or oidc".into()),
    };
    Auth::new(AuthConfig {
        origin: public_origin,
        mode,
    })
    .await
    .map_err(|error| error.to_string())
}

/// Builds isolated Basic authentication for integration tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) async fn test_auth() -> Auth {
    Auth::new(AuthConfig {
        origin: "http://127.0.0.1:8080".into(),
        mode: AuthMode::Basic {
            username: "test".into(),
            password: "test".into(),
            realm: "zero-g dashboard".into(),
        },
    })
    .await
    .unwrap()
}
