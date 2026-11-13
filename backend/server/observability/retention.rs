use crate::database::{Database, transaction_with_retry};
use crate::storage::{LogFilterOption, RequestLog, RequestLogBody, Setting};

use super::{logs::LogEntry, now};

/// Read the current retention policy from the same transaction as its consumer.
pub(super) async fn read_retention(tx: &mut toasty::Transaction<'_>) -> toasty::Result<i64> {
    let setting = Setting::filter_by_name("log_retention_days")
        .first()
        .exec(tx)
        .await?;
    let days = match setting {
        Some(setting) => setting.value.parse::<i64>().map_err(|_| {
            toasty::Error::from_args(format_args!(
                "log_retention_days must be a positive integer"
            ))
        })?,
        None => 30,
    };
    if !(1..=36500).contains(&days) {
        return Err(toasty::Error::from_args(format_args!(
            "log_retention_days must be between 1 and 36500"
        )));
    }
    Ok(days)
}

/// Earliest Unix second retained under a retention policy of `days`.
pub(super) fn cutoff(days: i64) -> i64 {
    now().saturating_sub(days.saturating_mul(86400))
}

/// Delete expired logs, their bodies, and filter snapshots using the current persisted policy.
pub(crate) async fn prune_expired(database: &Database) -> Result<(), String> {
    transaction_with_retry!(database, |mut tx| {
        let retention_days = read_retention(&mut tx).await?;
        let cutoff = cutoff(retention_days);
        RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
            .delete()
            .exec(&mut tx)
            .await?;
        RequestLogBody::filter(RequestLogBody::fields().created_at().lt(cutoff))
            .delete()
            .exec(&mut tx)
            .await?;
        LogFilterOption::filter(LogFilterOption::fields().last_seen().lt(cutoff))
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
