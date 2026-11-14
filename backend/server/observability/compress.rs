//! One-time conversion of plain-text bodies into compressed storage.

use toasty::stmt::{Type, Value};
use tracing::info;

use super::logs::compress;
use crate::database::{Database, transaction_with_retry};
use crate::storage::RequestLogBody;

/// Rows converted per transaction, bounding memory and write-lock time.
const BATCH: i64 = 50;

/// Compress bodies left in the legacy `request_log_bodies` table, then drop it.
///
/// Each batch moves rows and deletes them from the legacy table in one
/// transaction, so an interrupted run resumes where it stopped.
pub(crate) async fn compress_legacy_bodies(database: &Database) -> Result<(), String> {
    let mut db = (*database.db).clone();
    let exists = toasty::sql::query(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'request_log_bodies'",
    )
    .column_types([Type::I64])
    .exec(&mut db)
    .await
    .map_err(|e| e.to_string())?;
    if !matches!(first_i64(&exists), Some(n) if n > 0) {
        return Ok(());
    }
    let mut moved = 0_usize;
    loop {
        let count = transaction_with_retry!(database, |mut tx| {
            let rows = toasty::sql::query(
                "SELECT log_id, created_at, request_body, response_body FROM request_log_bodies ORDER BY log_id LIMIT ?1",
            )
            .bind(BATCH)
            .column_types([Type::I64, Type::I64, Type::String, Type::String])
            .exec(&mut tx)
            .await?;
            let mut last = None;
            for row in &rows {
                let [
                    Value::I64(log_id),
                    Value::I64(created_at),
                    request,
                    response,
                ] = row.as_record_unwrap().fields.as_slice()
                else {
                    return Err(toasty::Error::from_args(format_args!(
                        "Unexpected legacy body row"
                    )));
                };
                let text = |value: &Value| {
                    compress(value.as_str().unwrap_or_default()).map_err(|e| {
                        toasty::Error::from_args(format_args!("Could not compress body: {e}"))
                    })
                };
                RequestLogBody::upsert_by_log_id(*log_id)
                    .created_at(*created_at)
                    .request_body(text(request)?)
                    .response_body(text(response)?)
                    .exec(&mut tx)
                    .await?;
                last = Some(*log_id);
            }
            if let Some(last) = last {
                toasty::sql::statement("DELETE FROM request_log_bodies WHERE log_id <= ?1")
                    .bind(last)
                    .exec(&mut tx)
                    .await?;
            }
            tx.commit().await?;
            Ok(rows.len())
        })
        .map_err(|e| e.to_string())?;
        moved = moved.saturating_add(count);
        if i64::try_from(count).unwrap_or(i64::MAX) < BATCH {
            break;
        }
        info!(moved, "compressing stored request log bodies");
    }
    toasty::sql::statement("DROP TABLE request_log_bodies")
        .exec(&mut db)
        .await
        .map_err(|e| e.to_string())?;
    info!(moved, "compressed stored request log bodies");
    Ok(())
}

/// First column of the first row as an integer.
fn first_i64(rows: &[Value]) -> Option<i64> {
    match rows.first()?.as_record()?.fields.first()? {
        Value::I64(n) => Some(*n),
        _ => None,
    }
}
