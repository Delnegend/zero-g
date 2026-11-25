use axum::http::StatusCode;
use axum::{
    Extension, Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{
    openai::ModelListResponse,
    provider::{claude::models, common::request_identity::RequestIdentity, registry::Provider},
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Build virtual-key-authenticated Claude routes: model listing only. Messages go through
/// the Anthropic pass-through at `/v1/messages`.
pub(super) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(list))
        .route("/v1/models/", get(list))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "claude"),
            settings::virtual_key_auth,
        ))
}

/// Construct a gateway error response.
fn failure(status: StatusCode, message: &str) -> Response {
    let kind = if status.is_client_error() {
        "invalid_request_error"
    } else {
        "upstream_error"
    };
    (status, Json(crate::server::error_body(message, kind, None))).into_response()
}

/// Return models visible to the virtual key.
async fn list(
    State(state): State<AppState>,
    Extension(provider): Extension<Provider>,
    Extension(permissions): Extension<Permissions>,
    identity: Option<Extension<RequestIdentity>>,
) -> Response {
    match models::discover(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(mut models) => {
            models.retain(|m| permissions.allows_model(&m.id));
            Json(ModelListResponse {
                object: "list".into(),
                data: models,
            })
            .into_response()
        }
        Err(e) => failure(StatusCode::BAD_GATEWAY, &e),
    }
}
