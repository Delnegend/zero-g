//! Runs `observe` as agy would: the hook event on stdin, a transcript under `~/.gemini`.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::gateway;

/// Where agy keeps transcripts, relative to `HOME`.
const BRAIN: &str = ".gemini/antigravity-cli/brain";

/// Transcript lines: a model step before the invocation, a system step, then the target.
fn transcript() -> String {
    [
        r#"{"source":"MODEL","status":"DONE","step_index":0,"input_tokens":99}"#,
        r#"{"source":"SYSTEM_SDK","status":"DONE","step_index":1}"#,
        r#"{"source":"MODEL","status":"DONE","step_index":2,"input_tokens":12,"output_tokens":3,"content":"hi"}"#,
    ]
    .join("\n")
        + "\n"
}

/// Create `HOME/BRAIN/<conversation>/logs/transcript.jsonl` with `text` and return its path.
fn write_transcript(home: &Path, text: &str) -> PathBuf {
    let dir = home.join(BRAIN).join("conversation/logs");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("transcript.jsonl");
    std::fs::write(&path, text).unwrap();
    path
}

/// A hook event for `transcript` with `initialNumSteps` 1.
fn event(transcript: &Path) -> String {
    serde_json::json!({
        "transcriptPath": transcript,
        "initialNumSteps": 1,
        "conversationId": "conversation",
        "modelName": "gemini-test",
        "lastUserInput": "hello",
    })
    .to_string()
}

/// Run the hook with `stdin`, a fake HOME, and the given extra environment.
fn observe(home: &Path, stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zero-g-agy-integration"))
        .arg("observe")
        .env_clear()
        .env("HOME", home)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// The settings every observation test uses.
fn settings(url: &str) -> Vec<(&str, &str)> {
    vec![("ZERO_G_URL", url), ("ZERO_G_VIRTUAL_KEY", "zg-test")]
}

/// The hook always prints an empty JSON object and succeeds.
fn assert_quiet_success(output: &Output) {
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
    assert_eq!(output.stderr.len(), 0);
}

#[test]
fn posts_the_first_model_response_after_the_injected_system_step() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let path = write_transcript(home.path(), &transcript());
    let output = observe(home.path(), &event(&path), &settings(&url));
    assert_quiet_success(&output);
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/agy/observations");
    assert_eq!(seen[0].authorization, "Bearer zg-test");
    let body = &seen[0].body;
    assert_eq!(body["conversation_id"], "conversation");
    assert_eq!(body["model"], "gemini-test");
    assert_eq!(body["step_index"], 2);
    assert_eq!(body["input_tokens"], 12);
    assert_eq!(body["output_tokens"], 3);
    assert_eq!(body["request"]["lastUserInput"], "hello");
    assert_eq!(body["response"]["content"], "hi");
}

#[test]
fn missing_token_counts_are_null() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let path = write_transcript(
        home.path(),
        "{\"source\":\"MODEL\",\"status\":\"DONE\",\"step_index\":1,\"input_tokens\":-4}\n",
    );
    assert_quiet_success(&observe(home.path(), &event(&path), &settings(&url)));
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert!(seen[0].body["input_tokens"].is_null());
    assert!(seen[0].body["output_tokens"].is_null());
}

#[test]
fn settings_can_come_from_the_settings_file() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let path = write_transcript(home.path(), &transcript());
    let config = home.path().join(".config/zero-g");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("agy-integration.env"),
        format!("ZERO_G_URL={url}\nZERO_G_VIRTUAL_KEY=zg-file\n"),
    )
    .unwrap();
    assert_quiet_success(&observe(home.path(), &event(&path), &[]));
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    assert_eq!(seen[0].authorization, "Bearer zg-file");
}

#[test]
fn only_the_end_of_a_large_transcript_is_read() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let filler = format!("{{\"source\":\"USER\",\"pad\":\"{}\"}}\n", "x".repeat(1000));
    let mut text = filler.repeat(3000);
    text.push_str(&transcript());
    let path = write_transcript(home.path(), &text);
    assert_quiet_success(&observe(home.path(), &event(&path), &settings(&url)));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_transcript_outside_agys_directory_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    std::fs::create_dir_all(home.path().join(BRAIN)).unwrap();
    let outside = home.path().join("elsewhere.jsonl");
    std::fs::write(&outside, transcript()).unwrap();
    assert_quiet_success(&observe(home.path(), &event(&outside), &settings(&url)));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_symlinked_transcript_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let real = home.path().join("secret.jsonl");
    std::fs::write(&real, transcript()).unwrap();
    let dir = home.path().join(BRAIN).join("conversation");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("transcript.jsonl");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_quiet_success(&observe(home.path(), &event(&link), &settings(&url)));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn bad_events_and_missing_settings_still_succeed_quietly() {
    let home = tempfile::tempdir().unwrap();
    let (url, seen) = gateway(200);
    let path = write_transcript(home.path(), &transcript());
    for stdin in [
        "not json",
        "{}",
        r#"{"transcriptPath":"/x","initialNumSteps":-1}"#,
    ] {
        assert_quiet_success(&observe(home.path(), stdin, &settings(&url)));
    }
    // No settings at all: nothing is sent and nothing is printed besides `{}`.
    assert_quiet_success(&observe(home.path(), &event(&path), &[]));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn an_unreachable_or_refusing_gateway_never_fails_the_hook() {
    let home = tempfile::tempdir().unwrap();
    let path = write_transcript(home.path(), &transcript());
    let (refusing, _seen) = gateway(500);
    for url in ["http://127.0.0.1:1", refusing.as_str()] {
        assert_quiet_success(&observe(home.path(), &event(&path), &settings(url)));
    }
}

#[test]
fn a_stdin_that_never_closes_cannot_hold_the_hook_past_its_budget() {
    let home = tempfile::tempdir().unwrap();
    let (url, _seen) = gateway(200);
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_zero-g-agy-integration"))
        .arg("observe")
        .env_clear()
        .env("HOME", home.path())
        .envs(settings(&url))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep the pipe open and say nothing.
    let held = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    drop(held);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "{}");
    assert!(started.elapsed() < Duration::from_secs(5));
}
