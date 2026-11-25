//! Settings from the environment and an optional `KEY=value` file.
//!
//! Environment variables win over the file. The file exists so a background service or an agy
//! hook, which has no shell profile, can still be configured: it defaults to
//! `~/.config/zero-g/agy-integration.env`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Hours between reads of the subscription tier unless configured otherwise.
const DEFAULT_TIER_REFRESH_HOURS: f64 = 24.0;

/// Everything one run needs.
pub(crate) struct Settings {
    /// Gateway root, without `/v1`.
    pub(crate) url: String,
    /// Virtual key without a provider allowlist.
    pub(crate) key: String,
    /// Name of the Google Antigravity credential in Zero-G for this login (`quota` only).
    pub(crate) account: Option<String>,
    /// The agy executable.
    pub(crate) agy: String,
    /// How long a cached subscription tier stays valid.
    pub(crate) tier_refresh: Duration,
    /// Where the tier cache lives.
    pub(crate) cache_dir: PathBuf,
}

/// Parse `KEY=value` lines. Blank lines and `#` comments are skipped, an `export ` prefix and one
/// pair of surrounding quotes are dropped.
pub(crate) fn parse_file(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, value) = line.split_once('=')?;
            let value = value.trim();
            let unquoted = ['"', '\'']
                .iter()
                .find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote))
                .unwrap_or(value);
            Some((name.trim().to_owned(), unquoted.to_owned()))
        })
        .collect()
}

/// The user's home directory.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// An absolute directory from an environment variable, or `fallback` under the home directory.
fn base_dir(variable: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(fallback)))
}

/// Where the settings file is looked for when none is named.
pub(crate) fn default_config_path() -> Option<PathBuf> {
    base_dir("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("zero-g").join("agy-integration.env"))
}

/// Read the settings file: required when named, optional at the default path.
fn read_file(explicit: Option<&Path>) -> Result<HashMap<String, String>, String> {
    let named = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("ZERO_G_AGY_INTEGRATION_CONFIG").map(PathBuf::from));
    if let Some(path) = named {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        return Ok(parse_file(&text));
    }
    Ok(default_config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map_or_default(|text| parse_file(&text)))
}

/// Resolve every setting. The gateway URL and key are required; the account is only checked by
/// the commands that need it.
pub(crate) fn load(explicit: Option<&Path>) -> Result<Settings, String> {
    let file = read_file(explicit)?;
    let get = |name: &str| {
        std::env::var(name)
            .ok()
            .or_else(|| file.get(name).cloned())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let (Some(url), Some(key)) = (get("ZERO_G_URL"), get("ZERO_G_VIRTUAL_KEY")) else {
        return Err("set ZERO_G_URL and ZERO_G_VIRTUAL_KEY (environment or settings file)".into());
    };
    let hours = get("AGY_TIER_REFRESH_HOURS")
        .and_then(|hours| hours.parse::<f64>().ok())
        .filter(|hours| hours.is_finite() && *hours >= 0.0)
        .unwrap_or(DEFAULT_TIER_REFRESH_HOURS);
    let cache_dir = get("ZERO_G_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| base_dir("XDG_CACHE_HOME", ".cache").map(|dir| dir.join("zero-g")))
        .unwrap_or_else(|| std::env::temp_dir().join("zero-g"));
    Ok(Settings {
        url: url.trim_end_matches('/').to_owned(),
        key,
        account: get("ZERO_G_AGY_ACCOUNT"),
        agy: get("AGY_BIN").unwrap_or_else(|| "agy".to_owned()),
        tier_refresh: Duration::try_from_secs_f64(hours * 3600.0).unwrap_or(Duration::MAX),
        cache_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_file;

    #[test]
    fn parses_comments_quotes_and_export_prefixes() {
        let parsed = parse_file(
            "# comment\n\nZERO_G_URL=http://127.0.0.1:8080\nexport ZERO_G_AGY_ACCOUNT='My Account'\nZERO_G_VIRTUAL_KEY = \"zg-key\"\nnot a setting\n",
        );
        assert_eq!(parsed["ZERO_G_URL"], "http://127.0.0.1:8080");
        assert_eq!(parsed["ZERO_G_AGY_ACCOUNT"], "My Account");
        assert_eq!(parsed["ZERO_G_VIRTUAL_KEY"], "zg-key");
        assert_eq!(parsed.len(), 3);
    }
}
