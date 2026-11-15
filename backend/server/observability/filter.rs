//! Filters shared by the dashboard and log list, and the filter choices they offer.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, de::DeserializeOwned};
use toasty::stmt::Expr;

use super::now;
use crate::storage::{FilterKind, LogFilterOption, RequestLog};

/// Response class selected by the `statuses` filter.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum StatusClass {
    /// Status 200 through 399.
    Success,
    /// Status 400 and above.
    Error,
}

/// Optional inclusive time bounds and exact-match dimension filters.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Filter {
    /// Inclusive lower timestamp.
    pub(super) since: Option<i64>,
    /// Inclusive upper timestamp.
    pub(super) until: Option<i64>,
    /// Exact providers encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    pub(super) providers: Vec<String>,
    /// Exact models encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    pub(super) models: Vec<String>,
    /// Exact request paths encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    pub(super) paths: Vec<String>,
    /// Response classes encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    pub(super) statuses: Vec<StatusClass>,
    /// Exact virtual key IDs encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    pub(super) virtual_key_ids: Vec<String>,
    /// Exact provider credential IDs encoded as a JSON array.
    #[serde(default, deserialize_with = "json_array")]
    pub(super) credential_ids: Vec<String>,
    /// Whether to compute filter options; only the dashboard reads it.
    pub(super) include_options: Option<bool>,
}

/// Decode a JSON array from one URL query value.
pub(super) fn json_array<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = String::deserialize(deserializer)?;
    serde_json::from_str(&value).map_err(serde::de::Error::custom)
}

/// Inclusive time bounds resolved once per request.
#[derive(Clone, Copy)]
pub(super) struct Range {
    /// Inclusive lower timestamp.
    pub(super) since: Option<i64>,
    /// Inclusive upper timestamp; an open end after `since` means now.
    pub(super) upper: Option<i64>,
}

impl Range {
    /// Restrict rows to the time range.
    fn condition(self) -> Expr<bool> {
        let fields = RequestLog::fields();
        Expr::and_all(
            [
                self.since.map(|since| fields.created_at().ge(since)),
                self.upper.map(|upper| fields.created_at().le(upper)),
            ]
            .into_iter()
            .flatten(),
        )
    }
}

impl Filter {
    /// Resolve the time bounds, closing a lower-bounded range at the current time.
    pub(super) fn range(&self) -> Range {
        Range {
            since: self.since,
            upper: self.until.or_else(|| self.since.map(|_| now())),
        }
    }

    /// Restrict rows to the time range and every selected dimension.
    pub(super) fn condition(&self, range: Range) -> Expr<bool> {
        let fields = RequestLog::fields();
        let optional = |ids: &[String]| ids.iter().cloned().map(Some).collect::<Vec<_>>();
        let success = self.statuses.contains(&StatusClass::Success);
        let error = self.statuses.contains(&StatusClass::Error);
        let status = match (success, error) {
            (false, false) => None,
            (true, true) => Some(fields.status().ge(200)),
            (true, false) => Some(fields.status().ge(200).and(fields.status().lt(400))),
            (false, true) => Some(fields.status().ge(400)),
        };
        Expr::and_all(
            [
                Some(range.condition()),
                (!self.providers.is_empty())
                    .then(|| fields.provider().in_list(self.providers.clone())),
                (!self.models.is_empty()).then(|| fields.model().in_list(self.models.clone())),
                (!self.paths.is_empty()).then(|| fields.path().in_list(self.paths.clone())),
                (!self.virtual_key_ids.is_empty()).then(|| {
                    fields
                        .virtual_key_id()
                        .in_list(optional(&self.virtual_key_ids))
                }),
                (!self.credential_ids.is_empty()).then(|| {
                    fields
                        .credential_id()
                        .in_list(optional(&self.credential_ids))
                }),
                status,
            ]
            .into_iter()
            .flatten(),
        )
    }
}

/// Available dimensions before applying dimension filters.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct FilterOptions {
    /// Available providers.
    providers: Vec<String>,
    /// Available models.
    models: Vec<String>,
    /// Available paths.
    paths: Vec<String>,
    /// Virtual keys observed within the time range.
    virtual_keys: Vec<IdentityOption>,
    /// Provider credentials observed within the time range.
    credentials: Vec<IdentityOption>,
}

/// A stable identity and its name snapshot.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct IdentityOption {
    /// Stable identifier.
    id: String,
    /// Name observed in a request.
    name: String,
}

/// Collects options, keeping the lexically first name per identity so renames stay stable.
#[derive(Default)]
struct OptionSet {
    /// Providers seen.
    providers: BTreeSet<String>,
    /// Models seen.
    models: BTreeSet<String>,
    /// Paths seen.
    paths: BTreeSet<String>,
    /// Virtual key names by ID.
    virtual_keys: BTreeMap<String, String>,
    /// Credential names by ID.
    credentials: BTreeMap<String, String>,
}

impl OptionSet {
    /// Record a nonempty dimension value.
    fn value(set: &mut BTreeSet<String>, value: String) {
        if !value.is_empty() {
            set.insert(value);
        }
    }

    /// Record an identity, keeping the lexically first name.
    fn identity(map: &mut BTreeMap<String, String>, id: Option<String>, name: Option<String>) {
        let (Some(id), Some(name)) = (id, name) else {
            return;
        };
        map.entry(id)
            .and_modify(|old| {
                if name < *old {
                    old.clone_from(&name);
                }
            })
            .or_insert(name);
    }

    /// Produce sorted options.
    fn finish(self) -> FilterOptions {
        let identities = |map: BTreeMap<String, String>| {
            map.into_iter()
                .map(|(id, name)| IdentityOption { id, name })
                .collect()
        };
        FilterOptions {
            providers: self.providers.into_iter().collect(),
            models: self.models.into_iter().collect(),
            paths: self.paths.into_iter().collect(),
            virtual_keys: identities(self.virtual_keys),
            credentials: identities(self.credentials),
        }
    }
}

/// Filter choices for a time range, independent of dimension filters.
///
/// Each snapshot row's `last_seen` is the newest retained log carrying it, and
/// prune drops logs and snapshots at the same cutoff. So when no log is newer
/// than the range end, an option occurs in the range exactly when
/// `last_seen >= since`, and the snapshots replace a scan of the logs.
pub(super) async fn options(
    tx: &mut toasty::Transaction<'_>,
    range: Range,
) -> toasty::Result<FilterOptions> {
    let exact = match range.upper {
        Some(upper) => RequestLog::filter(RequestLog::fields().created_at().gt(upper))
            .first()
            .exec(&mut *tx)
            .await?
            .is_none(),
        None => true,
    };
    if exact {
        snapshot_options(tx, range.since).await
    } else {
        scanned_options(tx, range).await
    }
}

/// Options whose newest retained log is at or after `since`.
pub(super) async fn snapshot_options(
    tx: &mut toasty::Transaction<'_>,
    since: Option<i64>,
) -> toasty::Result<FilterOptions> {
    let fields = LogFilterOption::fields();
    let query = since.map_or_else(LogFilterOption::all, |since| {
        LogFilterOption::filter(fields.last_seen().ge(since))
    });
    let rows = query
        .select((fields.kind(), fields.value(), fields.name()))
        .exec(tx)
        .await?;
    let mut set = OptionSet::default();
    for (kind, value, name) in rows {
        match kind {
            FilterKind::Provider => OptionSet::value(&mut set.providers, value),
            FilterKind::Model => OptionSet::value(&mut set.models, value),
            FilterKind::RequestPath => OptionSet::value(&mut set.paths, value),
            FilterKind::VirtualKey => {
                OptionSet::identity(&mut set.virtual_keys, Some(value), Some(name));
            }
            FilterKind::Credential => {
                OptionSet::identity(&mut set.credentials, Some(value), Some(name));
            }
        }
    }
    Ok(set.finish())
}

/// Options read from every log in the range.
pub(super) async fn scanned_options(
    tx: &mut toasty::Transaction<'_>,
    range: Range,
) -> toasty::Result<FilterOptions> {
    let fields = RequestLog::fields();
    let rows = RequestLog::filter(range.condition())
        .select((
            fields.provider(),
            fields.model(),
            fields.path(),
            fields.virtual_key_id(),
            fields.virtual_key_name(),
            fields.credential_id(),
            fields.credential_name(),
        ))
        .exec(tx)
        .await?;
    let mut set = OptionSet::default();
    for (provider, model, path, key_id, key_name, credential_id, credential_name) in rows {
        OptionSet::value(&mut set.providers, provider);
        OptionSet::value(&mut set.models, model);
        OptionSet::value(&mut set.paths, path);
        OptionSet::identity(&mut set.virtual_keys, key_id, key_name);
        OptionSet::identity(&mut set.credentials, credential_id, credential_name);
    }
    Ok(set.finish())
}
