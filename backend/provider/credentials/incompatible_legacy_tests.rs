use super::*;
use crate::database::Database;

/// Legacy Zen instances share the same official upstream regardless of old shim URLs.
#[tokio::test]
async fn different_zen_upstreams_are_merged() {
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
    for id in ["one", "two"] {
        toasty::create!(ProviderRecord {
            id: id.to_string(),
            name: id.to_string(),
            kind: "zen".to_string(),
            base_url: format!("https://{id}.example/v1"),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
    }
    migrate_legacy(&store).await.unwrap();
    assert_eq!(registry::list(&store.database).await.unwrap().len(), 1);
}
