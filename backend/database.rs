use std::{future::Future, sync::Arc, time::Duration};

/// Schema migrations embedded at compile time.
static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

/// Shared connection to the application database.
#[derive(Clone)]
pub(crate) struct Database {
    /// Toasty connection shared by the stores.
    pub(crate) db: Arc<toasty::Db>,
}

/// Run each retry attempt in a fresh transaction. The body controls whether to
/// commit and must keep side effects inside the transaction.
macro_rules! transaction_with_retry {
    ($database:expr, |mut $tx:ident| $body:block) => {{
        let database = &$database;
        database
            .retry(|| async {
                let mut db = (*database.db).clone();
                let mut $tx = db.transaction().await?;
                $body
            })
            .await
    }};
}

pub(crate) use transaction_with_retry;

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

    /// Retry an operation after a write conflict. Use `transaction_with_retry!`
    /// to give each attempt its own transaction.
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

/// Transaction boundary behavior.
#[cfg(test)]
mod tests {
    use super::Database;
    use crate::storage::Setting;

    /// Dropped transactions roll back while committed writes remain visible.
    #[tokio::test]
    async fn transaction_commits_only_when_requested() {
        let database = Database::open(":memory:").await.unwrap();
        let changed = transaction_with_retry!(database, |mut tx| {
            Setting::upsert_by_name("transaction-test")
                .value("discarded")
                .exec(&mut tx)
                .await?;
            Ok(false)
        })
        .unwrap();
        assert!(!changed);
        let row = Setting::filter_by_name("transaction-test")
            .first()
            .exec(&mut (*database.db).clone())
            .await
            .unwrap();
        assert!(row.is_none());
        transaction_with_retry!(database, |mut tx| {
            Setting::upsert_by_name("transaction-test")
                .value("saved")
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .unwrap();
        let row = Setting::filter_by_name("transaction-test")
            .first()
            .exec(&mut (*database.db).clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.value, "saved");
    }
}
