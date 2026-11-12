use super::*;
use crate::storage::Secret;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

/// Sends a JSON request to a registry test router.
async fn request(
    router: &Router,
    method: &str,
    path: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

/// Credential secrets and updates remain isolated between providers.
#[tokio::test]
async fn credentials_are_isolated_and_encrypted() {
    let database = Database::open(":memory:").await.unwrap();
    let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .await
        .unwrap();
    let router = router(store.clone());
    for id in ["first", "second"] {
        let body = json!({"id": id, "name": id, "kind": "openai", "base_url": format!("https://{id}.example/v1")});
        assert_eq!(
            request(&router, "POST", "/api/providers", &body.to_string())
                .await
                .0,
            StatusCode::CREATED
        );
    }
    let (status, credential) = request(
        &router,
        "POST",
        "/api/providers/first/credentials",
        r#"{"name":"primary","weight":3,"enabled":true,"api_key":"secret"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = credential["id"].as_str().unwrap();
    assert_eq!(credential["connected"], true);
    let secret_name = format!("credential.{id}.api_key");
    let row = Secret::filter_by_name(secret_name.as_str())
        .first()
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(row.value.as_slice(), b"secret");
    assert_eq!(
        store.secret_get(&secret_name).await.unwrap().as_deref(),
        Some("secret")
    );
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/providers/second/credentials/{id}"),
            ""
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("/api/providers/second/credentials/{id}"),
            r#"{"name":"stolen","weight":1,"enabled":true,"api_key":"replacement"}"#
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        store.secret_get(&secret_name).await.unwrap().as_deref(),
        Some("secret")
    );
    assert_eq!(credentials::list(&store, "first").await.unwrap().len(), 1);
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("/api/providers/first/credentials/{id}"),
            r#"{"name":"primary","weight":3,"enabled":false}"#
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(credentials::list(&store, "first").await.unwrap().is_empty());
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("/api/providers/first/credentials/{id}"),
            r#"{"name":"keyless","weight":2,"enabled":true,"api_key":""}"#
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        store.secret_get(&secret_name).await.unwrap().as_deref(),
        Some("")
    );
    assert_eq!(credentials::list(&store, "first").await.unwrap().len(), 1);
}

/// Both disabled and enabled credentials block deletion without losing secrets.
#[tokio::test]
async fn credentials_block_provider_deletion_until_removed() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store.clone());
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers",
            r#"{"id":"sample","name":"Sample","kind":"openai","base_url":"https://example.test"}"#
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, credential) = request(
        &router,
        "POST",
        "/api/providers/sample/credentials",
        r#"{"name":"account","weight":1,"enabled":false,"api_key":"example"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = credential["id"].as_str().unwrap();
    let credential_path = format!("/api/providers/sample/credentials/{id}");
    for enabled in [false, true] {
        if enabled {
            assert_eq!(
                request(
                    &router,
                    "PUT",
                    &credential_path,
                    r#"{"name":"account","weight":1,"enabled":true}"#
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        assert_eq!(
            request(&router, "DELETE", "/api/providers/sample", "")
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            store
                .secret_get(&format!("credential.{id}.api_key"))
                .await
                .unwrap()
                .as_deref(),
            Some("example")
        );
    }
    assert_eq!(
        request(&router, "DELETE", &credential_path, "").await.0,
        StatusCode::NO_CONTENT
    );
    assert!(
        store
            .secret_get(&format!("credential.{id}.api_key"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        request(&router, "DELETE", "/api/providers/sample", "")
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

/// Concurrent creation cannot leave a credential attached to a deleted provider.
#[tokio::test]
async fn credential_creation_and_provider_deletion_are_serialized() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store.clone());
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CREATED
    );
    let (create, delete) = tokio::join!(
        request(
            &router,
            "POST",
            "/api/providers/agy/credentials",
            r#"{"name":"A","weight":1,"enabled":false}"#
        ),
        request(&router, "DELETE", "/api/providers/agy", "")
    );
    match (create.0, delete.0) {
        (StatusCode::CREATED, StatusCode::CONFLICT) => {
            assert!(get(&store.database, "agy").await.unwrap().is_some());
            assert_eq!(
                credentials::all(&store, "agy")
                    .await
                    .unwrap()
                    .unwrap()
                    .len(),
                1
            );
        }
        (StatusCode::NOT_FOUND, StatusCode::NO_CONTENT) => {
            assert!(get(&store.database, "agy").await.unwrap().is_none());
        }
        result => panic!("Unexpected create/delete result: {result:?}"),
    }
}

/// Provider uniqueness and credential input validation are enforced.
#[tokio::test]
async fn provider_uniqueness_and_credential_validation() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store);
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers/agy/credentials",
            r#"{"name":"A","weight":0,"enabled":true}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers/agy/credentials",
            r#"{"name":"A","weight":1,"enabled":true,"api_key":"key"}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers",
            r#"{"id":"c","name":"C","kind":"openai","base_url":"https://example.com/v1"}"#
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers",
            r#"{"id":"d","name":"D","kind":"openai","base_url":"https://example.com/v1/"}"#
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
/// Built-in provider identifiers and names cannot be overridden.
#[tokio::test]
async fn fixed_provider_identity_is_enforced() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store);
    let (_, kinds) = request(&router, "GET", "/api/provider-kinds", "").await;
    assert!(
        kinds["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind["value"] == "agy"
                && kind["fixed_id"] == "agy"
                && kind["fixed_name"] == "Google Antigravity")
    );
    for kind in kinds["kinds"].as_array().unwrap() {
        if kind["fixed_name"].is_string() {
            assert!(kind["label"].is_null());
        } else if kind["value"] == "openai" {
            assert_eq!(kind["label"], "OpenAI-compatible");
        }
    }
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers",
            r#"{"id":"agy","name":"custom","kind":"openai","base_url":"https://example.com"}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers",
            r#"{"id":"wrong","name":"wrong","kind":"agy"}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, provider) = request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(provider["id"], "agy");
    assert_eq!(provider["name"], "Google Antigravity");
    assert_eq!(
        request(
            &router,
            "PUT",
            "/api/providers/agy",
            r#"{"name":"renamed"}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&router, "PUT", "/api/providers/agy", "{}").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn zen_rejects_configured_upstream_urls() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store);
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/providers",
            r#"{"kind":"zen","base_url":"https://example.com/v1"}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"zen"}"#)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            "/api/providers/zen",
            r#"{"base_url":"https://example.com/v1"}"#
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

/// Recreating a provider respects existing virtual-key restrictions.
#[tokio::test]
async fn provider_recreation_respects_virtual_keys() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store.clone());
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(&router, "DELETE", "/api/providers/agy", "").await.0,
        StatusCode::NO_CONTENT
    );
    let mut db = (*store.database.db).clone();
    toasty::create!(VirtualKey {
        id: "key".to_string(),
        name: "restricted".to_string(),
        key_hash: vec![1],
        providers: "[\"agy\"]".to_string(),
        models: "[]".to_string(),
        created_at: 0,
        expires_at: Some(1),
        revoked_at: None
    })
    .exec(&mut db)
    .await
    .unwrap();
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CONFLICT
    );
    VirtualKey::upsert_by_id("key")
        .name("restricted")
        .key_hash(vec![1])
        .providers("[\"agy\"]")
        .models("[]")
        .created_at(0)
        .expires_at(Some(1))
        .revoked_at(Some(1))
        .exec(&mut db)
        .await
        .unwrap();
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(&router, "DELETE", "/api/providers/agy", "").await.0,
        StatusCode::NO_CONTENT
    );
    toasty::create!(VirtualKey {
        id: "all".to_string(),
        name: "all".to_string(),
        key_hash: vec![3],
        providers: "[]".to_string(),
        models: "[]".to_string(),
        created_at: 0,
        expires_at: None,
        revoked_at: None
    })
    .exec(&mut db)
    .await
    .unwrap();
    assert_eq!(
        request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_custom_provider_recreation_conflicts(&router).await;
}

/// Custom provider recreation is blocked by an unrestricted virtual key.
async fn assert_custom_provider_recreation_conflicts(router: &Router) {
    assert_eq!(
        request(
            router,
            "POST",
            "/api/providers",
            r#"{"id":"custom","name":"Custom","kind":"openai","base_url":"https://example.com"}"#
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(router, "DELETE", "/api/providers/custom", "")
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(
            router,
            "POST",
            "/api/providers",
            r#"{"id":"custom","name":"Custom","kind":"openai","base_url":"https://example.com"}"#
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

/// Creates a store with data for identity cutover checks.
async fn cutover_fixture() -> SettingsStore {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let mut db = (*store.database.db).clone();
    Setting::delete_by_name(&mut db, "provider_fixed_identities_v1")
        .await
        .unwrap();
    toasty::create!(ProviderRecord {
        id: "old".to_string(),
        name: "Old".to_string(),
        kind: "agy".to_string(),
        base_url: String::new(),
        keyless: false,
        deleted: false,
    })
    .exec(&mut db)
    .await
    .unwrap();
    toasty::create!(ProviderRecord {
        id: "agy".to_string(),
        name: String::new(),
        kind: "openai".to_string(),
        base_url: String::new(),
        keyless: false,
        deleted: true,
    })
    .exec(&mut db)
    .await
    .unwrap();
    toasty::create!(ProviderCredential {
        id: "cred".to_string(),
        provider_id: "old".to_string(),
        name: "account".to_string(),
        weight: 1,
        enabled: true
    })
    .exec(&mut db)
    .await
    .unwrap();
    toasty::create!(VirtualKey {
        id: "key".to_string(),
        name: "restricted".to_string(),
        key_hash: vec![2],
        providers: "[\"old\"]".to_string(),
        models: "[]".to_string(),
        created_at: 0,
        expires_at: None,
        revoked_at: None
    })
    .exec(&mut db)
    .await
    .unwrap();
    store
}

/// Identity cutover is atomic and retains access restrictions.
#[tokio::test]
async fn identity_cutover_is_atomic_and_preserves_restrictions() {
    let store = cutover_fixture().await;
    let mut db = (*store.database.db).clone();
    assert!(
        migrate_fixed_identities(&store)
            .await
            .unwrap_err()
            .contains("occupied")
    );
    assert_eq!(
        ProviderCredential::filter_by_id("cred")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        "old"
    );
    assert_eq!(
        VirtualKey::filter_by_id("key")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap()
            .providers,
        "[\"old\"]"
    );
    ProviderRecord::delete_by_id(&mut db, "agy").await.unwrap();
    VirtualKey::upsert_by_id("key")
        .name("restricted".to_string())
        .key_hash(vec![2])
        .providers("[\"agy\"]".to_string())
        .models("[]".to_string())
        .created_at(0)
        .exec(&mut db)
        .await
        .unwrap();
    assert!(
        migrate_fixed_identities(&store)
            .await
            .unwrap_err()
            .contains("already references")
    );
    VirtualKey::upsert_by_id("key")
        .name("restricted".to_string())
        .key_hash(vec![2])
        .providers("[\"old\"]".to_string())
        .models("[]".to_string())
        .created_at(0)
        .exec(&mut db)
        .await
        .unwrap();
    migrate_fixed_identities(&store).await.unwrap();
    assert_eq!(
        get(&store.database, "agy").await.unwrap().unwrap().name,
        "Google Antigravity"
    );
    assert!(get(&store.database, "old").await.unwrap().is_none());
    assert_eq!(
        ProviderCredential::filter_by_id("cred")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        "agy"
    );
    assert_eq!(
        VirtualKey::filter_by_id("key")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap()
            .providers,
        "[\"agy\"]"
    );
    migrate_fixed_identities(&store).await.unwrap();
}

#[tokio::test]
async fn keyless_provider_lifecycle_and_cascade_delete() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let router = router(store.clone());

    let (status, provider) = request(
        &router,
        "POST",
        "/api/providers",
        r#"{"id":"ollama","name":"Ollama","kind":"openai","base_url":"http://localhost:11434/v1","keyless":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(provider["keyless"], true);

    let (status, _) = request(
        &router,
        "POST",
        "/api/providers/ollama/credentials",
        r#"{"name":"default","weight":1,"enabled":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = request(&router, "DELETE", "/api/providers/ollama", "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(get(&store.database, "ollama").await.unwrap().is_none());
    assert!(
        credentials::list(&store, "ollama")
            .await
            .unwrap()
            .is_empty()
    );
}
