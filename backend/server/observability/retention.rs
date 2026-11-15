use crate::database::{Database, transaction_with_retry};
use crate::server::settings::keys::{BODY_RETENTION_DAYS, LOG_RETENTION_DAYS};
use crate::storage::{LogFilterOption, RequestLog, RequestLogBody};

use super::{logs::LogEntry, now};

/// Read the log metadata retention policy in days.
pub(super) async fn read_retention(exec: &mut dyn toasty::Executor) -> toasty::Result<i64> {
    days(LOG_RETENTION_DAYS.read(exec).await?)
}

/// Convert a validated day count (at most 36500) to the timestamp domain.
fn days(value: u64) -> toasty::Result<i64> {
    i64::try_from(value).map_err(|e| toasty::Error::from_args(format_args!("{e}")))
}

/// Earliest Unix second retained under a retention policy of `days`.
pub(super) fn cutoff(days: i64) -> i64 {
    now().saturating_sub(days.saturating_mul(86400))
}

/// Earliest retained log and body timestamps. Bodies are kept for the shorter
/// of the two policies, so a log can outlive its bodies.
pub(super) async fn cutoffs(exec: &mut dyn toasty::Executor) -> toasty::Result<(i64, i64)> {
    let log_cutoff = cutoff(read_retention(exec).await?);
    let body_cutoff = cutoff(days(BODY_RETENTION_DAYS.read(exec).await?)?).max(log_cutoff);
    Ok((log_cutoff, body_cutoff))
}

/// Delete expired logs, bodies, and filter snapshots using the current persisted policies.
pub(crate) async fn prune_expired(database: &Database) -> Result<(), String> {
    transaction_with_retry!(database, |mut tx| {
        let (log_cutoff, body_cutoff) = cutoffs(&mut tx).await?;
        RequestLog::filter(RequestLog::fields().created_at().lt(log_cutoff))
            .delete()
            .exec(&mut tx)
            .await?;
        RequestLogBody::filter(RequestLogBody::fields().created_at().lt(body_cutoff))
            .delete()
            .exec(&mut tx)
            .await?;
        LogFilterOption::filter(LogFilterOption::fields().last_seen().lt(log_cutoff))
            .delete()
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    })
    .map_err(|err| err.to_string())
}

/// Track the newest retained request for every option and identity-name snapshot.
pub(super) async fn update_filter_options(
    tx: &mut toasty::Transaction<'_>,
    log: &LogEntry,
) -> toasty::Result<()> {
    let options = [
        (!log.provider.is_empty()).then_some((0_i64, log.provider.as_str(), "")),
        (!log.model.is_empty()).then_some((1, log.model.as_str(), "")),
        (!log.path.is_empty()).then_some((2, log.path.as_str(), "")),
        log.virtual_key_id
            .as_deref()
            .zip(log.virtual_key_name.as_deref())
            .map(|(id, name)| (3, id, name)),
        log.credential_id
            .as_deref()
            .zip(log.credential_name.as_deref())
            .map(|(id, name)| (4, id, name)),
    ];
    for (kind, value, name) in options.into_iter().flatten() {
        let id = format!("{kind}:{}:{value}{name}", value.len());
        let existing = LogFilterOption::filter_by_id(id.as_str())
            .first()
            .exec(tx)
            .await?;
        let last_seen = existing.map_or(log.created_at, |row| row.last_seen.max(log.created_at));
        LogFilterOption::upsert_by_id(id.as_str())
            .kind(kind)
            .value(value)
            .name(name)
            .last_seen(last_seen)
            .exec(tx)
            .await?;
    }
    Ok(())
}
