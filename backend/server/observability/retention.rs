use crate::database::{Database, transaction_with_retry};
use crate::storage::{LogFilterOption, RequestLog, RequestLogBody, Setting};

use super::{logs::LogEntry, now};

/// Default days to keep request log metadata, charts, and totals.
pub(crate) const LOG_RETENTION_DEFAULT: i64 = 90;
/// Default days to keep captured request and response bodies.
pub(crate) const BODY_RETENTION_DEFAULT: i64 = 7;

/// Read a retention policy in days from the same transaction as its consumer.
async fn read_days(
    tx: &mut toasty::Transaction<'_>,
    name: &str,
    default: i64,
) -> toasty::Result<i64> {
    let setting = Setting::filter_by_name(name).first().exec(tx).await?;
    let days = match setting {
        Some(setting) => setting.value.parse::<i64>().map_err(|_| {
            toasty::Error::from_args(format_args!("{name} must be a positive integer"))
        })?,
        None => default,
    };
    if !(1..=36500).contains(&days) {
        return Err(toasty::Error::from_args(format_args!(
            "{name} must be between 1 and 36500"
        )));
    }
    Ok(days)
}

/// Read the log metadata retention policy.
pub(super) async fn read_retention(tx: &mut toasty::Transaction<'_>) -> toasty::Result<i64> {
    read_days(tx, "log_retention_days", LOG_RETENTION_DEFAULT).await
}

/// Earliest Unix second retained under a retention policy of `days`.
pub(super) fn cutoff(days: i64) -> i64 {
    now().saturating_sub(days.saturating_mul(86400))
}

/// Delete expired logs, bodies, and filter snapshots using the current persisted policies.
///
/// Bodies are kept for the shorter of the two policies, so a log can outlive its bodies.
pub(crate) async fn prune_expired(database: &Database) -> Result<(), String> {
    transaction_with_retry!(database, |mut tx| {
        let log_cutoff = cutoff(read_retention(&mut tx).await?);
        let body_days = read_days(&mut tx, "body_retention_days", BODY_RETENTION_DEFAULT).await?;
        let body_cutoff = cutoff(body_days).max(log_cutoff);
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
