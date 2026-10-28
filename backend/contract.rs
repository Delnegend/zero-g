use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::server::settings::get_settings,
        crate::server::settings::put_settings
    ),
    components(schemas(
        crate::server::settings::SettingsResponse,
        crate::server::settings::UpdateSettings
    ))
)]
struct SettingsApi;

fn openapi_json() -> Result<String, serde_json::Error> {
    SettingsApi::openapi().to_pretty_json()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;

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
}
