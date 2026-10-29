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
    document.merge(crate::server::model_setting::Api::openapi());
    document.merge(crate::server::inference::Api::openapi());
    document.merge(crate::provider::registry::Api::openapi());
    document.merge(crate::provider::credentials::Api::openapi());
    document.merge(crate::provider::quota::Api::openapi());
    document.merge(crate::provider::zen::Api::openapi());
    document.merge(crate::provider::antigravity::dashboard_login::Api::openapi());
    document.merge(crate::provider::codex::dashboard_login::Api::openapi());
    document
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
        let document: Value = serde_json::to_value(openapi()).unwrap();
        let settings = &document["paths"]["/api/settings"];
        assert_eq!(settings["get"]["operationId"], "getSettings");
        assert_eq!(settings["put"]["operationId"], "updateSettings");
        assert_eq!(
            settings["put"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/UpdateSettings"
        );
        for method in ["get", "put"] {
            assert_eq!(
                settings[method]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/SettingsResponse"
            );
        }
        for schema in ["SettingsResponse", "UpdateSettings"] {
            let retention =
                &document["components"]["schemas"][schema]["properties"]["log_retention_days"];
            assert_eq!(retention["minimum"], 0);
            assert_eq!(retention["maximum"], 4_294_967_295u64);
        }
    }
}
