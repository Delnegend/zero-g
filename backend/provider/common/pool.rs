//! Shared credential-pool selection, cooldown filtering, and per-account catalog caching.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::openai::Model;
use crate::provider::common::cooldown;
use crate::provider::common::errors::{UpstreamError, retryable_status};
use crate::provider::common::request_identity::RequestIdentity;
use crate::provider::credentials::Credential;
use crate::provider::login::CredentialError;
use crate::provider::registry::Provider;
use crate::server::settings::SettingsStore;

/// Default time-to-live for cached per-account model catalogs.
pub(crate) const DEFAULT_CATALOG_TTL: Duration = Duration::from_secs(300);

/// Failure while selecting credentials or contacting an upstream provider.
#[derive(Debug)]
pub(crate) enum PoolError {
    /// A local configuration or credential failure.
    Internal(String),
    /// An upstream request failure.
    Upstream(UpstreamError),
    /// Every eligible credential is temporarily rate limited.
    Cooling(Duration),
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal(message) => f.write_str(message),
            Self::Upstream(error) => write!(f, "{error}"),
            Self::Cooling(_) => f.write_str("All accounts for this provider are rate limited"),
        }
    }
}

impl PoolError {
    /// Converts a credential lookup or refresh error into a pool failure.
    pub(crate) fn from_credential(error: CredentialError) -> Self {
        match error.upstream_status {
            status if retryable_status(status) => Self::Upstream(UpstreamError {
                status,
                message: error.message,
                headers: Box::default(),
            }),
            _ => Self::Internal(error.message),
        }
    }

    /// Returns whether another credential may be tried.
    pub(crate) fn retryable(&self) -> bool {
        matches!(self, Self::Upstream(error) if error.retryable())
    }

    /// Converts the failure into a gateway HTTP response.
    ///
    /// When `preserve_upstream_status` is true, non-429 upstream statuses are forwarded;
    /// otherwise they map to `502 Bad Gateway`.
    pub(crate) fn into_response(
        self,
        rate_limited_message: &str,
        preserve_upstream_status: bool,
    ) -> Response {
        let (status, kind, code, message, headers) = match self {
            Self::Cooling(wait) => {
                let mut response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(crate::server::error_body(
                        rate_limited_message,
                        "rate_limit_error",
                        Some("rate_limited"),
                    )),
                )
                    .into_response();
                if let Ok(value) = cooldown::retry_after_secs(wait).to_string().parse() {
                    response
                        .headers_mut()
                        .insert(axum::http::header::RETRY_AFTER, value);
                }
                return response;
            }
            Self::Internal(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "internal_error",
                message,
                None,
            ),
            Self::Upstream(error) => {
                let limited = error.status == Some(StatusCode::TOO_MANY_REQUESTS);
                let status = if limited {
                    StatusCode::TOO_MANY_REQUESTS
                } else if preserve_upstream_status {
                    error.status.unwrap_or(StatusCode::BAD_GATEWAY)
                } else {
                    StatusCode::BAD_GATEWAY
                };
                (
                    status,
                    if limited {
                        "rate_limit_error"
                    } else {
                        "upstream_error"
                    },
                    if limited {
                        "rate_limited"
                    } else {
                        "bad_gateway"
                    },
                    error.message,
                    Some(error.headers),
                )
            }
        };
        let mut response = (
            status,
            Json(crate::server::error_body(&message, kind, Some(code))),
        )
            .into_response();
        if let Some(headers) = headers {
            cooldown::copy_rate_limit_headers(&headers, response.headers_mut());
        }
        response
    }
}

/// Model catalog advertised by a single provider account.
pub(crate) trait AccountCatalog {
    /// Returns whether this account's catalog advertises the given wire model ID.
    fn has_model(&self, model: &str) -> bool;
}

impl AccountCatalog for Vec<String> {
    fn has_model(&self, model: &str) -> bool {
        self.iter().any(|id| id == model)
    }
}

impl AccountCatalog for Vec<Model> {
    fn has_model(&self, model: &str) -> bool {
        self.iter().any(|entry| entry.id == model)
    }
}

impl<T> AccountCatalog for Vec<(String, T)> {
    fn has_model(&self, model: &str) -> bool {
        self.iter().any(|(id, _)| id == model)
    }
}

/// Cached catalog snapshot for a single credential.
struct CachedCatalog<T> {
    /// Digest of the credential scope (token and optional project/base URL).
    scope: [u8; 32],
    /// Instant at which discovery succeeded.
    fetched: Instant,
    /// Discovered catalog value.
    value: T,
}

/// Per-credential in-memory cache for discovered account catalogs.
pub(crate) struct CatalogCache<T> {
    /// Cached entries keyed by credential ID.
    entries: Mutex<HashMap<String, CachedCatalog<T>>>,
    /// Maximum age of a cached entry.
    ttl: Duration,
}

impl<T> Default for CatalogCache<T> {
    fn default() -> Self {
        Self::new(DEFAULT_CATALOG_TTL)
    }
}

impl<T> CatalogCache<T> {
    /// Creates an empty cache with the given entry lifetime.
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Computes a stable digest for the credential scope without retaining secrets.
    pub(crate) fn scope_hash(parts: &[&str]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part.len().to_le_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.finalize().into()
    }

    /// Drops the cached catalog for a credential.
    pub(crate) async fn invalidate(&self, credential_id: &str) {
        self.entries.lock().await.remove(credential_id);
    }

    /// Drops the cached catalog when an upstream response rejects the credential.
    pub(crate) async fn invalidate_on_auth_error(
        &self,
        credential_id: &str,
        status: Option<StatusCode>,
    ) {
        if matches!(
            status,
            Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        ) {
            self.invalidate(credential_id).await;
        }
    }
}

impl<T: Clone> CatalogCache<T> {
    /// Returns a fresh cached catalog matching `credential_id` and `scope`, if present.
    pub(crate) async fn get(&self, credential_id: &str, scope: [u8; 32]) -> Option<T> {
        let mut entries = self.entries.lock().await;
        let fresh = entries
            .get(credential_id)
            .is_some_and(|entry| entry.scope == scope && entry.fetched.elapsed() < self.ttl);
        if fresh {
            return entries.get(credential_id).map(|entry| entry.value.clone());
        }
        entries.remove(credential_id);
        None
    }

    /// Stores a discovered catalog for `credential_id` and `scope`.
    pub(crate) async fn insert(&self, credential_id: String, scope: [u8; 32], value: T) {
        self.entries.lock().await.insert(
            credential_id,
            CachedCatalog {
                scope,
                fetched: Instant::now(),
                value,
            },
        );
    }
}

/// Loads weighted credentials, excluding those currently in cooldown.
pub(crate) async fn eligible(
    settings: &SettingsStore,
    provider: &Provider,
) -> Result<Vec<Credential>, PoolError> {
    eligible_by(settings, provider, |credential| {
        Cow::Borrowed(credential.id.as_str())
    })
    .await
}

/// Loads weighted credentials, excluding those whose cooldown key is active.
pub(crate) async fn eligible_by<F>(
    settings: &SettingsStore,
    provider: &Provider,
    cooldown_key: F,
) -> Result<Vec<Credential>, PoolError>
where
    F: Fn(&Credential) -> Cow<'_, str>,
{
    let credentials = crate::provider::credentials::list(settings, provider)
        .await
        .map_err(PoolError::Internal)?;
    filter_cooldowns(credentials, cooldown_key)
}

/// Filters out credentials currently in cooldown, returning `PoolError::Cooling` if all are cooling.
pub(crate) fn filter_cooldowns<F>(
    mut credentials: Vec<Credential>,
    cooldown_key: F,
) -> Result<Vec<Credential>, PoolError>
where
    F: Fn(&Credential) -> Cow<'_, str>,
{
    let mut earliest = None;
    credentials.retain(|credential| {
        let key = cooldown_key(credential);
        cooldown::remaining(&key).is_none_or(|wait| {
            earliest = Some(earliest.map_or(wait, |old: Duration| old.min(wait)));
            false
        })
    });
    if credentials.is_empty()
        && let Some(wait) = earliest
    {
        return Err(PoolError::Cooling(wait));
    }
    Ok(credentials)
}

/// Ranks accounts for `model`: advertising accounts first, then accounts whose lookup failed.
///
/// Accounts that clearly lack `model` are excluded when at least one account advertises it.
/// When no account advertises `model`, all accounts remain eligible in their original order so
/// unlisted models remain directly reachable.
pub(crate) fn rank_accounts<C, Cat, E>(
    candidates: Vec<(C, Result<Cat, E>)>,
    model: &str,
) -> Vec<C>
where
    Cat: AccountCatalog,
{
    let has_match = candidates
        .iter()
        .any(|(_, catalog)| catalog.as_ref().is_ok_and(|cat| cat.has_model(model)));
    if !has_match {
        return candidates
            .into_iter()
            .map(|(candidate, _)| candidate)
            .collect();
    }
    let mut advertising = Vec::new();
    let mut failed = Vec::new();
    for (candidate, catalog) in candidates {
        match catalog {
            Ok(cat) if cat.has_model(model) => advertising.push(candidate),
            Ok(_) => {}
            Err(_) => failed.push(candidate),
        }
    }
    advertising.extend(failed);
    advertising
}

/// Tries candidates in order, advancing on retryable `PoolError` failures.
pub(crate) async fn try_candidates<C, T, F, Fut>(
    candidates: Vec<C>,
    empty_message: &'static str,
    mut attempt: F,
) -> Result<T, PoolError>
where
    F: FnMut(C) -> Fut,
    Fut: Future<Output = Result<T, PoolError>>,
{
    let mut candidates = candidates.into_iter().peekable();
    while let Some(candidate) = candidates.next() {
        match attempt(candidate).await {
            Err(error) if error.retryable() && candidates.peek().is_some() => {}
            result => return result,
        }
    }
    Err(PoolError::Internal(empty_message.to_owned()))
}

/// Runs `operation` across the given `credentials`, handling cooldowns and identity tracking.
pub(crate) async fn run_candidates<Creds, T, L, LFut, Op, OpFut>(
    credentials: Vec<Credential>,
    identity: Option<&RequestIdentity>,
    empty_message: &'static str,
    load_creds: L,
    operation: Op,
) -> Result<T, PoolError>
where
    L: Fn(&Credential) -> LFut + Send + Sync,
    LFut: Future<Output = Result<Option<Creds>, PoolError>> + Send,
    Op: Fn(Creds) -> OpFut + Send + Sync,
    OpFut: Future<Output = Result<T, UpstreamError>> + Send,
{
    try_candidates(credentials, empty_message, |credential| {
        let load_creds = &load_creds;
        let operation = &operation;
        async move {
            let creds = load_creds(&credential)
                .await
                .map_err(|error| {
                    if let PoolError::Upstream(upstream) = &error
                        && upstream.status == Some(StatusCode::TOO_MANY_REQUESTS)
                    {
                        cooldown::start(
                            &credential.id,
                            cooldown::wait_for(&reqwest::header::HeaderMap::new()),
                        );
                    }
                    error
                })?
                .ok_or_else(|| PoolError::Internal(empty_message.to_owned()))?;
            let result = operation(creds).await.map_err(|error| {
                if error.status == Some(StatusCode::TOO_MANY_REQUESTS) {
                    cooldown::start(&credential.id, cooldown::wait_for(&error.headers));
                }
                PoolError::Upstream(error)
            })?;
            cooldown::clear(&credential.id);
            if let Some(identity) = identity {
                identity.set_credential(&credential.id, &credential.name);
            }
            Ok(result)
        }
    })
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rank_accounts_puts_advertisers_first_then_failures_and_excludes_non_advertisers() {
        let candidates = vec![
            ("no-match", Ok::<Vec<String>, ()>(vec!["other".into()])),
            ("failed-1", Err(())),
            ("match-1", Ok(vec!["target".into()])),
            ("failed-2", Err(())),
            ("match-2", Ok(vec!["other".into(), "target".into()])),
        ];
        assert_eq!(
            rank_accounts(candidates, "target"),
            vec!["match-1", "match-2", "failed-1", "failed-2"]
        );
    }

    #[test]
    fn rank_accounts_keeps_all_in_order_when_none_advertise() {
        let candidates = vec![
            ("acc-1", Ok::<Vec<String>, ()>(vec!["other".into()])),
            ("acc-2", Err(())),
            ("acc-3", Ok(vec!["another".into()])),
        ];
        assert_eq!(
            rank_accounts(candidates, "undiscovered"),
            vec!["acc-1", "acc-2", "acc-3"]
        );
    }

    #[tokio::test]
    async fn catalog_cache_scopes_by_credential_and_invalidates_on_auth_error() {
        let cache: CatalogCache<Vec<String>> = CatalogCache::new(Duration::from_secs(60));
        let scope_a = CatalogCache::<Vec<String>>::scope_hash(&["tok-a", "proj-1"]);
        let scope_b = CatalogCache::<Vec<String>>::scope_hash(&["tok-b", "proj-1"]);

        cache
            .insert("cred-1".into(), scope_a, vec!["m1".into()])
            .await;
        cache
            .insert("cred-2".into(), scope_b, vec!["m2".into()])
            .await;

        assert_eq!(cache.get("cred-1", scope_a).await, Some(vec!["m1".into()]));
        assert_eq!(cache.get("cred-2", scope_b).await, Some(vec!["m2".into()]));
        cache
            .invalidate_on_auth_error("cred-1", Some(StatusCode::BAD_REQUEST))
            .await;
        assert_eq!(cache.get("cred-1", scope_a).await, Some(vec!["m1".into()]));
        assert_eq!(cache.get("cred-1", scope_b).await, None);
        cache
            .insert("cred-1".into(), scope_a, vec!["m1".into()])
            .await;

        cache
            .invalidate_on_auth_error("cred-1", Some(StatusCode::UNAUTHORIZED))
            .await;
        assert_eq!(cache.get("cred-1", scope_a).await, None);
        assert_eq!(cache.get("cred-2", scope_b).await, Some(vec!["m2".into()]));
    }
}
