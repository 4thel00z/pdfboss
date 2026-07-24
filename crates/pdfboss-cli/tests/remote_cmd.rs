//! End-to-end tests for the `http(s)://` input path shared by `json`, `hex`,
//! `q` and `tui`. These drive the actual `pdfboss` binary against a minimal local
//! HTTP range server, so the whole chain — URL detection, the aio HTTP
//! backend, range requests, and the CLI's own bounds guard — runs for real.
//!
//! The server is modeled on the tokio-based mock in
//! `crates/pdfboss-aio/tests/http.rs` (HEAD → `Content-Length`; GET with
//! `Range: bytes=a-b` → `206` + `Content-Range` + partial body), but built on
//! `std::net` instead: the CLI test drives a *subprocess* synchronously via
//! `std::process::Command`, so a blocking server on a background OS thread is
//! the natural fit (no tokio runtime needed in this test binary).

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use common::{fixture, pdfboss, stdout_str};

/// A minimal blocking HTTP/1.1 range server bound to an ephemeral port.
struct MockServer {
    addr: SocketAddr,
    done: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Starts serving `data` in the background. Each connection is handled
    /// on its own thread (mirroring the aio mock's per-connection
    /// `tokio::spawn`), so a kept-alive HEAD connection never blocks a
    /// concurrent GET.
    fn start(data: Vec<u8>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let done = Arc::new(AtomicBool::new(false));
        let done_for_accept = Arc::clone(&done);
        let accept_thread = thread::spawn(move || {
            for incoming in listener.incoming() {
                if done_for_accept.load(Ordering::Acquire) {
                    break;
                }
                let Ok(socket) = incoming else { continue };
                let data = data.clone();
                thread::spawn(move || handle_connection(socket, &data));
            }
        });
        MockServer {
            addr,
            done,
            accept_thread: Some(accept_thread),
        }
    }

    /// A `http://127.0.0.1:PORT/name` URL served by this instance.
    fn url(&self, name: &str) -> String {
        format!("http://{}/{name}", self.addr)
    }
}

impl Drop for MockServer {
    /// Deterministic, sleep-free shutdown: flip the done flag, then unblock
    /// the accept thread's blocking `accept()` with a throwaway connection so
    /// it notices the flag and exits; join it so no server thread outlives
    /// the test.
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Serves requests on one connection until the client closes it: a HEAD
/// gets `Content-Length`; a GET with a satisfiable `Range` gets a `206` +
/// `Content-Range` + partial body; anything else gets the full `200` body
/// (exercised by neither test here, but keeps the server honest).
fn handle_connection(mut socket: TcpStream, data: &[u8]) {
    let total = data.len();
    loop {
        let Some(head) = read_request_head(&mut socket) else {
            return; // client closed the connection
        };
        if head.starts_with("HEAD ") {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\r\n"
            );
            if socket.write_all(response.as_bytes()).is_err() {
                return;
            }
            continue;
        }
        match parse_range_header(&head) {
            Some((start, end)) if start < total && start <= end => {
                let end = end.min(total - 1);
                let body = &data[start..=end];
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     Content-Range: bytes {start}-{end}/{total}\r\n\
                     Content-Length: {}\r\n\r\n",
                    body.len()
                );
                if socket.write_all(response.as_bytes()).is_err() || socket.write_all(body).is_err()
                {
                    return;
                }
            }
            _ => {
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n\r\n");
                if socket.write_all(response.as_bytes()).is_err() || socket.write_all(data).is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Reads one request head (through the blank line); `None` on a cleanly
/// closed connection.
fn read_request_head(socket: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match socket.read(&mut byte) {
            Ok(0) => {
                return if head.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&head).into_owned())
                };
            }
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    return Some(String::from_utf8_lossy(&head).into_owned());
                }
            }
            Err(_) => return None,
        }
    }
}

/// Extracts `Range: bytes=a-b` from a request head.
fn parse_range_header(head: &str) -> Option<(usize, usize)> {
    let line = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))?;
    let spec = line.split('=').nth(1)?.trim();
    let (start, end) = spec.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
}

fn hello_bytes() -> Vec<u8> {
    std::fs::read(fixture("hello.pdf")).expect("fixture reads")
}

#[test]
fn json_over_url_matches_local() {
    let server = MockServer::start(hello_bytes());
    let url = server.url("hello.pdf");

    let remote = pdfboss(&["json", &url]);
    assert!(remote.status.success(), "json over url failed: {remote:?}");

    let file = fixture("hello.pdf");
    let local = pdfboss(&["json", file.to_str().unwrap()]);
    assert!(
        local.status.success(),
        "json over local file failed: {local:?}"
    );

    assert_eq!(
        remote.stdout, local.stdout,
        "json output over url must match local byte-for-byte"
    );
}

#[test]
fn hex_over_url_range_guard_fires() {
    let data = hello_bytes();
    let len = data.len() as u64;
    assert!(
        len < 999_999,
        "fixture grew past the selector's hard-coded out-of-range bound"
    );
    let server = MockServer::start(data);
    let url = server.url("hello.pdf");

    let output = pdfboss(&["hex", &url, "range:0-999999"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected the bounds guard to fail: {output:?}"
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains(&len.to_string()),
        "error does not mention the file length {len}: {err}"
    );
}

#[test]
fn q_over_url_basic() {
    let server = MockServer::start(hello_bytes());
    let url = server.url("hello.pdf");

    let output = pdfboss(&["q", &url, ".header.version"]);
    assert!(output.status.success(), "q over url failed: {output:?}");
    assert_eq!(stdout_str(&output), "\"1.7\"\n");
}

/// `tui` must accept `http(s)://` targets unconditionally in the default
/// build, exactly like `json`/`hex`/`q` above -- no opt-in cargo feature.
/// No mock server needed: an unreachable loopback port still proves the
/// point, since the failure must come from the aio HTTP backend actually
/// being invoked (an "http:"-prefixed transport error), not from a
/// feature-gate rejection that never touches the network.
#[test]
fn tui_over_unreachable_url_fails_with_http_error_not_a_feature_gate() {
    let output = pdfboss(&["tui", "http://127.0.0.1:1/nope.pdf"]);
    assert_eq!(output.status.code(), Some(1), "expected exit 1: {output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        !err.contains("--features http"),
        "tui must not require an opt-in http feature: {err}"
    );
    assert!(
        err.contains("http:"),
        "expected the aio HTTP backend's own transport error (\"http: ...\"), got: {err}"
    );
}
