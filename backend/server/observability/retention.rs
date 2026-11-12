use crate::storage::{LogFilterOption, RequestLog};

use super::LogEntry;

/// Delete expired logs and their retained filter snapshots in the same transaction.
pub(super) async fn prune_expired(
    tx: &mut toasty::Transaction<'_>,
    cutoff: i64,
) -> toasty::Result<()> {
    RequestLog::filter(RequestLog::fields().created_at().lt(cutoff))
        .delete()
        .exec(tx)
        .await?;
    LogFilterOption::filter(LogFilterOption::fields().last_seen().lt(cutoff))
        .delete()
        .exec(tx)
        .await?;
    Ok(())
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
