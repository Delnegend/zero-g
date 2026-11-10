//! Current authenticated dashboard user route handler.

use axum::{Json, extract::Extension};
use axum_auth_gate::User as Identity;
use serde::Serialize;
use utoipa::ToSchema;

/// Authenticated dashboard user identity details.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct DashboardUser {
    /// User's display name.
    pub(crate) name: String,
    /// User's verified email address, if available.
    pub(crate) email: Option<String>,
    /// URL pointing to the user's profile picture or avatar.
    pub(crate) avatar: Option<String>,
}

/// Reads a nonblank environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Returns the first nonblank entry of the configured email allowlist.
fn allowed_email() -> Option<String> {
    env("ZERO_G_OIDC_ALLOWED_EMAILS").and_then(|emails| {
        emails
            .split(',')
            .map(str::trim)
            .find(|email| !email.is_empty())
            .map(str::to_owned)
    })
}

/// Returns the current dashboard user identity based on authentication configuration.
#[utoipa::path(
    get,
    path = "/api/auth/user",
    operation_id = "currentUser",
    responses(
        (status = 200, description = "Current authenticated dashboard user", body = DashboardUser)
    )
)]
pub(crate) async fn current_user(identity: Option<Extension<Identity>>) -> Json<DashboardUser> {
    let oidc = env("ZERO_G_DASHBOARD_AUTH").as_deref() == Some("oidc");
    let fallback_email = oidc.then(allowed_email).flatten();
    let fallback_name = if oidc {
        env("ZERO_G_OIDC_USER_NAME").or_else(|| {
            fallback_email
                .as_ref()
                .and_then(|email| email.split('@').next().map(str::to_owned))
        })
    } else {
        env("ZERO_G_DASHBOARD_USERNAME")
    };
    let fallback_avatar = oidc.then(|| env("ZERO_G_OIDC_USER_PICTURE")).flatten();

    let (name, email, avatar) = if let Some(Extension(identity)) = identity {
        (
            identity.name.or(fallback_name),
            identity.email.or(fallback_email),
            identity.avatar.or(fallback_avatar),
        )
    } else {
        (fallback_name, fallback_email, fallback_avatar)
    };

    Json(DashboardUser {
        name: name.unwrap_or_else(|| {
            if oidc {
                "OIDC User".to_owned()
            } else {
                "Admin".to_owned()
            }
        }),
        email,
        avatar,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_user_defaults_to_admin() {
        let user = current_user(None).await;
        assert_ne!(user.name, "");
    }

    #[tokio::test]
    async fn current_user_prefers_authenticated_identity() {
        let identity = Identity {
            name: Some("Test User".to_owned()),
            email: Some("test@example.com".to_owned()),
            avatar: Some("https://example.com/avatar.png".to_owned()),
        };
        let Json(user) = current_user(Some(Extension(identity))).await;
        assert_eq!(user.name, "Test User");
        assert_eq!(user.email.as_deref(), Some("test@example.com"));
        assert_eq!(
            user.avatar.as_deref(),
            Some("https://example.com/avatar.png")
        );
    }
}
