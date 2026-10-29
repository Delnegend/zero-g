use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use crate::{
    provider::{credentials, registry},
    server::settings::SettingsStore,
};

/// Usage remaining within a provider-defined time window.
#[derive(Debug, Serialize)]
pub(crate) struct QuotaWindow {
    /// Display label for the window.
    pub(crate) label: String,
    /// Remaining quota as a percentage, when available.
    pub(crate) remaining_percent: Option<f64>,
    /// Reset timestamp, when provided by the upstream.
    pub(crate) reset_at: Option<i64>,
}

/// Provider usage represented as quota windows or a monetary balance.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum QuotaSnapshot {
    /// Time-windowed quota measurements.
    Quota {
        /// Individual windows reported by the provider.
        windows: Vec<QuotaWindow>,
    },
    /// Monetary account balance.
    Balance {
        /// Balance in the indicated currency.
        amount: f64,
        /// Currency identifier.
        currency: &'static str,
    },
}

/// Returns usage for an existing provider credential when supported.
async fn usage(
    State(store): State<SettingsStore>,
    Path((provider_id, credential_id)): Path<(String, String)>,
) -> Response {
    let provider = match registry::get(&store.database, &provider_id).await {
        Ok(Some(value)) => value,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match credentials::get(&store, &provider_id, &credential_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let Some(capability) =
        super::ProviderKind::from_str(&provider.kind).and_then(super::ProviderKind::quota)
    else {
        return (
            StatusCode::NOT_FOUND,
            "Usage is not available for this provider",
        )
            .into_response();
    };
    let result = capability.fetch(&store, &provider_id, &credential_id).await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(message) => (StatusCode::BAD_GATEWAY, message).into_response(),
    }
}

/// Builds the credential usage route.
pub(crate) fn router(store: SettingsStore) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/usage",
            get(usage),
        )
        .with_state(store)
}

/// Quota parsing and console credential integration checks.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::provider::{
        go::parse_go,
        zen::{ConsoleConnection, connection, parse_balance},
    };
    use crate::storage::{ProviderCredential, ProviderRecord, Secret};
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    /// Console credentials are scoped, encrypted, and removed with their owner.
    #[tokio::test]
    async fn console_credentials_are_scoped_encrypted_and_deleted() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        toasty::create!(ProviderRecord {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            kind: "zen".to_string(),
            base_url: "https://example.com/v1".to_string(),
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "credential".to_string(),
            provider_id: "zen".to_string(),
            name: "Account".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        let create = || ConsoleConnection {
            workspace_id: "workspace".into(),
            session: "private-session".into(),
        };
        assert_eq!(
            connection(&store, "other", "credential", Some(create()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            connection(
                &store,
                "zen",
                "credential",
                Some(ConsoleConnection {
                    workspace_id: "workspace".into(),
                    session: "bad;session".into()
                })
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            connection(&store, "zen", "credential", Some(create()))
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        let secret = Secret::filter_by_name("credential.credential.console_session")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(secret.value, b"private-session");
        assert_eq!(
            store
                .secret_get("credential.credential.console_session")
                .await
                .unwrap()
                .as_deref(),
            Some("private-session")
        );
        let response = credentials::router(store.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/providers/zen/credentials/credential")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            store
                .secret_get("credential.credential.console_session")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .secret_get("credential.credential.console_workspace")
                .await
                .unwrap()
                .is_none()
        );
    }
    /// Balance conversion uses the upstream microcent value.
    #[test]
    fn balance_uses_actual_microcents() {
        let value = serde_json::json!({"balanceMicroCents":"123456789"});
        assert!(
            matches!(parse_balance(&value).unwrap(), QuotaSnapshot::Balance {amount, ..} if (amount-1.234_567_89).abs()<1e-9)
        );
        assert!(parse_balance(&serde_json::json!({"availableMicroCents":"1"})).is_err());
    }
    /// Go usage preserves remaining percentages and requires windows.
    #[test]
    fn go_usage_preserves_remaining_and_rejects_missing_windows() {
        let value = serde_json::json!({"usage":{"rolling":{"percent":25,"resetsAt":"2026-01-01T00:00:00Z"},"weekly":{"percent":80},"monthly":{"percent":101}}});
        let QuotaSnapshot::Quota { windows } = parse_go(&value).unwrap() else {
            panic!()
        };
        assert_eq!(
            windows
                .iter()
                .map(|x| x.remaining_percent)
                .collect::<Vec<_>>(),
            vec![Some(75.0), Some(20.0), Some(0.0)]
        );
        assert!(windows[0].reset_at.is_some());
        assert!(parse_go(&serde_json::json!({"usage":{"rolling":{"percent":25}}})).is_err());
    }
}
