use crate::storage;
#[derive(Debug, toasty::Model)]
#[table = "request_logs"]
struct LegacyLog {
    #[key]
    id: i64,
    created_at: i64,
    provider: String,
    model: String,
    path: String,
    status: i64,
    latency_ms: i64,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    request_body: String,
    response_body: String,
}

#[derive(Debug, toasty::Model)]
#[table = "sqlite_schema"]
struct SchemaEntry {
    #[key]
    name: String,
    r#type: String,
}

static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Keep the offline import transaction and its cleanup in one auditable sequence.
#[allow(clippy::too_many_lines)]
pub(crate) async fn import(
    source: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !source.is_file() || dest.exists() {
        return Err("source must exist and destination must not exist".into());
    }
    let temp = std::env::temp_dir().join(format!("zero-g-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o700))?;
    }
    let _cleanup = Cleanup(temp.clone());
    let mut wal = source.as_os_str().to_os_string();
    wal.push("-wal");
    if std::path::Path::new(&wal).exists() {
        return Err(
            "source has a WAL file; stop all writers and checkpoint the database before importing"
                .into(),
        );
    }
    let copy = temp.join("source.db");
    std::fs::copy(source, &copy)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(dest)?;
    let source = copy.to_str().ok_or("source path is not UTF-8")?;
    let dest = dest.to_str().ok_or("destination path is not UTF-8")?;
    let mut old = toasty::Db::builder()
        .models(toasty::models!(
            LegacyLog,
            SchemaEntry,
            storage::Setting,
            storage::Secret,
            storage::VirtualKey
        ))
        .connect(&format!("turso:{source}"))
        .await?;
    let tables: Vec<String> = SchemaEntry::all()
        .filter(SchemaEntry::fields().r#type().eq("table"))
        .select(SchemaEntry::fields().name())
        .exec(&mut old)
        .await?;
    if !tables.iter().any(|t| t == "request_logs") {
        return Err("source has no request_logs table".into());
    }
    let logs = LegacyLog::all().exec(&mut old).await?;
    let settings = if tables.iter().any(|t| t == "settings") {
        storage::Setting::all().exec(&mut old).await?
    } else {
        Vec::new()
    };
    let secrets = if tables.iter().any(|t| t == "secrets") {
        storage::Secret::all().exec(&mut old).await?
    } else {
        Vec::new()
    };
    let keys = if tables.iter().any(|t| t == "virtual_keys") {
        storage::VirtualKey::all().exec(&mut old).await?
    } else {
        Vec::new()
    };
    let db = toasty::Db::builder()
        .models(toasty::models!(
            storage::RequestLog,
            storage::Setting,
            storage::Secret,
            storage::VirtualKey
        ))
        .connect(&format!("turso:{dest}"))
        .await?;
    MIGRATIONS.apply(&db).await?;
    drop(db);
    let mut new = toasty::Db::builder()
        .models(toasty::models!(
            LegacyLog,
            storage::Setting,
            storage::Secret,
            storage::VirtualKey
        ))
        .connect(&format!("turso:{dest}"))
        .await?;
    let mut tx = new.transaction().await?;
    for r in logs {
        toasty::create!(LegacyLog {
            id: r.id,
            created_at: r.created_at,
            provider: r.provider,
            model: r.model,
            path: r.path,
            status: r.status,
            latency_ms: r.latency_ms,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            request_body: r.request_body,
            response_body: r.response_body
        })
        .exec(&mut tx)
        .await?;
    }
    for r in settings {
        toasty::create!(storage::Setting {
            name: r.name,
            value: r.value
        })
        .exec(&mut tx)
        .await?;
    }
    for r in secrets {
        toasty::create!(storage::Secret {
            name: r.name,
            value: r.value
        })
        .exec(&mut tx)
        .await?;
    }
    for r in keys {
        toasty::create!(storage::VirtualKey {
            id: r.id,
            name: r.name,
            key_hash: r.key_hash,
            providers: r.providers,
            models: r.models,
            created_at: r.created_at
        })
        .exec(&mut tx)
        .await?;
    }
    tx.commit().await?;
    println!(
        "Import complete. Source unchanged; configure ZERO_G_LOG_DB to use {dest} after verifying the destination."
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn imports_legacy_data_without_modifying_source_or_overwriting_destination() {
        let dir = std::env::temp_dir().join(format!("zero-g-legacy-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let _cleanup = Cleanup(dir.clone());
        let source = dir.join("legacy.db");
        let dest = dir.join("new.db");
        std::fs::write(&source, include_bytes!("../tests/fixtures/legacy.db")).unwrap();
        let before = ring::digest::digest(&ring::digest::SHA256, &std::fs::read(&source).unwrap());

        import(&source, &dest).await.unwrap();
        let after = ring::digest::digest(&ring::digest::SHA256, &std::fs::read(&source).unwrap());
        assert_eq!(before.as_ref(), after.as_ref());

        let mut db = toasty::Db::builder()
            .models(toasty::models!(
                LegacyLog,
                storage::Setting,
                storage::Secret,
                storage::VirtualKey
            ))
            .connect(&format!("turso:{}", dest.display()))
            .await
            .unwrap();
        let log = LegacyLog::get_by_id(&mut db, 42).await.unwrap();
        assert_eq!(log.request_body, "prompt");
        assert_eq!(log.response_body, "response");
        let setting = storage::Setting::get_by_name(&mut db, "log_retention_days")
            .await
            .unwrap();
        assert_eq!(setting.value, "90");
        let secret = storage::Secret::get_by_name(&mut db, "credential")
            .await
            .unwrap();
        assert_eq!(secret.value, [0, 1, 2, 255]);
        let key = storage::VirtualKey::get_by_id(&mut db, "key-id")
            .await
            .unwrap();
        assert_eq!(key.key_hash, [255, 1, 2, 0]);
        drop(db);
        let destination_before = std::fs::read(&dest).unwrap();
        assert!(import(&source, &dest).await.is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), destination_before);
    }
}
