//! The `observe` command: agy's `PostInvocation` hook.
//!
//! agy runs it after each model invocation and passes an event on stdin. The command finds the
//! model response in the conversation transcript and posts it to Zero-G. It is best effort: any
//! problem means the observation is skipped, and the caller prints `{}` either way so the hook
//! can never interrupt agy.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::{config::Settings, gateway};

/// Largest hook event accepted on stdin.
const MAX_INPUT: u64 = 1024 * 1024;
/// How much of the end of the transcript is searched.
const MAX_TRANSCRIPT: u64 = 2 * 1024 * 1024;
/// Longest the post may take.
const POST_TIMEOUT: Duration = Duration::from_secs(2);

/// The fields of the hook event that matter.
struct Event {
    /// Path of the conversation transcript.
    transcript: PathBuf,
    /// Index of the first step of this invocation.
    start: i64,
    /// Conversation id.
    conversation: String,
    /// Model name.
    model: String,
    /// What the user last typed, or null.
    last_user_input: Value,
}

impl Event {
    /// Read the event, or `None` if a required field is missing or malformed.
    fn parse(value: &Value) -> Option<Self> {
        let text = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        };
        Some(Self {
            transcript: PathBuf::from(value.get("transcriptPath")?.as_str()?),
            start: value
                .get("initialNumSteps")
                .and_then(Value::as_i64)
                .filter(|start| *start >= 0)?,
            conversation: text("conversationId")?.to_owned(),
            model: text("modelName")?.to_owned(),
            last_user_input: value.get("lastUserInput").cloned().unwrap_or(Value::Null),
        })
    }
}

/// Where agy keeps conversation transcripts.
fn transcript_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(&home).join(".gemini/antigravity-cli/brain"))
}

/// Read the end of a transcript, or `None` if the file is not a plain file inside `root`.
///
/// The path comes from the hook event, so it is checked after opening: no symlinks, a regular
/// file, and the opened file really lives under `root`.
fn read_tail(path: &Path, root: &Path) -> Option<Vec<u8>> {
    let root = std::fs::canonicalize(root).ok()?;
    if std::fs::symlink_metadata(path)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let mut file: File = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let info = file.metadata().ok()?;
    if !info.is_file() {
        return None;
    }
    let resolved = if cfg!(target_os = "linux") {
        std::fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?
    } else {
        std::fs::canonicalize(path).ok()?
    };
    if !resolved.starts_with(&root) {
        return None;
    }
    let current = std::fs::symlink_metadata(path).ok()?;
    let target = std::fs::metadata(&resolved).ok()?;
    let same = |other: &std::fs::Metadata| other.dev() == info.dev() && other.ino() == info.ino();
    if current.file_type().is_symlink() || !same(&current) || !same(&target) {
        return None;
    }
    let offset = info.len().saturating_sub(MAX_TRANSCRIPT);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut data = Vec::new();
    file.take(MAX_TRANSCRIPT).read_to_end(&mut data).ok()?;
    if offset > 0 {
        // The cut usually lands mid-line, so drop the partial first line.
        let newline = data.iter().position(|byte| *byte == b'\n')?;
        data.drain(..=newline);
    }
    Some(data)
}

/// The first completed model response at or after `start`.
fn find_record(data: &[u8], start: i64) -> Option<Value> {
    data.split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|record| {
            record.get("source").and_then(Value::as_str) == Some("MODEL")
                && record.get("status").and_then(Value::as_str) == Some("DONE")
                && step_index(record).is_some_and(|step| step >= start)
        })
        .min_by_key(|record| step_index(record).unwrap_or(i64::MAX))
}

/// The record's step index, if it is an integer.
fn step_index(record: &Value) -> Option<i64> {
    record.get("step_index").and_then(Value::as_i64)
}

/// A non-negative token count, or null when absent or invalid.
fn count(record: &Value, name: &str) -> Value {
    record
        .get(name)
        .and_then(Value::as_i64)
        .filter(|count| *count >= 0)
        .map_or(Value::Null, Value::from)
}

/// The observation body for an event and its transcript record.
fn observation(event: &Event, record: &Value) -> Value {
    json!({
        "conversation_id": event.conversation,
        "step_index": step_index(record),
        "model": event.model,
        "input_tokens": count(record, "input_tokens"),
        "output_tokens": count(record, "output_tokens"),
        "request": {"lastUserInput": event.last_user_input},
        "response": record,
    })
}

/// Read the event from stdin, find its record and post it. Every failure is a silent skip.
pub(crate) async fn collect(settings: Option<&Settings>) {
    let Some(settings) = settings else { return };
    let mut input = Vec::new();
    if tokio::io::stdin()
        .take(MAX_INPUT.saturating_add(1))
        .read_to_end(&mut input)
        .await
        .is_err()
        || u64::try_from(input.len()).is_ok_and(|len| len > MAX_INPUT)
    {
        return;
    }
    let Some(event) = serde_json::from_slice::<Value>(&input)
        .ok()
        .as_ref()
        .and_then(Event::parse)
    else {
        return;
    };
    let Some(data) = transcript_root().and_then(|root| read_tail(&event.transcript, &root)) else {
        return;
    };
    let Some(record) = find_record(&data, event.start) else {
        return;
    };
    let body = observation(&event, &record);
    let _ = gateway::post(settings, "/v1/agy/observations", &body, POST_TIMEOUT).await;
}

#[cfg(test)]
mod tests {
    use super::{count, find_record};
    use serde_json::json;

    #[test]
    fn picks_the_first_finished_model_step_at_or_after_the_start() {
        let lines = [
            json!({"source": "MODEL", "status": "DONE", "step_index": 0, "input_tokens": 99}),
            json!({"source": "SYSTEM_SDK", "status": "DONE", "step_index": 1}),
            json!({"source": "MODEL", "status": "RUNNING", "step_index": 2}),
            json!({"source": "MODEL", "status": "DONE", "step_index": 4, "input_tokens": 40}),
            json!({"source": "MODEL", "status": "DONE", "step_index": 3, "input_tokens": 12}),
        ];
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let record = find_record(format!("not json\n{text}\n").as_bytes(), 1).unwrap();
        assert_eq!(record["step_index"], 3);
        assert!(find_record(text.as_bytes(), 5).is_none());
    }

    #[test]
    fn counts_are_null_unless_a_non_negative_integer() {
        let record = json!({"a": 5, "b": -1, "c": 1.5, "d": "7"});
        assert_eq!(count(&record, "a"), json!(5));
        for name in ["b", "c", "d", "missing"] {
            assert_eq!(count(&record, name), json!(null), "{name}");
        }
    }
}
