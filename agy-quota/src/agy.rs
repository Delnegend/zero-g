//! Reads the quota and the subscription tier from the official `agy` CLI.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use crate::config::Settings;

/// Longest the `/usage` command may run.
const USAGE_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest one ACP request may wait for its reply.
const ACP_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest the ACP server may take to exit after its input closes.
const ACP_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `agy --print /usage --output-format json` and return the `command.data` report.
pub(crate) async fn usage(agy: &str) -> Result<Value, String> {
    let output = timeout(
        USAGE_TIMEOUT,
        Command::new(agy)
            .args(["--print", "/usage", "--output-format", "json"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "agy /usage timed out".to_owned())?
    .map_err(|error| format!("cannot run {agy}: {error}"))?;
    if !output.status.success() {
        return Err(format!("agy exited with {}", output.status));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "agy printed something that is not JSON".to_owned())?;
    let command = value.get("command");
    let is_usage = command
        .and_then(|command| command.get("name"))
        .and_then(Value::as_str)
        == Some("usage");
    command
        .and_then(|command| command.get("data"))
        .filter(|data| is_usage && data.get("groups").is_some_and(Value::is_array))
        .cloned()
        .ok_or_else(|| "unexpected /usage output".to_owned())
}

/// A running `agy acp` server, spoken to over JSON-RPC lines.
struct Acp {
    /// Pipe to the server's input.
    stdin: ChildStdin,
    /// Lines from the server's output.
    lines: Lines<BufReader<ChildStdout>>,
    /// Id of the last request sent.
    last_id: u64,
}

impl Acp {
    /// Send one request and wait for the reply with the same id.
    async fn call(&mut self, method: &str, params: Value) -> Option<Value> {
        self.last_id = self.last_id.saturating_add(1);
        let id = self.last_id;
        let mut line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.ok()?;
        self.stdin.flush().await.ok()?;
        timeout(ACP_CALL_TIMEOUT, async {
            loop {
                let text = self.lines.next_line().await.ok()??;
                if let Ok(message) = serde_json::from_str::<Value>(&text)
                    && message.get("id") == Some(&json!(id))
                {
                    return message.get("result").cloned();
                }
            }
        })
        .await
        .ok()?
    }
}

/// Open a throwaway ACP session and read the account tier from its reply.
async fn read_tier(agy: &str) -> Option<Value> {
    let mut child: Child = Command::new(agy)
        .arg("acp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let mut acp = Acp {
        stdin,
        lines: BufReader::new(stdout).lines(),
        last_id: 0,
    };
    let tier = exchange(&mut acp).await;
    drop(acp);
    if timeout(ACP_EXIT_TIMEOUT, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    tier
}

/// The ACP conversation: initialize, open a session, read its tier, close it.
async fn exchange(acp: &mut Acp) -> Option<Value> {
    acp.call(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    )
    .await?;
    let session = acp
        .call(
            "session/new",
            json!({"cwd": std::env::temp_dir(), "mcpServers": []}),
        )
        .await?;
    if let Some(id) = session.get("sessionId") {
        acp.call("session/close", json!({"sessionId": id})).await;
    }
    let tier = session.pointer("/_meta/antigravity/tier")?;
    let text = |field: &str| {
        tier.get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    Some(json!({"name": text("name")?, "id": text("id")?}))
}

/// Seconds since the Unix epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Where this account's tier is cached.
fn cache_path(dir: &Path, account: &str) -> PathBuf {
    let safe: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("agy-tier-{safe}.json"))
}

/// The account tier, read from agy at most once per refresh interval.
///
/// Each read leaves a small throwaway conversation file in agy's own storage, so the result is
/// cached and the cached value is sent with every push. A failed read falls back to the cache.
pub(crate) async fn cached_tier(settings: &Settings) -> Option<Value> {
    let path = cache_path(&settings.cache_dir, &settings.account);
    let cached: Option<Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let known = cached
        .as_ref()
        .and_then(|cached| cached.get("tier"))
        .cloned();
    let fresh = cached
        .as_ref()
        .and_then(|cached| cached.get("checked_at"))
        .and_then(Value::as_u64)
        .is_some_and(|checked| {
            Duration::from_secs(now_secs().saturating_sub(checked)) < settings.tier_refresh
        });
    if fresh && known.is_some() {
        return known;
    }
    let Some(tier) = read_tier(&settings.agy).await else {
        return known;
    };
    if std::fs::create_dir_all(&settings.cache_dir).is_ok() {
        let _ = std::fs::write(
            &path,
            json!({"checked_at": now_secs(), "tier": tier}).to_string(),
        );
    }
    Some(tier)
}
