/// Maximum size of an upstream SSE line or assembled data payload.
pub(crate) const MAX_SSE_BYTES: usize = 1024 * 1024;

/// Event name and data of a complete upstream SSE frame.
type SseFrame = (Option<String>, String);
/// Complete frames and, on failure, the subsequent decoding error.
type SseDecodeResult = Result<Vec<SseFrame>, (Vec<SseFrame>, &'static str)>;

/// Incrementally decodes SSE frames without interpreting their payloads.
pub(crate) struct SseRawLineDecoder {
    /// Bytes retained from an incomplete line.
    buffer: Vec<u8>,
    /// Event name accumulated for the current frame.
    current_event: Option<String>,
    /// Data accumulated for the current frame.
    data: Option<String>,
    /// Error reported when a line exceeds the size limit.
    line_error: &'static str,
    /// Error reported when frame data exceeds the size limit.
    data_error: &'static str,
}

impl SseRawLineDecoder {
    /// Creates a decoder with provider-specific size-limit errors.
    pub(crate) const fn new(line_error: &'static str, data_error: &'static str) -> Self {
        Self {
            buffer: Vec::new(),
            current_event: None,
            data: None,
            line_error,
            data_error,
        }
    }

    /// Returns completed frames even when a later line in the chunk fails.
    pub(crate) fn push_chunk(&mut self, chunk: &[u8]) -> SseDecodeResult {
        let mut out = Vec::new();
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            let line_part = part.strip_suffix(b"\n");
            let complete = line_part.is_some();
            let line_part = line_part.unwrap_or(part);
            if line_part.len() > MAX_SSE_BYTES.saturating_sub(self.buffer.len()) {
                return Err((out, self.line_error));
            }
            self.buffer.extend_from_slice(line_part);
            if !complete {
                continue;
            }
            let line = String::from_utf8_lossy(&self.buffer);
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                if let Some(data) = self.data.take() {
                    out.push((self.current_event.take(), data));
                } else {
                    self.current_event = None;
                }
            } else if let Some(event) = line.strip_prefix("event:") {
                self.current_event = Some(event.strip_prefix(' ').unwrap_or(event).to_string());
            } else if let Some(data) = line.strip_prefix("data:") {
                let data = data.strip_prefix(' ').unwrap_or(data);
                let len = self
                    .data
                    .as_ref()
                    .map_or(0, |previous| previous.len().saturating_add(1));
                if data.len() > MAX_SSE_BYTES.saturating_sub(len) || len > MAX_SSE_BYTES {
                    return Err((out, self.data_error));
                }
                if let Some(previous) = &mut self.data {
                    previous.push('\n');
                    previous.push_str(data);
                } else {
                    self.data = Some(data.to_string());
                }
            }
            self.buffer.clear();
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{MAX_SSE_BYTES, SseRawLineDecoder};

    #[test]
    fn retains_completed_frames_before_size_errors() {
        for oversized in [
            "x".repeat(MAX_SSE_BYTES + 1),
            format!(
                "data: {}\ndata: {}\n",
                "x".repeat(MAX_SSE_BYTES / 2),
                "y".repeat(MAX_SSE_BYTES / 2),
            ),
        ] {
            let mut decoder = SseRawLineDecoder::new("line", "data");
            let input = format!("event: ready\ndata: first\ndata: second\n\n{oversized}");
            let (frames, error) = decoder.push_chunk(input.as_bytes()).unwrap_err();
            assert_eq!(frames, vec![(Some("ready".into()), "first\nsecond".into())]);
            assert_eq!(
                error,
                if oversized.starts_with("data:") {
                    "data"
                } else {
                    "line"
                }
            );
        }
    }
}
