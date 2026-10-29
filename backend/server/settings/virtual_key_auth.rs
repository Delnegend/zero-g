use super::{SettingsStore, internal_error};
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};

/// Authenticates a virtual key and attaches provider and permission context.
pub(crate) async fn virtual_key_auth(
    State((store, kind)): State<(SettingsStore, &'static str)>,
    Path(id): Path<String>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    let provider = match crate::provider::registry::get(&store.database, &id).await {
        Ok(Some(provider)) if provider.kind == kind => provider,
        Ok(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(super::super::error_body(
                    "Provider not found",
                    "invalid_request_error",
                    Some("provider_not_found"),
                )),
            )
                .into_response();
        }
        Err(err) => return internal_error(&err),
    };
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim());
    let Some(token) = token else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(super::super::error_body(
                "A virtual key is required",
                "authentication_error",
                None,
            )),
        )
            .into_response();
    };
    match store.permissions(token).await {
        Ok(Some(permissions))
            if permissions.providers.is_empty()
                || permissions.providers.iter().any(|p| p == &id) =>
        {
            let session =
                crate::provider::common::session::RequestSession::new(req.headers(), token, &id);
            req.extensions_mut().insert(session);
            req.extensions_mut().insert(provider);
            req.extensions_mut().insert(permissions);
            next.run(req).await
        }
        Ok(Some(_)) => (
            StatusCode::FORBIDDEN,
            Json(super::super::error_body(
                "Virtual key does not permit this provider",
                "permission_error",
                None,
            )),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(super::super::error_body(
                "Invalid virtual key",
                "authentication_error",
                None,
            )),
        )
            .into_response(),
        Err(err) => internal_error(&err),
    }
}
