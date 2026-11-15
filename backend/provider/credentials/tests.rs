use super::*;
use crate::database::Database;

/// Typed OAuth reads revalidate ownership and observe the latest stored secret.
#[tokio::test]
async fn oauth_load_revalidates_and_reads_current_secret() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let mut db = (*store.database.db).clone();
    for id in ["owner", "other"] {
        toasty::create!(ProviderRecord {
            id: id.to_string(),
            name: id.to_string(),
            kind: "codex".to_string(),
            base_url: String::new(),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
    }
    toasty::create!(ProviderCredential {
        id: "account".to_string(),
        provider_id: "owner".to_string(),
        name: "Account".to_string(),
        weight: 1,
        enabled: true
    })
    .exec(&mut db)
    .await
    .unwrap();
    let lock = crate::provider::login::credential_lock("account").await;
    let _guard = lock.lock().await;
    assert!(
        load_oauth::<serde_json::Value>(&store, "owner", "account", "codex", "Codex")
            .await
            .unwrap()
            .is_none()
    );
    let name = "credential.account.credential";
    for value in ["first", "rotated"] {
        let json = serde_json::json!({"access_token": value}).to_string();
        Secret::upsert_by_name(name)
            .value(store.encrypt(name, &json).unwrap())
            .exec(&mut db)
            .await
            .unwrap();
        let loaded = load_oauth::<serde_json::Value>(&store, "owner", "account", "codex", "Codex")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded["access_token"], value);
    }
    assert!(
        load_oauth::<serde_json::Value>(&store, "other", "account", "codex", "Codex")
            .await
            .is_err()
    );
    assert!(
        load_oauth::<serde_json::Value>(&store, "owner", "account", "agy", "Antigravity")
            .await
            .is_err()
    );
    Secret::upsert_by_name(name)
        .value(store.encrypt(name, "not json").unwrap())
        .exec(&mut db)
        .await
        .unwrap();
    let error = load_oauth::<serde_json::Value>(&store, "owner", "account", "codex", "Codex")
        .await
        .unwrap_err();
    assert_eq!(error, "Invalid stored Codex credentials");
}

/// Dashboard credential lists expose account profiles without exposing OAuth secrets.
#[tokio::test]
async fn dashboard_lists_stored_account_profiles_without_tokens() {
    use axum::body::to_bytes;
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let mut db = (*store.database.db).clone();
    for kind in ["agy", "codex", "claude"] {
        toasty::create!(ProviderRecord {
            id: kind.to_string(),
            name: kind.to_string(),
            kind: kind.to_string(),
            base_url: String::new(),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        let id = format!("{kind}-account");
        toasty::create!(ProviderCredential {
            id: id.clone(),
            provider_id: kind.to_string(),
            name: "Personal".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        let name = format!("credential.{id}.credential");
        let json = serde_json::json!({
            "email": "person@example.test",
            "plan_type": "pro",
            "access_token": "secret-token"
        })
        .to_string();
        Secret::upsert_by_name(name.as_str())
            .value(store.encrypt(&name, &json).unwrap())
            .exec(&mut db)
            .await
            .unwrap();
        let response = list_endpoint(State(store.clone()), Path(kind.to_string())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["credentials"][0]["email"], "person@example.test");
        if kind == "codex" {
            assert_eq!(value["credentials"][0]["plan_type"], "pro");
        } else {
            assert!(value["credentials"][0].get("plan_type").is_none());
        }
        assert!(!String::from_utf8_lossy(&body).contains("secret-token"));
    }
}
