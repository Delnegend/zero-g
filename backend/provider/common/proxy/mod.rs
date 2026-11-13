use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

use crate::server::settings::Permissions;
use crate::server::settings::SettingsStore;
use std::future::Future;

/// Resolve weighted API keys in order and retry only credential-related responses.
pub(crate) async fn retry_api_keys<F, Fut>(
    settings: &SettingsStore,
    provider: &crate::provider::registry::Provider,
    no_credentials: &'static str,
    identity: Option<&crate::provider::common::request_identity::RequestIdentity>,
    mut send: F,
) -> Result<reqwest::Response, ProxyError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<reqwest::Response, &'static str>>,
{
    if provider.keyless {
        let response = send(String::new()).await.map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if response.status().is_success()
            && let Some(identity) = identity
        {
            identity.set_credential("keyless", "Keyless");
        }
        return Ok(response);
    }
    let credentials = crate::provider::credentials::list(settings, &provider.id)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    for credential in credentials {
        let key = settings
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| settings_error())?
            .ok_or_else(settings_error)?;
        let response = send(key).await.map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if !crate::provider::common::errors::retryable_status(Some(response.status())) {
            if response.status().is_success()
                && let Some(identity) = identity
            {
                identity.set_credential(&credential.id, &credential.name);
            }
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            no_credentials,
            "upstream_error",
            "no_credentials",
        )
    })
}

/// Forward unrestricted model responses unchanged, filtering successful restricted lists.
pub(crate) async fn model_response(
    response: reqwest::Response,
    permissions: &Permissions,
) -> Response {
    if !response.status().is_success() || permissions.models.is_empty() {
        return crate::provider::openai::forward(response);
    }
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let Ok(mut value) = response.json::<Value>().await else {
        return invalid_model_list();
    };
    if !filter_models(&mut value, permissions) {
        return invalid_model_list();
    }
    let mut result = Response::new(Body::from(value.to_string()));
    *result.status_mut() = status;
    if let Some(value) = content_type {
        result.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    result
}

/// Reports an upstream model response that cannot be filtered.
fn invalid_model_list() -> Response {
    error(
        StatusCode::BAD_GATEWAY,
        "Invalid upstream model list",
        "upstream_error",
        "bad_gateway",
    )
    .into_response()
}

/// OpenAI-compatible error response with its HTTP status and error metadata.
#[derive(Debug)]
pub(crate) struct ProxyError(StatusCode, &'static str, &'static str, &'static str);

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::server::error_body(self.1, self.2, Some(self.3))),
        )
            .into_response()
    }
}
impl ProxyError {
    /// HTTP status.
    pub(crate) const fn status(&self) -> StatusCode {
        self.0
    }
    /// Error message.
    pub(crate) const fn message(&self) -> &'static str {
        self.1
    }
    /// Wire error type.
    pub(crate) const fn kind(&self) -> &'static str {
        self.2
    }
    /// Error code.
    pub(crate) const fn code(&self) -> &'static str {
        self.3
    }
}

/// Constructs a proxy error from its status and wire-level error fields.
pub(crate) const fn error(
    status: StatusCode,
    message: &'static str,
    kind: &'static str,
    code: &'static str,
) -> ProxyError {
    ProxyError(status, message, kind, code)
}

/// Reports failure to load OpenAI-compatible provider settings.
pub(crate) const fn settings_error() -> ProxyError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to load OpenAI-compatible provider settings",
        "server_error",
        "internal_error",
    )
}

/// Checks every model the upstream can select against key permissions.
pub(crate) fn check_model(body: &[u8], permissions: &Permissions) -> Result<(), ProxyError> {
    // Reject duplicate selection fields and track case variants: some upstream
    // JSON decoders treat these as aliases even though serde does not.
    #[derive(Default)]
    struct Selection {
        model: Option<Value>,
        models: Option<Value>,
        case_alias: bool,
    }
    impl<'de> serde::Deserialize<'de> for Selection {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Selection;

                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a request object")
                }

                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Selection, A::Error> {
                    let mut selection = Selection::default();
                    while let Some(key) = map.next_key::<String>()? {
                        let slot = match key.as_str() {
                            "model" => Some(&mut selection.model),
                            "models" => Some(&mut selection.models),
                            _ => None,
                        };
                        if let Some(slot) = slot {
                            if slot.is_some() {
                                return Err(serde::de::Error::custom(
                                    "duplicate model selection field",
                                ));
                            }
                            *slot = Some(map.next_value()?);
                        } else {
                            selection.case_alias |=
                                key.chars().flat_map(char::to_uppercase).eq("MODEL".chars())
                                    || key
                                        .chars()
                                        .flat_map(char::to_uppercase)
                                        .eq("MODELS".chars());
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                    Ok(selection)
                }
            }
            deserializer.deserialize_map(Visitor)
        }
    }
    const fn invalid() -> ProxyError {
        error(
            StatusCode::BAD_REQUEST,
            "A valid model or models list is required",
            "invalid_request_error",
            "invalid_model",
        )
    }
    fn check(model: &Value, permissions: &Permissions) -> Result<(), ProxyError> {
        let model = model
            .as_str()
            .filter(|model| !model.is_empty())
            .ok_or_else(invalid)?;
        if !permissions.allows_model(model) {
            return Err(error(
                StatusCode::FORBIDDEN,
                "Model is not allowed for this virtual key",
                "permission_error",
                "model_not_allowed",
            ));
        }
        Ok(())
    }

    let selection: Selection = serde_json::from_slice(body).map_err(|_| invalid())?;
    if !permissions.models.is_empty() && selection.case_alias {
        return Err(invalid());
    }
    if let Some(model) = &selection.model {
        check(model, permissions)?;
    }
    if let Some(models) = &selection.models {
        // An unrestricted key with a valid primary model need not interpret
        // provider-specific fallback formats.
        if !permissions.models.is_empty() || selection.model.is_none() {
            let models = models
                .as_array()
                .filter(|models| !models.is_empty())
                .ok_or_else(invalid)?;
            for model in models {
                check(model, permissions)?;
            }
        }
    } else if selection.model.is_none() {
        return Err(invalid());
    }
    Ok(())
}

/// Filters a model-list response by key permissions; returns false if `data` is not an array.
pub(crate) fn filter_models(value: &mut Value, permissions: &Permissions) -> bool {
    let Some(data) = value.get_mut("data").and_then(Value::as_array_mut) else {
        return false;
    };
    data.retain(|model| {
        model
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| permissions.allows_model(id))
    });
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
