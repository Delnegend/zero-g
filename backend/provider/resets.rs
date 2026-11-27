//! Banked quota resets.
//!
//! Claude and Codex sometimes grant an account a voucher that refills its usage windows at once.
//! Each voucher expires. These routes list them and spend one on request.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;

use crate::{provider::quota::authorize, provider::registry, server::settings::SettingsStore};

/// A reset the account holds and may spend.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ResetCredit {
    /// Provider identifier of the reset, safe to put in a URL path.
    pub(crate) id: String,
    /// What the reset refills, as the provider names it.
    pub(crate) label: String,
    /// Resets of this kind still unspent.
    pub(crate) remaining: u32,
    /// Expiry in Unix seconds, when the provider reports one.
    #[schema(required = true)]
    pub(crate) expires_at: Option<i64>,
    /// Whether spending it now would be accepted.
    pub(crate) usable: bool,
    /// Why it cannot be used right now.
    #[schema(required = true)]
    pub(crate) note: Option<String>,
}

/// The resets an account holds.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
pub(crate) struct ResetCredits {
    /// Unspent, unexpired resets.
    pub(crate) credits: Vec<ResetCredit>,
    /// Why the provider offers none to this account, when it says.
    #[schema(required = true)]
    pub(crate) note: Option<String>,
}

/// Outcome of spending a reset.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RedeemResult {
    /// Whether the provider refilled the usage windows.
    pub(crate) redeemed: bool,
    /// Plain-language outcome for the dashboard.
    pub(crate) message: String,
}

/// Credential reset API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list, redeem),
    components(schemas(ResetCredit, ResetCredits, RedeemResult))
)]
pub(crate) struct Api;

/// Current Unix time in seconds.
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

/// Whether an identifier from the provider is safe to forward and to put in a URL path.
pub(crate) fn is_safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Keep a provider-supplied reason short and plain before showing it.
pub(crate) fn plain_reason(reason: &str) -> String {
    reason
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' '))
        .take(40)
        .collect()
}

/// Lists the resets an existing credential holds, when the provider supports them.
#[utoipa::path(get, path = "/api/providers/{id}/credentials/{credential_id}/resets", operation_id = "getCredentialResets", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 200, body = ResetCredits), (status = 404, description = "Provider or credential not found, or resets are not available"), (status = 500, description = "Failed to load provider or credential"), (status = 502, description = "Failed to fetch resets", body = String, content_type = "text/plain")))]
async fn list(
    State(store): State<SettingsStore>,
    Path((provider_id, credential_id)): Path<(String, String)>,
) -> Response {
    let provider = match authorize(&store, &provider_id, &credential_id).await {
        Ok(provider) => provider,
        Err(status) => return status.into_response(),
    };
    let Some(capability) = capability(&provider) else {
        return unsupported();
    };
    match capability.list(&store, &provider_id, &credential_id).await {
        Ok(resets) => Json(resets).into_response(),
        Err(message) => (StatusCode::BAD_GATEWAY, message).into_response(),
    }
}

/// Spends one reset. Resets cannot be undone or put back.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/resets/{reset_id}/redeem", operation_id = "redeemCredentialReset", params(("id" = String, Path), ("credential_id" = String, Path), ("reset_id" = String, Path)), responses((status = 200, body = RedeemResult), (status = 400, description = "Invalid reset identifier", body = String, content_type = "text/plain"), (status = 404, description = "Provider or credential not found, or resets are not available"), (status = 500, description = "Failed to load provider or credential"), (status = 502, description = "The provider did not confirm the reset", body = String, content_type = "text/plain")))]
async fn redeem(
    State(store): State<SettingsStore>,
    Path((provider_id, credential_id, reset_id)): Path<(String, String, String)>,
) -> Response {
    let provider = match authorize(&store, &provider_id, &credential_id).await {
        Ok(provider) => provider,
        Err(status) => return status.into_response(),
    };
    let Some(capability) = capability(&provider) else {
        return unsupported();
    };
    if !is_safe_id(&reset_id) {
        return (StatusCode::BAD_REQUEST, "Invalid reset identifier").into_response();
    }
    match capability
        .redeem(&store, &provider_id, &credential_id, &reset_id)
        .await
    {
        Ok(result) => {
            tracing::info!(
                provider = %provider_id,
                credential = %credential_id,
                redeemed = result.redeemed,
                "Quota reset requested"
            );
            Json(result).into_response()
        }
        Err(message) => (StatusCode::BAD_GATEWAY, message).into_response(),
    }
}

/// The provider's reset capability, if its kind has one.
fn capability(provider: &registry::Provider) -> Option<super::ResetCapability> {
    super::ProviderKind::from_str(&provider.kind).and_then(super::ProviderKind::resets)
}

/// Response for providers without resets.
fn unsupported() -> Response {
    (
        StatusCode::NOT_FOUND,
        "Resets are not available for this provider",
    )
        .into_response()
}

/// Builds the credential reset routes.
pub(crate) fn router(store: SettingsStore) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/resets",
            get(list),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/resets/{reset_id}/redeem",
            post(redeem),
        )
        .with_state(store)
}

/// Reset route checks that need no upstream.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::storage::{ProviderCredential, ProviderRecord};
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    /// Store with a Zen and a Claude provider, each owning one credential.
    async fn store() -> SettingsStore {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        for (provider, kind) in [("zen", "zen"), ("claude", "claude")] {
            toasty::create!(ProviderRecord {
                id: provider.to_string(),
                name: provider.to_string(),
                kind: kind.to_string(),
                base_url: String::new(),
                keyless: false,
                deleted: false
            })
            .exec(&mut db)
            .await
            .unwrap();
            toasty::create!(ProviderCredential {
                id: format!("{provider}-credential"),
                provider_id: provider.to_string(),
                name: "Account".to_string(),
                weight: 1,
                enabled: true
            })
            .exec(&mut db)
            .await
            .unwrap();
        }
        store
    }

    /// Status of a request to the reset routes.
    async fn status(store: &SettingsStore, method: &str, path: &str) -> StatusCode {
        router(store.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Resets are limited to providers that have them, to credentials the provider owns, and to safe identifiers.
    #[tokio::test]
    async fn routes_check_provider_credential_and_identifier() {
        let store = store().await;
        let list = |provider: &str, credential: &str| {
            format!("/api/providers/{provider}/credentials/{credential}/resets")
        };
        // A provider without resets, an unknown provider, and another provider's credential.
        for path in [
            list("zen", "zen-credential"),
            list("missing", "zen-credential"),
            list("claude", "zen-credential"),
        ] {
            assert_eq!(
                status(&store, "GET", &path).await,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        let redeem = |reset: &str| {
            format!("/api/providers/claude/credentials/claude-credential/resets/{reset}/redeem")
        };
        // An identifier that could change the upstream path is refused before any upstream call.
        for reset in ["a.b", "a%2Fb", "%2e%2e", "x".repeat(65).as_str()] {
            assert_eq!(
                status(&store, "POST", &redeem(reset)).await,
                StatusCode::BAD_REQUEST,
                "{reset}"
            );
        }
        // Spending is POST only.
        assert_eq!(
            status(&store, "GET", &redeem("abc")).await,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    /// Only plain identifiers pass, and reasons are cut down to plain words.
    #[test]
    fn identifiers_and_reasons_are_plain() {
        assert!(is_safe_id("fable-1_A"));
        for bad in ["", "a b", "a/b", "a.b", "é", &"x".repeat(65)] {
            assert!(!is_safe_id(bad), "{bad}");
        }
        assert_eq!(plain_reason("not<b>_limited\n"), "notb_limited");
        assert_eq!(plain_reason(&"a".repeat(100)).len(), 40);
    }
}
