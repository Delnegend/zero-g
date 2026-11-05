//! Current authenticated dashboard user route handler.

use axum::Json;
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

/// Returns the current dashboard user identity based on authentication configuration.
#[utoipa::path(
    get,
    path = "/api/auth/user",
    operation_id = "currentUser",
    responses(
        (status = 200, description = "Current authenticated dashboard user", body = DashboardUser)
    )
)]
pub(crate) async fn current_user() -> Json<DashboardUser> {
    let mode = std::env::var("ZERO_G_DASHBOARD_AUTH").unwrap_or_default();
    let (name, email, avatar) = if mode == "oidc" {
        let email = std::env::var("ZERO_G_OIDC_ALLOWED_EMAILS")
            .ok()
            .and_then(|emails| {
                emails
                    .split(',')
                    .next()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            });
        let name = std::env::var("ZERO_G_OIDC_USER_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                email
                    .as_ref()
                    .and_then(|e| e.split('@').next().map(str::to_owned))
            })
            .unwrap_or_else(|| "OIDC User".to_owned());
        let avatar = std::env::var("ZERO_G_OIDC_USER_PICTURE")
            .ok()
            .filter(|s| !s.trim().is_empty());
        (name, email, avatar)
    } else {
        let name = std::env::var("ZERO_G_DASHBOARD_USERNAME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Admin".to_owned());
        (name, None, None)
    };

    Json(DashboardUser {
        name,
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
        let user = current_user().await;
        assert_ne!(user.name, "");
    }
}
