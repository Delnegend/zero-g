use utoipa::OpenApi;

/// Root `OpenAPI` document with package metadata.
#[derive(OpenApi)]
struct Api;

/// Combines provider and server contracts for generation and documentation.
pub(crate) fn openapi() -> utoipa::openapi::OpenApi {
    let mut document = Api::openapi();
    document.merge(crate::server::Api::openapi());
    document.merge(crate::server::dashboard_auth::Api::openapi());
    document.merge(crate::server::settings::Api::openapi());
    document.merge(crate::server::observability::Api::openapi());
    document.merge(crate::server::model_catalog::Api::openapi());
    document.merge(crate::server::model_settings::Api::openapi());
    document.merge(crate::server::inference::Api::openapi());
    document.merge(crate::provider::registry::Api::openapi());
    document.merge(crate::provider::credentials::Api::openapi());
    document.merge(crate::provider::quota::Api::openapi());
    document.merge(crate::provider::zen::Api::openapi());
    document.merge(crate::provider::antigravity::dashboard_login::Api::openapi());
    document.merge(crate::provider::codex::dashboard_login::Api::openapi());
    document
}

/// Serializes the `OpenAPI` document as formatted JSON for client generation.
#[cfg(test)]
fn openapi_json() -> Result<String, serde_json::Error> {
    openapi().to_pretty_json()
}

/// Settings API contract coverage.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Checks operation IDs, response schemas, and retention integer bounds.
    #[test]
    fn settings_contract_exposes_operations_and_u32_bounds() {
        let json = openapi_json().unwrap();
        if let Some(path) = std::env::var_os("ZERO_G_OPENAPI_OUTPUT") {
            std::fs::write(path, &json).unwrap();
        }
        let document: Value = serde_json::from_str(&json).unwrap();
        let settings = &document["paths"]["/api/settings"];
        assert_eq!(settings["get"]["operationId"], "getSettings");
        assert_eq!(settings["put"]["operationId"], "updateSettings");
        assert!(
            settings["put"]["requestBody"]["content"]["application/json"]["schema"].is_object()
        );
        for method in ["get", "put"] {
            assert!(
                settings[method]["responses"]["200"]["content"]["application/json"]["schema"]
                    .is_object()
            );
        }
        for schema in ["SettingsResponse", "UpdateSettings"] {
            let retention =
                &document["components"]["schemas"][schema]["properties"]["log_retention_days"];
            assert_eq!(retention["minimum"], 0);
            assert_eq!(retention["maximum"], 4_294_967_295u64);
        }
    }

    /// Consumers can discover dashboard, provider, and inference operations.
    #[test]
    fn contract_exposes_dashboard_and_inference_operations() {
        let document = openapi();
        for (path, method) in [
            ("/api/observability", "get"),
            ("/api/model-catalog", "get"),
            ("/api/model-catalog/refresh", "post"),
            ("/api/model-settings", "get"),
            ("/api/model-settings", "put"),
            ("/api/model-settings/data", "get"),
            ("/api/model-settings/sync", "post"),
            ("/api/keys", "get"),
            ("/api/keys", "post"),
            ("/api/keys/{id}", "delete"),
            ("/api/provider-kinds", "get"),
            ("/api/providers", "get"),
            ("/api/providers", "post"),
            ("/api/providers/{id}", "put"),
            ("/api/providers/{id}", "delete"),
            ("/api/providers/{id}/credentials", "get"),
            ("/api/providers/{id}/credentials", "post"),
            ("/api/providers/{id}/credentials/{credential_id}", "put"),
            ("/api/providers/{id}/credentials/{credential_id}", "delete"),
            (
                "/api/providers/{id}/credentials/{credential_id}/usage",
                "get",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/console",
                "put",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/console",
                "delete",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/agy/start",
                "post",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/agy/finish",
                "post",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/agy/poll",
                "post",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/codex/start",
                "post",
            ),
            (
                "/api/providers/{id}/credentials/{credential_id}/codex/poll",
                "post",
            ),
            ("/v1/models", "get"),
            ("/v1/chat/completions", "post"),
            ("/v1/responses", "post"),
        ] {
            let path = document.paths.paths.get(path).unwrap();
            let present = match method {
                "get" => path.get.is_some(),
                "post" => path.post.is_some(),
                "put" => path.put.is_some(),
                "delete" => path.delete.is_some(),
                _ => false,
            };
            assert!(present);
        }
    }
}
