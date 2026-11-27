//! Dotted client versions that Zero-G's own upstream calls claim to be.
//!
//! Some upstream endpoints answer only recent client versions, so a version pinned by hand goes
//! stale. Each provider keeps a pinned floor and raises it from what it learns at run time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Bits for each of major, minor and patch when a version is packed into one integer.
const PART_BITS: u32 = 21;

/// Time between registry checks.
const CHECK_EVERY: Duration = Duration::from_hours(24);

/// The newest version learned at run time; zero until one is.
pub(crate) struct Newest(AtomicU64);

impl Newest {
    /// A holder that has learned nothing yet.
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Remember a version, keeping the newer of it and what is already held.
    pub(crate) fn record(&self, packed: u64) {
        self.0.fetch_max(packed, Ordering::Relaxed);
    }

    /// The newest version learned, but never older than `floor`.
    pub(crate) fn current(&self, floor: u64) -> u64 {
        self.0.load(Ordering::Relaxed).max(floor)
    }
}

/// Pack `major.minor.patch` so that integer order is version order.
fn pack(major: u64, minor: u64, patch: u64) -> Option<u64> {
    let limit = 1_u64 << PART_BITS;
    (major < limit && minor < limit && patch < limit)
        .then_some((major << (2 * PART_BITS)) | (minor << PART_BITS) | patch)
}

/// Pack a plain `x.y.z` version. Anything else, including a pre-release suffix, a missing or
/// extra part, a sign, a space or an absurd number, is `None`.
pub(crate) fn parse(text: &str) -> Option<u64> {
    let mut parts = text.split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
    });
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(())?;
    pack(major, minor, patch)
}

/// Text form of a packed version.
pub(crate) fn render(packed: u64) -> String {
    let mask = (1_u64 << PART_BITS) - 1;
    format!(
        "{}.{}.{}",
        packed >> (2 * PART_BITS),
        (packed >> PART_BITS) & mask,
        packed & mask
    )
}

/// Ask the npm registry entry at `url` for a release. It is an error unless the answer names a
/// plain `x.y.z`, so a pre-release or a surprise never becomes the version.
async fn fetch_latest(client: &reqwest::Client, url: &str) -> Result<u64, String> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| "registry request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!("registry answered {}", response.status().as_u16()));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "registry answer was not JSON".to_string())?;
    body.get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(parse)
        .ok_or_else(|| "registry named no stable version".to_string())
}

/// Check the registry entry at `url` once a day for as long as the server runs, keeping the
/// newest release in `newest`. A failed check changes nothing, so offline installs keep working
/// on their pinned version.
pub(crate) async fn check_daily(label: &'static str, url: &'static str, newest: &'static Newest) {
    loop {
        match fetch_latest(&crate::util::HTTP_REDIRECT_CLIENT, url).await {
            Ok(latest) => {
                newest.record(latest);
                tracing::info!(version = %render(newest.current(latest)), "{label} client version checked");
            }
            Err(message) => tracing::warn!("{label} client version check failed: {message}"),
        }
        tokio::time::sleep(CHECK_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;

    /// Registry stand-in answering each path with a fixed reply.
    async fn serve() -> String {
        let app = Router::new()
            .route(
                "/stable",
                get(|| async { Json(json!({"name": "pkg", "version": "0.180.2"})) }),
            )
            .route(
                "/alpha",
                get(|| async { Json(json!({"version": "0.181.0-alpha.3"})) }),
            )
            .route("/none", get(|| async { Json(json!({"name": "pkg"})) }))
            .route("/text", get(|| async { "not json" }))
            .route("/down", get(|| async { StatusCode::BAD_GATEWAY }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    /// A plain release is taken; pre-releases, missing versions and failures are errors, never a guess.
    #[tokio::test]
    async fn reads_only_a_stable_release_from_the_registry() {
        let base = serve().await;
        let client = reqwest::Client::new();
        let latest = fetch_latest(&client, &format!("{base}/stable"))
            .await
            .unwrap();
        assert_eq!(render(latest), "0.180.2");
        for path in ["alpha", "none", "text", "down"] {
            assert!(
                fetch_latest(&client, &format!("{base}/{path}"))
                    .await
                    .is_err(),
                "{path}"
            );
        }
    }

    /// Only a plain `x.y.z` counts, and the text form round-trips.
    #[test]
    fn parses_plain_versions_only() {
        for good in ["2.1.332", "0.172.0", "10.0.0"] {
            assert_eq!(parse(good).map(render).as_deref(), Some(good));
        }
        for bad in [
            "",
            "2.1",
            "2.1.x",
            "2.1.332-beta.1",
            "0.174.0-alpha.8",
            "2.1.3.4",
            " 2.1.3",
            "2.1.3 ",
            "-1.0.0",
            "+1.0.0",
            "99999999.0.0",
            "1..3",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    /// Versions order by number, not by text, and the held value never drops below the floor.
    #[test]
    fn keeps_the_newest_version_above_the_floor() {
        let floor = parse("2.1.332").unwrap();
        let newest = Newest::new();
        assert_eq!(render(newest.current(floor)), "2.1.332");
        newest.record(parse("2.1.9").unwrap());
        assert_eq!(render(newest.current(floor)), "2.1.332");
        newest.record(parse("2.1.340").unwrap());
        assert_eq!(render(newest.current(floor)), "2.1.340");
        newest.record(parse("2.1.1000").unwrap());
        newest.record(parse("2.1.340").unwrap());
        assert_eq!(render(newest.current(floor)), "2.1.1000");
        newest.record(parse("3.0.0").unwrap());
        assert_eq!(render(newest.current(floor)), "3.0.0");
    }
}
