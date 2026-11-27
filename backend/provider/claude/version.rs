//! The Claude Code version Zero-G's own Anthropic calls claim to be.
//!
//! Anthropic's usage, profile and reset endpoints answer only recent Claude Code versions. Chat
//! requests are not affected, because the pass-through forwards each client's own user agent. A
//! version pinned by hand goes stale, so Zero-G asks the npm registry for the current release at
//! startup and then once a day. The pinned value is the floor, used until the registry answers and
//! whenever it is unreachable.

use crate::provider::common::version::{self, Newest};

/// Release used until the registry answers. Raise it now and then; it is only a floor.
const FLOOR: &str = "2.1.332";
/// Prefix of the user agent Claude Code sends.
const PREFIX: &str = "claude-cli/";

/// Registry entry for the current release.
const LATEST_URL: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code/latest";

/// Newest release the registry has reported.
static LATEST: Newest = Newest::new();

/// Check the registry once a day for as long as the server runs.
pub(crate) async fn run_background() {
    version::check_daily("Claude Code", LATEST_URL, &LATEST).await;
}

/// User agent for Zero-G's own calls to Anthropic.
pub(super) fn user_agent() -> String {
    // The floor is a literal in this file, so it always parses.
    let floor = version::parse(FLOOR).unwrap_or_default();
    format!(
        "{PREFIX}{} (external, cli)",
        version::render(LATEST.current(floor))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero-G's own user agent has the form the usage endpoint expects.
    #[test]
    fn own_user_agent_has_the_claude_code_form() {
        let agent = user_agent();
        assert!(agent.starts_with("claude-cli/2."), "{agent}");
        assert!(agent.ends_with(" (external, cli)"), "{agent}");
    }
}
