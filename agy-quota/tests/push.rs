//! Runs the real binary against a fake `agy` script and a local HTTP server.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

/// A fake `agy`: `/usage` prints `$FAKE_USAGE`, `acp` answers the three ACP calls.
const FAKE_AGY: &str = r#"#!/bin/sh
echo "$@" >> "$FAKE_AGY_LOG"
if [ "$1" = "acp" ]; then
  [ "$FAKE_ACP" = "broken" ] && exit 1
  while IFS= read -r line; do
    id=$(printf '%s' "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
    case "$line" in
      *'"session/new"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1","_meta":{"antigravity":{"tier":{"name":"Google AI Pro","id":"g1-pro-tier"}}}}}\n' "$id";;
      *) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id";;
    esac
  done
else
  printf '%s\n' "$FAKE_USAGE"
fi
"#;

/// A valid `/usage` output.
const USAGE: &str = r#"{"command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-5h","window":"5h","remaining_fraction":0.5,"reset_time":"2026-11-25T10:30:13Z"}]}]}}}"#;

/// One request the gateway received.
struct Received {
    /// Request path.
    path: String,
    /// The `Authorization` header.
    authorization: String,
    /// The JSON body.
    body: serde_json::Value,
}

/// A gateway stand-in that answers every request with `status` and records it.
fn gateway(status: u16) -> (String, Arc<Mutex<Vec<Received>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut data = Vec::new();
            let mut buffer = [0u8; 4096];
            let (head, body) = loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                data.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&data).into_owned();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if rest.len() >= length || read == 0 {
                        break (head.to_owned(), rest.to_owned());
                    }
                } else if read == 0 {
                    return;
                }
            };
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let authorization = head
                .lines()
                .find_map(|line| line.strip_prefix("authorization: "))
                .unwrap_or_default()
                .to_owned();
            log.lock().unwrap().push(Received {
                path,
                authorization,
                body: serde_json::from_str(&body).unwrap_or_default(),
            });
            let _ = stream.write_all(
                format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
        }
    });
    (url, seen)
}

/// Write the fake agy into `dir` and return its path.
fn install_agy(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("agy");
    std::fs::write(&path, FAKE_AGY).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Run the collector once with the fake agy and gateway.
fn run(dir: &Path, url: &str, usage: &str, acp: &str) -> Output {
    let agy = install_agy(dir);
    Command::new(env!("CARGO_BIN_EXE_zero-g-agy-quota"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("AGY_BIN", agy)
        .env("FAKE_AGY_LOG", dir.join("agy.log"))
        .env("FAKE_USAGE", usage)
        .env("FAKE_ACP", acp)
        .env("ZERO_G_CACHE_DIR", dir.join("cache"))
        .env("ZERO_G_URL", url)
        .env("ZERO_G_VIRTUAL_KEY", "zg-test")
        .env("ZERO_G_AGY_ACCOUNT", "Kien")
        .output()
        .unwrap()
}

/// How many times the fake agy was started with this first argument.
fn agy_calls(dir: &Path, first: &str) -> usize {
    std::fs::read_to_string(dir.join("agy.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with(first))
        .count()
}

#[test]
fn pushes_quota_and_plan_and_reads_the_plan_once_a_day() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    for _ in 0..2 {
        let output = run(dir.path(), &url, USAGE, "ok");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert_eq!(seen.len(), 2);
    for received in &seen {
        assert_eq!(received.path, "/v1/agy/quota");
        assert_eq!(received.authorization, "Bearer zg-test");
        assert_eq!(received.body["account"], "Kien");
        assert_eq!(
            received.body["report"]["groups"][0]["name"],
            "Gemini Models"
        );
        assert_eq!(received.body["tier"]["name"], "Google AI Pro");
        assert_eq!(received.body["tier"]["id"], "g1-pro-tier");
    }
    assert_eq!(agy_calls(dir.path(), "acp"), 1);
    assert_eq!(agy_calls(dir.path(), "--print"), 2);
}

#[test]
fn an_unreadable_plan_still_pushes_the_quota() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let output = run(dir.path(), &url, USAGE, "broken");
    assert!(output.status.success());
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert_eq!(seen.len(), 1);
    assert!(seen[0].body.get("tier").is_none());
}

#[test]
fn unexpected_agy_output_sends_nothing_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let output = run(dir.path(), &url, r#"{"response":"hello"}"#, "ok");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not read quota from agy"));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_rejected_report_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _) = gateway(404);
    let output = run(dir.path(), &url, USAGE, "ok");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("did not accept the report: HTTP 404")
    );
}

#[test]
fn settings_can_come_from_a_file_and_missing_ones_fail() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let agy = install_agy(dir.path());
    let settings = dir.path().join("agy-quota.env");
    std::fs::write(
        &settings,
        format!(
            "# test\nZERO_G_URL={url}\nZERO_G_VIRTUAL_KEY=\"zg-file\"\nZERO_G_AGY_ACCOUNT=From File\nAGY_BIN={}\nZERO_G_CACHE_DIR={}\n",
            agy.display(),
            dir.path().join("cache").display()
        ),
    )
    .unwrap();
    let command = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_zero-g-agy-quota"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("FAKE_AGY_LOG", dir.path().join("agy.log"))
            .env("FAKE_USAGE", USAGE)
            .env("FAKE_ACP", "ok")
            .args(extra)
            .output()
            .unwrap()
    };
    let output = command(&["--config", settings.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert_eq!(seen[0].authorization, "Bearer zg-file");
    assert_eq!(seen[0].body["account"], "From File");
    let missing = command(&[]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("ZERO_G_URL"));
}

#[test]
fn version_and_help_print_without_settings() {
    let version = Command::new(env!("CARGO_BIN_EXE_zero-g-agy-quota"))
        .arg("--version")
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("zero-g-agy-quota {}", env!("CARGO_PKG_VERSION"))
    );
    let help = Command::new(env!("CARGO_BIN_EXE_zero-g-agy-quota"))
        .arg("--help")
        .env_clear()
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("ZERO_G_AGY_ACCOUNT"));
}
