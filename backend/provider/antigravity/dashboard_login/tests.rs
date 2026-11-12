//! Tests for dashboard Google OAuth flows and callback handling.

use super::{
    Flow, OAuthFlows, agy_finish, agy_poll, agy_start, complete_google, parse_google_callback,
};
use crate::provider::antigravity::auth::{self as agy, AuthManager, Credentials};
use crate::server::settings::SettingsStore;
use axum::{Json, Router, http::StatusCode, routing::post};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use openidconnect::PkceCodeChallenge;
use openidconnect::PkceCodeVerifier;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tower::ServiceExt;

/// Sends a request to the dashboard routes.
async fn request(
    router: &Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("host", "dashboard.test")
                .header("origin", "http://dashboard.test")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"error":String::from_utf8_lossy(&bytes)}));
    (status, value)
}

/// Verifies the OAuth wire protocol and failed onboarding through dashboard routes.
#[tokio::test]
#[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
async fn oauth_pkce_project_and_expiry() {
    use crate::{
        database::Database,
        provider::{credentials, registry},
    };
    use base64::Engine as _;
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let config: toml::Value = toml::from_str(include_str!("../../oauth-clients.toml")).unwrap();
    let decode = |key: &str| {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(config["antigravity"][key].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    let expected_id = decode("client_id");
    let expected_secret = decode("client_secret");
    assert_eq!(
        request(
            &registry::router(store.clone()),
            "POST",
            "/api/providers",
            json!({"kind":"agy"})
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, created) = request(
        &credentials::router(store.clone()),
        "POST",
        "/api/providers/agy/credentials",
        json!({"name":"primary","weight":1,"enabled":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    let old = Credentials {
        access_token: "old-access".into(),
        refresh_token: Some("old-refresh".into()),
        project_id: "old-project".into(),
        expires_at: None,
        email: Some("old@example.test".into()),
    };
    AuthManager::save(&store, "agy", id, &old).await.unwrap();
    let challenge = Arc::new(tokio::sync::Mutex::new(String::new()));
    let project_exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let onboarding_error = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fake = Router::new()
            .route("/token", post({
                let challenge = challenge.clone();
                let expected_id = expected_id.clone();
                let expected_secret = expected_secret.clone();
                move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                    let challenge = challenge.clone();
                    let expected_id = expected_id.clone();
                    let expected_secret = expected_secret.clone();
                    async move {
                        if form.get("grant_type").is_some_and(|v| v == "refresh_token") {
                            let valid = form.get("client_id") == Some(&expected_id)
                                && form.get("client_secret") == Some(&expected_secret)
                                && form.get("refresh_token").is_some_and(|v| v == "new-refresh");
                            return if valid {
                                (StatusCode::OK, Json(json!({"access_token":"refreshed-access","expires_in":3600})))
                            } else {
                                (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"})))
                            };
                        }
                        let verifier = PkceCodeVerifier::new(form.get("code_verifier").cloned().unwrap_or_default());
                        let valid = verifier.secret().len() >= 43 && verifier.secret().len() <= 128
                            && PkceCodeChallenge::from_code_verifier_sha256(&verifier).as_str() == *challenge.lock().await
                            && form.get("client_id") == Some(&expected_id)
                            && form.get("client_secret") == Some(&expected_secret)
                            && form.get("code").is_some_and(|v| v == "authorized-code")
                            && form.get("redirect_uri").is_some_and(|v| v == agy::OAUTH_CALLBACK_URL);
                        if valid { (StatusCode::OK, Json(json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}))) }
                        else { (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))) }
                    }
                }
            }))
            .route("/userinfo", axum::routing::get(|| async { Json(json!({"email":"user@example.test"})) }))
            .route("/v1internal:loadCodeAssist", post({
                let project_exists = project_exists.clone();
                move || {
                    let project_exists = project_exists.clone();
                    async move {
                        if project_exists.load(std::sync::atomic::Ordering::SeqCst) { Json(json!({"cloudaicompanionProject":"real-project"})) }
                        else { Json(json!({"currentTier":{"name":"free-tier"}})) }
                    }
                }
            }))
            .route("/v1internal:onboardUser", post({
                let onboarding_error = onboarding_error.clone();
                move || {
                    let onboarding_error = onboarding_error.clone();
                    async move {
                        if onboarding_error.load(std::sync::atomic::Ordering::SeqCst) {
                            Json(json!({"done":true,"error":{"message":"account ineligible"}}))
                        } else {
                            Json(json!({"done":true}))
                        }
                    }
                }
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
    let flows = OAuthFlows {
        token_url: format!("{base}/token"),
        userinfo_url: format!("{base}/userinfo"),
        cloud_url: base,
        ..OAuthFlows::default()
    };
    let _listener_guard = flows.google_listener.lock().await;
    let router = Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/agy/start",
            post(agy_start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/agy/finish",
            post(agy_finish),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/agy/poll",
            post(agy_poll),
        )
        .with_state((store.clone(), flows.clone()));
    let start = format!("/api/providers/agy/credentials/{id}/agy/start");
    let finish = format!("/api/providers/agy/credentials/{id}/agy/finish");
    let poll = format!("/api/providers/agy/credentials/{id}/agy/poll");
    let (status, flow) = request(&router, "POST", &start, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let state = flow["id"].as_str().unwrap();
    let url = reqwest::Url::parse(flow["url"].as_str().unwrap()).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params.get("client_id"), Some(&expected_id));
    assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(params.get("access_type").unwrap(), "offline");
    assert!(
        params
            .get("scope")
            .unwrap()
            .contains("https://www.googleapis.com/auth/cloud-platform")
    );
    *challenge.lock().await = params.get("code_challenge").unwrap().clone();
    assert_eq!(
        request(&router, "POST", &poll, json!({"id":state})).await.1["status"],
        "pending"
    );
    let callback =
        format!("http://127.0.0.1:51123/oauth-callback?state={state}&code=authorized-code");
    let wrong_state = "http://127.0.0.1:51123/oauth-callback?state=wrong&code=authorized-code";
    assert_ne!(
        request(
            &router,
            "POST",
            &finish,
            json!({"callback_url":wrong_state})
        )
        .await
        .0,
        StatusCode::OK
    );
    let cross_origin = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&finish)
                .header("content-type", "application/json")
                .header("host", "dashboard.test")
                .header("origin", "http://attacker.test")
                .body(Body::from(json!({"callback_url":callback}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(cross_origin.status(), StatusCode::OK);
    assert_ne!(
        request(&router, "POST", &finish, json!({"callback_url":callback}))
            .await
            .0,
        StatusCode::OK
    );
    let (_, failed) = request(&router, "POST", &poll, json!({"id":state})).await;
    assert_eq!(failed["status"], "failed");
    assert!(failed["error"].as_str().unwrap().contains("project ID"));
    let stored = AuthManager::load(store.clone(), "agy", id)
        .await
        .unwrap()
        .unwrap()
        .get_credentials()
        .await
        .unwrap();
    assert_eq!(stored.access_token, "old-access");
    assert_eq!(stored.refresh_token.as_deref(), Some("old-refresh"));
    assert_eq!(stored.project_id, "old-project");
    onboarding_error.store(true, std::sync::atomic::Ordering::SeqCst);
    let (_, error_flow) = request(&router, "POST", &start, json!({})).await;
    let error_state = error_flow["id"].as_str().unwrap();
    let url = reqwest::Url::parse(error_flow["url"].as_str().unwrap()).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    *challenge.lock().await = params.get("code_challenge").unwrap().clone();
    let callback =
        format!("http://127.0.0.1:51123/oauth-callback?state={error_state}&code=authorized-code");
    assert_ne!(
        request(&router, "POST", &finish, json!({"callback_url":callback}))
            .await
            .0,
        StatusCode::OK
    );
    let (_, failed) = request(&router, "POST", &poll, json!({"id":error_state})).await;
    assert_eq!(failed["status"], "failed");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("account ineligible")
    );
    onboarding_error.store(false, std::sync::atomic::Ordering::SeqCst);
    project_exists.store(true, std::sync::atomic::Ordering::SeqCst);
    let (_, flow) = request(&router, "POST", &start, json!({})).await;
    let state = flow["id"].as_str().unwrap();
    let url = reqwest::Url::parse(flow["url"].as_str().unwrap()).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    *challenge.lock().await = params.get("code_challenge").unwrap().clone();
    let callback =
        format!("http://127.0.0.1:51123/oauth-callback?state={state}&code=authorized-code");
    assert_eq!(
        request(&router, "POST", &finish, json!({"callback_url":callback}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "POST", &poll, json!({"id":state})).await.1["status"],
        "complete"
    );
    assert_ne!(
        request(&router, "POST", &finish, json!({"callback_url":callback}))
            .await
            .0,
        StatusCode::OK
    );
    let stored = AuthManager::load(store.clone(), "agy", id)
        .await
        .unwrap()
        .unwrap()
        .get_credentials()
        .await
        .unwrap();
    assert_eq!(stored.access_token, "new-access");
    assert_eq!(stored.project_id, "real-project");
    let manager = AuthManager::load(store.clone(), "agy", id)
        .await
        .unwrap()
        .unwrap();
    manager.refresh_at(&flows.token_url).await.unwrap();
    assert_eq!(
        manager.get_credentials().await.unwrap().access_token,
        "refreshed-access"
    );
    let (_, flow) = request(&router, "POST", &start, json!({})).await;
    let state = flow["id"].as_str().unwrap();
    flows.pending.lock().await.get_mut(state).unwrap().created = Instant::now()
        .checked_sub(Duration::from_secs(601))
        .unwrap();
    let callback =
        format!("http://127.0.0.1:51123/oauth-callback?state={state}&code=authorized-code");
    assert_ne!(
        request(&router, "POST", &finish, json!({"callback_url":callback}))
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(
        request(&router, "POST", &poll, json!({"id":state})).await.0,
        StatusCode::OK
    );
    server.abort();
}

#[test]
fn flow_is_bound_to_provider_and_credential() {
    let flow = Flow {
        provider_id: "google-one".into(),
        credential_id: "google-key".into(),
        created: Instant::now(),
        in_flight: false,
        verifier: "test".into(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
    };
    assert!(flow.belongs_to("google-one", "google-key"));
    assert!(!flow.belongs_to("google-two", "google-key"));
    assert!(!flow.belongs_to("google-one", "other-key"));
}

#[test]
fn callback_rejects_wrong_redirect_and_duplicate_state() {
    assert_eq!(
        parse_google_callback("http://127.0.0.1:51123/oauth-callback?state=abc&code=x%2By"),
        Ok(("abc".to_string(), "x+y".to_string()))
    );
    assert!(parse_google_callback("http://localhost:51123/oauth-callback?state=a&code=b").is_err());
    assert!(
        parse_google_callback("http://127.0.0.1:51123/oauth-callback?state=a&state=b&code=c")
            .is_err()
    );
    assert!(parse_google_callback("http://127.0.0.1:51123/oauth-callback?state=&code=c").is_err());
}

#[tokio::test]
async fn simultaneous_google_flows_keep_results_bound_to_credentials() {
    let flows = OAuthFlows::default();
    for id in ["success", "failure"] {
        flows.pending.lock().await.insert(
            id.into(),
            Flow {
                provider_id: "account".into(),
                credential_id: id.into(),
                created: Instant::now(),
                in_flight: true,
                verifier: "test".into(),
                shutdown: Arc::new(tokio::sync::Notify::new()),
            },
        );
    }
    complete_google(&flows, "success", Ok(())).await;
    assert!(flows.pending.lock().await.contains_key("failure"));
    complete_google(&flows, "failure", Err("exchange failed".into())).await;
    let results = flows.results.lock().await;
    let (_, provider, credential, result) = results.get("success").unwrap();
    assert_eq!(
        (provider.as_str(), credential.as_str()),
        ("account", "success")
    );
    assert!(result.is_ok());
    let (_, provider, credential, result) = results.get("failure").unwrap();
    assert_eq!(
        (provider.as_str(), credential.as_str()),
        ("account", "failure")
    );
    assert_eq!(result.as_ref().unwrap_err(), "exchange failed");
    drop(results);
}

#[tokio::test]
async fn google_listener_has_a_single_local_callback_owner() {
    let flows = OAuthFlows::default();
    let guard = flows.google_listener.clone().try_lock_owned();
    assert!(guard.is_ok());
    assert!(flows.google_listener.clone().try_lock_owned().is_err());
    drop(guard);
    assert!(flows.google_listener.try_lock_owned().is_ok());
}
