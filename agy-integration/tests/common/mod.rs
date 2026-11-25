//! Helpers shared by the integration tests: a gateway stand-in that records requests.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    dead_code
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

/// One request the gateway received.
pub(crate) struct Received {
    /// Request path.
    pub(crate) path: String,
    /// The `Authorization` header.
    pub(crate) authorization: String,
    /// The JSON body.
    pub(crate) body: serde_json::Value,
}

/// A gateway stand-in that answers every request with `status` and records it.
pub(crate) fn gateway(status: u16) -> (String, Arc<Mutex<Vec<Received>>>) {
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
