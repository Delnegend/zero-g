use std::sync::Arc;

use axum::{
    Extension, Router,
    body::Bytes,
    extract::{RawQuery, State},
    http::{HeaderMap, Method},
    response::{IntoResponse, Response},
    routing::post,
};

use crate::{
    provider::{
        common::proxy::{check_model, retry_api_keys},
        common::request_identity::RequestIdentity,
        openai,
        registry::Provider,
    },
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Fixed upstream API root for a Decisions request.
#[derive(Clone)]
struct ApiRoot(Arc<str>);

/// Build the OpenRouter-only private Decisions route.
pub(super) fn router(settings: &SettingsStore, api_root: &str) -> Router<AppState> {
    Router::new()
        .route("/v1/decisions", post(decide))
        .layer(Extension(ApiRoot(Arc::from(api_root))))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "openrouter"),
            settings::virtual_key_auth,
        ))
}

/// Forward a permitted Decisions payload to `OpenRouter` without translating it.
#[allow(clippy::too_many_arguments)]
async fn decide(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    Extension(ApiRoot(api_root)): Extension<ApiRoot>,
    identity: Option<Extension<RequestIdentity>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(error) = check_model(&body, &permissions) {
        return error.into_response();
    }
    let response = retry_api_keys(
        &state.settings,
        &provider.id,
        "No connected OpenRouter credentials",
        identity.as_ref().map(|Extension(identity)| identity),
        |key| {
            let body = body.clone();
            let api_root = &api_root;
            let headers = &headers;
            let query = &query;
            async move {
                openai::send(
                    api_root,
                    &key,
                    "alpha/decisions",
                    query.as_deref(),
                    Method::POST,
                    headers,
                    Some(body),
                )
                .await
            }
        },
    )
    .await;
    match response {
        Ok(response) => openai::forward(response),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        database::Database,
        server::observability::LogStore,
        storage::{ProviderCredential, ProviderRecord, Secret, VirtualKey},
    };
    use axum::{
        Json,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
        routing::post,
    };
    use ring::digest;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Decisions reach the correct upstream endpoint with credential failover and intact payloads.
    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
    async fn forwards_decisions_and_enforces_permissions() {
        let (send, mut received) = tokio::sync::mpsc::channel::<(HeaderMap, Value)>(3);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let upstream = Router::new().route("/api/alpha/decisions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let send = send.clone();
            let count = count.clone();
            async move {
                send.send((headers, body)).await.unwrap();
                if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":"retry"})))
                } else {
                    (StatusCode::OK, Json(json!({"answers":{"ok":{"type":"noul","noul":0.9}}, "usage":{"input_tokens":4,"output_tokens":0}})))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}/api", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let database = Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*database.db).clone();
        toasty::create!(ProviderRecord {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: "openrouter".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        for id in ["first", "second"] {
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: "openrouter".to_string(),
                name: id.to_string(),
                weight: 1,
                enabled: true
            })
            .exec(&mut db)
            .await
            .unwrap();
            let name = format!("credential.{id}.api_key");
            Secret::upsert_by_name(name.as_str())
                .value(store.encrypt(&name, &format!("upstream-{id}")).unwrap())
                .exec(&mut db)
                .await
                .unwrap();
        }
        let token = "fictional-virtual-key";
        toasty::create!(VirtualKey {
            id: "key".to_string(),
            name: "key".to_string(),
            key_hash: digest::digest(&digest::SHA256, token.as_bytes())
                .as_ref()
                .to_vec(),
            providers: r#"["openrouter"]"#.to_string(),
            models: r#"["cloudflare/clef"]"#.to_string(),
            created_at: 0
        })
        .exec(&mut db)
        .await
        .unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            logs: LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
            settings: store.clone(),
        };
        let app = Router::new()
            .nest("/openrouter/{id}", router(&store, &root))
            .with_state(state);
        let request = |model: &str| {
            Request::builder().method("POST")
            .uri("/openrouter/openrouter/v1/decisions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"model":model,"state":["example",{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}],"questions":{"ok":{"type":"noul","instructions":"Is it valid?"}}}).to_string())).unwrap()
        };
        let denied = app
            .clone()
            .oneshot(request("cloudflare/other"))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let response = app.oneshot(request("cloudflare/clef")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(value["answers"]["ok"]["noul"], 0.9);
        assert_eq!(value["usage"]["input_tokens"], 4);
        let first = received.recv().await.unwrap();
        let second = received.recv().await.unwrap();
        for (headers, body) in [&first, &second] {
            let authorization = headers[header::AUTHORIZATION].to_str().unwrap();
            assert!(
                authorization == "Bearer upstream-first"
                    || authorization == "Bearer upstream-second"
            );
            assert_eq!(body["model"], "cloudflare/clef");
            assert_eq!(
                body["state"][1]["image_url"]["url"],
                "data:image/png;base64,AAAA"
            );
            assert_eq!(body["questions"]["ok"]["type"], "noul");
        }
        assert_ne!(
            first.0[header::AUTHORIZATION],
            second.0[header::AUTHORIZATION]
        );
        server.abort();
    }
}
