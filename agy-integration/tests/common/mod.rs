//! Helpers shared by the integration tests: a gateway stand-in that records requests.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    dead_code
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::LockResult;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// One request the gateway received.
pub(crate) struct Received {
    /// Request path.
    pub(crate) path: String,
    /// The `Authorization` header.
    pub(crate) authorization: String,
    /// The JSON body.
    pub(crate) body: serde_json::Value,
}

/// A handle to the fake gateway: its requests and the switch that stops its thread.
pub(crate) struct Log {
    requests: Arc<Mutex<Vec<Received>>>,
    stop: Arc<AtomicBool>,
}

impl Log {
    /// Lock the recorded requests.
    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, Vec<Received>>> {
        self.requests.lock()
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A gateway stand-in that answers every request with `status` and records it.
///
/// Its thread stops when the returned `Log` is dropped.
pub(crate) fn gateway(status: u16) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::clone(&stop);
    thread::spawn(move || {
        while !running.load(Ordering::Relaxed) {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => return,
            };
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
    (url, Log { requests, stop })
}
