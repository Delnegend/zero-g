//! The Codex client version Zero-G's own ChatGPT backend calls claim to be.
//!
//! The backend decides which models to list from the client version, so a stale version hides new
//! models. Zero-G's clients are not Codex CLIs, so it cannot learn the version from their
//! requests. It asks the npm registry for the current stable release once a day instead. The
//! pinned value is the floor, used until that answer arrives and whenever the registry is
//! unreachable.

use crate::provider::common::version::{self, Newest};

/// Release used until the registry answers. Raise it now and then; it is only a floor.
const FLOOR: &str = "0.172.0";
/// Registry entry for the stable release. Pre-release builds have their own tags.
const LATEST_URL: &str = "https://registry.npmjs.org/@openai/codex/latest";

/// Newest stable release the registry has reported.
static LATEST: Newest = Newest::new();

/// Version to send as the Codex client version.
pub(crate) fn current() -> String {
    // The floor is a literal in this file, so it always parses.
    let floor = version::parse(FLOOR).unwrap_or_default();
    version::render(LATEST.current(floor))
}

/// Check the registry once a day for as long as the server runs.
pub(crate) async fn run_background() {
    version::check_daily("Codex", LATEST_URL, &LATEST).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Without an answer the floor is used, and it has the plain form the backend expects.
    #[test]
    fn sends_a_plain_version_by_default() {
        let current = current();
        assert!(version::parse(&current).is_some(), "{current}");
        assert!(
            version::parse(&current) >= version::parse(FLOOR),
            "{current}"
        );
    }
}
