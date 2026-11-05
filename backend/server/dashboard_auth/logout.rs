//! Dashboard session termination route handler.

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};

/// Queries the `OpenID` Connect discovery document to find `end_session_endpoint`.
pub(crate) async fn discover_end_session_endpoint(issuer: &str) -> Option<String> {
    if let Ok(override_url) = std::env::var("ZERO_G_OIDC_END_SESSION_ENDPOINT")
        && !override_url.trim().is_empty()
    {
        return Some(override_url);
    }
    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let response = client.get(&discovery_url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let metadata: serde_json::Value = response.json().await.ok()?;
    metadata
        .get("end_session_endpoint")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// Constructs the final RP-initiated logout redirect URL with standard query parameters.
pub(crate) fn build_logout_redirect_url(
    end_session_endpoint: &str,
    post_logout_redirect_uri: Option<&str>,
    client_id: Option<&str>,
) -> String {
    reqwest::Url::parse(end_session_endpoint).map_or_else(
        |_| end_session_endpoint.to_owned(),
        |mut url| {
            if let Some(redirect_uri) = post_logout_redirect_uri.filter(|s| !s.trim().is_empty()) {
                url.query_pairs_mut()
                    .append_pair("post_logout_redirect_uri", redirect_uri);
                if let Some(id) = client_id.filter(|s| !s.trim().is_empty()) {
                    url.query_pairs_mut().append_pair("client_id", id);
                }
            }
            url.to_string()
        },
    )
}

/// Clears session cookies and redirects to the OIDC `end_session_endpoint` or dashboard root.
#[utoipa::path(
    get,
    post,
    path = "/auth/logout",
    operation_id = "logout",
    responses(
        (status = 303, description = "Redirect to end session endpoint or dashboard root after clearing session cookies")
    )
)]
pub(crate) async fn logout() -> Response {
    let clear_http = "axum_auth_gate_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0";
    let clear_https =
        "__Host-axum_auth_gate_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Secure";

    let redirect_target = if let Ok(issuer) = std::env::var("ZERO_G_OIDC_ISSUER") {
        discover_end_session_endpoint(&issuer).await.map_or_else(
            || "/".to_owned(),
            |end_session| {
                let origin = std::env::var("ZERO_G_DASHBOARD_ORIGIN").ok();
                let client_id = std::env::var("ZERO_G_OIDC_CLIENT_ID").ok();
                build_logout_redirect_url(&end_session, origin.as_deref(), client_id.as_deref())
            },
        )
    } else {
        "/".to_owned()
    };

    let mut response = Redirect::to(&redirect_target).into_response();
    if let Ok(value) = HeaderValue::from_str(clear_http) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    if let Ok(value) = HeaderValue::from_str(clear_https) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn logout_clears_cookies_and_redirects() {
        let response = logout().await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookies: Vec<_> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert!(
            cookies
                .iter()
                .any(|c| c.contains("axum_auth_gate_session=;"))
        );
        assert!(
            cookies
                .iter()
                .any(|c| c.contains("__Host-axum_auth_gate_session=;"))
        );
    }

    #[test]
    fn test_build_logout_redirect_url() {
        let base = "https://id.delnegend.com/api/oidc/end-session";
        let with_params =
            build_logout_redirect_url(base, Some("http://127.0.0.1:5173/"), Some("zero-g-client"));
        assert!(with_params.starts_with(base));
        assert!(with_params.contains("post_logout_redirect_uri=http%3A%2F%2F127.0.0.1%3A5173%2F"));
        assert!(with_params.contains("client_id=zero-g-client"));

        let without_params = build_logout_redirect_url(base, None, None);
        assert_eq!(without_params, base);
    }
}
