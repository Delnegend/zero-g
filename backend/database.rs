use std::{future::Future, sync::Arc, time::Duration};

/// Schema migrations embedded at compile time.
static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

/// Shared connection to the application database.
#[derive(Clone)]
pub(crate) struct Database {
    /// Toasty connection shared by the stores.
    pub(crate) db: Arc<toasty::Db>,
}

impl Database {
    /// Opens the database, registers models, and applies embedded migrations.
    pub(crate) async fn open(path: &str) -> Result<Self, String> {
        if path != ":memory:" {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            if let Err(err) = options.open(path)
                && err.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(err.to_string());
            }
        }
        let db = toasty::Db::builder()
            .models(toasty::models!(
                crate::storage::RequestLog,
                crate::storage::Setting,
                crate::storage::Secret,
                crate::storage::VirtualKey,
                crate::storage::ProviderRecord,
                crate::storage::ProviderCredential
            ))
            .connect(&format!("turso:{path}"))
            .await
            .map_err(|e| e.to_string())?;
        MIGRATIONS.apply(&db).await.map_err(|e| e.to_string())?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Retry the whole database operation after a write conflict. Callers must
    /// create a fresh transaction on every attempt and keep side effects inside it.
    pub(crate) async fn retry<T, F, Fut>(&self, mut operation: F) -> Result<T, toasty::Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, toasty::Error>>,
    {
        let start = tokio::time::Instant::now();
        let mut delay = Duration::from_millis(5);
        loop {
            match operation().await {
                Err(error)
                    if error.is_serialization_failure()
                        && start.elapsed() < Duration::from_secs(5) =>
                {
                    let jitter = Duration::from_millis(rand::random_range(0..=20));
                    tokio::time::sleep(delay.saturating_add(jitter)).await;
                    delay = delay.saturating_mul(2).min(Duration::from_millis(250));
                }
                result => return result,
            }
        }
    }
}
