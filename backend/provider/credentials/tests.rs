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
    assert!(error.starts_with("Failed to parse stored Codex credentials: "));
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

#[tokio::test]
/// Merging legacy credentials must not widen virtual-key access.
#[allow(clippy::too_many_lines)]
async fn legacy_merge_preserves_credentials_and_refuses_to_widen_key_access() {
    let store = SettingsStore::new(
        Database::open(":memory:").await.unwrap(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let mut db = (*store.database.db).clone();
    Setting::delete_by_name(&mut db, "credential_pool_migrated_v1")
        .await
        .unwrap();
    for id in ["first", "second"] {
        toasty::create!(ProviderRecord {
            id: id.to_string(),
            name: id.to_string(),
            kind: "agy".to_string(),
            base_url: String::new(),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        let name = format!("provider.{id}.credential");
        let encrypted = store.encrypt(&name, id).unwrap();
        Secret::upsert_by_name(name.as_str())
            .value(encrypted)
            .exec(&mut db)
            .await
            .unwrap();
    }
    toasty::create!(VirtualKey {
        id: "key".to_string(),
        name: "restricted".to_string(),
        key_hash: vec![1],
        providers: "[\"first\"]".to_string(),
        models: "[]".to_string(),
        created_at: 1,
        expires_at: None,
        revoked_at: None
    })
    .exec(&mut db)
    .await
    .unwrap();

    let error = migrate_legacy(&store).await.unwrap_err();
    assert!(error.contains("Virtual key 'restricted'"));
    assert!(error.contains("Remove or recreate"));
    assert_eq!(registry::list(&store.database).await.unwrap().len(), 2);
    assert_eq!(
        store
            .secret_get("provider.first.credential")
            .await
            .unwrap()
            .as_deref(),
        Some("first")
    );
    assert!(
        ProviderCredential::all()
            .exec(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .setting_get("credential_pool_migrated_v1")
            .await
            .unwrap()
            .is_none()
    );

    VirtualKey::upsert_by_id("key")
        .name("restricted".to_string())
        .key_hash(vec![1])
        .providers("[\"first\",\"second\"]".to_string())
        .models("[]".to_string())
        .created_at(1)
        .exec(&mut db)
        .await
        .unwrap();
    migrate_legacy(&store).await.unwrap();
    assert_eq!(registry::list(&store.database).await.unwrap().len(), 1);
    let credentials = list(&store, "first").await.unwrap();
    assert_eq!(credentials.len(), 2);
    let mut values = Vec::new();
    for credential in credentials {
        values.push(
            store
                .secret_get(&format!("credential.{}.credential", credential.id))
                .await
                .unwrap()
                .unwrap(),
        );
    }
    values.sort();
    assert_eq!(values, ["first", "second"]);
    assert!(
        store
            .secret_get("provider.first.credential")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .secret_get("provider.second.credential")
            .await
            .unwrap()
            .is_none()
    );
    let key = VirtualKey::filter_by_id("key")
        .first()
        .exec(&mut db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key.providers, "[\"first\"]");
    migrate_legacy(&store).await.unwrap();
    assert_eq!(list(&store, "first").await.unwrap().len(), 2);
}
