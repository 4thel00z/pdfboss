#![cfg(feature = "http")]

//! The HTTP backend against a local mock server: a Range-honoring server
//! yields a working document; a Range-refusing server (200 with the full
//! body) yields a working document too, from a single full download.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pdfboss_aio::{AsyncDocument, Backend, CachedBackend, HttpBackend};
use pdfboss_testkit::simple_doc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Serves `data` from a minimal hand-rolled HTTP/1.1 responder.
/// `honor_range` selects whether GETs with a Range header receive 206
/// slices or the full 200 body. The returned counter tracks GET requests
/// (HEADs excluded).
async fn spawn_server(data: Vec<u8>, honor_range: bool) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gets = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&gets);
    tokio::spawn(async move {
        loop {
            let Ok((socket, peer)) = listener.accept().await else {
                break;
            };
            let payload = data.clone();
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                if let Err(err) = handle_connection(socket, payload, honor_range, counter).await {
                    eprintln!("mock server, peer {peer}: {err}");
                }
            });
        }
    });
    (addr, gets)
}

async fn handle_connection(
    mut socket: TcpStream,
    data: Vec<u8>,
    honor_range: bool,
    gets: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    loop {
        let Some(head) = read_request_head(&mut socket).await? else {
            return Ok(()); // client closed the connection
        };
        let total = data.len();
        if head.starts_with("HEAD ") {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await?;
            continue;
        }
        gets.fetch_add(1, Ordering::SeqCst);
        match parse_range_header(&head).filter(|_| honor_range) {
            Some((start, end)) => {
                let end = end.min(total - 1);
                let body = &data[start..=end];
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     Content-Range: bytes {start}-{end}/{total}\r\n\
                     Content-Length: {}\r\n\r\n",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await?;
                socket.write_all(body).await?;
            }
            None => {
                // Range ignored (or absent): full body with 200.
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n\r\n");
                socket.write_all(response.as_bytes()).await?;
                socket.write_all(&data).await?;
            }
        }
    }
}

/// Reads one request head (through the blank line); `None` on a cleanly
/// closed connection.
async fn read_request_head(socket: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let count = socket.read(&mut byte).await?;
        if count == 0 {
            return Ok(if head.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&head).into_owned())
            });
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(Some(String::from_utf8_lossy(&head).into_owned()));
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

#[tokio::test]
async fn open_url_works_against_a_range_honoring_server() {
    let data = simple_doc("remote");
    let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
    let (addr, _) = spawn_server(data, true).await;
    let doc = AsyncDocument::open_url(format!("http://{addr}/remote.pdf"))
        .await
        .unwrap();
    assert_eq!(doc.version(), sync_doc.version());
    assert_eq!(doc.page_count(), sync_doc.page_count());
    assert_eq!(doc.metadata().await.unwrap(), sync_doc.metadata());
}

#[tokio::test]
async fn range_refusing_server_falls_back_to_one_full_download() {
    let data = simple_doc("no ranges");
    let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
    let (addr, gets) = spawn_server(data, false).await;
    let doc = AsyncDocument::open_url(format!("http://{addr}/no-ranges.pdf"))
        .await
        .unwrap();
    assert_eq!(doc.version(), sync_doc.version());
    assert_eq!(doc.page_count(), sync_doc.page_count());
    assert_eq!(doc.metadata().await.unwrap(), sync_doc.metadata());
    assert_eq!(
        gets.load(Ordering::SeqCst),
        1,
        "every read after the 200 fallback must come from memory, not new GETs"
    );
}

#[tokio::test]
async fn dense_scan_over_http_batches_ranged_requests() {
    // 1 MiB of data on a range-honoring server, read end to end through
    // the default 64 KiB cache grid: adaptive batching must collapse the
    // 16 chunk misses into a handful of ranged GETs (1, 1, 2, 4, 8).
    let data: Vec<u8> = (0..1_048_576u32).map(|v| (v % 251) as u8).collect();
    let (addr, gets) = spawn_server(data.clone(), true).await;
    let backend = CachedBackend::new(
        HttpBackend::new(format!("http://{addr}/big.bin"))
            .await
            .unwrap(),
    );
    let mut got = vec![0u8; 64 * 1024];
    for step in 0..16u64 {
        let offset = step * 64 * 1024;
        assert_eq!(backend.read_at(offset, &mut got).await.unwrap(), got.len());
        assert_eq!(got[0], data[offset as usize], "content intact at {offset}");
    }
    assert_eq!(
        gets.load(Ordering::SeqCst),
        5,
        "16 dense chunk misses must batch into 5 ranged GETs"
    );
}

/// Serves ranges correctly, except that the first `flake_count` GETs answer
/// 500 — a server that buckles intermittently under sustained request load.
async fn spawn_flaky_server(data: Vec<u8>, flake_count: usize) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gets = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&gets);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let data = data.clone();
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                loop {
                    let head = match read_request_head(&mut socket).await {
                        Ok(Some(head)) => head,
                        _ => return, // closed connection or socket error
                    };
                    let total = data.len();
                    if head.starts_with("HEAD ") {
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\r\n"
                        );
                        if socket.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                        continue;
                    }
                    let seen = counter.fetch_add(1, Ordering::SeqCst);
                    if seen < flake_count {
                        let response =
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
                        if socket.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                        continue;
                    }
                    let (start, end) = parse_range_header(&head).unwrap_or((0, total - 1));
                    let end = end.min(total - 1);
                    let body = &data[start..=end];
                    let response = format!(
                        "HTTP/1.1 206 Partial Content\r\n\
                         Content-Range: bytes {start}-{end}/{total}\r\n\
                         Content-Length: {}\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(response.as_bytes()).await.is_err()
                        || socket.write_all(body).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (addr, gets)
}

#[tokio::test]
async fn transient_500_is_retried_and_the_read_succeeds() {
    let data = b"0123456789abcdef".to_vec();
    let (addr, gets) = spawn_flaky_server(data, 1).await;
    let backend = HttpBackend::new(format!("http://{addr}/flaky.bin"))
        .await
        .unwrap();
    let mut buf = [0u8; 4];
    assert_eq!(backend.read_at(10, &mut buf).await.unwrap(), 4);
    assert_eq!(&buf, b"abcd");
    assert_eq!(gets.load(Ordering::SeqCst), 2, "one 500, one retried 206");
}

#[tokio::test]
async fn persistent_500_surfaces_after_bounded_retries() {
    let data = b"0123456789abcdef".to_vec();
    let (addr, gets) = spawn_flaky_server(data, usize::MAX).await;
    let backend = HttpBackend::new(format!("http://{addr}/dead.bin"))
        .await
        .unwrap();
    let mut buf = [0u8; 4];
    let err = backend.read_at(0, &mut buf).await.unwrap_err();
    match pdfboss_aio::Error::from(err) {
        pdfboss_aio::Error::Http {
            status: Some(500), ..
        } => {}
        other => panic!("expected Http 500, got {other:?}"),
    }
    assert_eq!(
        gets.load(Ordering::SeqCst),
        3,
        "three attempts, then the failure surfaces"
    );
}

#[tokio::test]
async fn fallback_reports_progress_through_the_whole_download() {
    let data = simple_doc("progress");
    let total = data.len() as u64;
    let (addr, _) = spawn_server(data, false).await;
    let seen: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let backend = HttpBackend::new(format!("http://{addr}/progress.pdf"))
        .await
        .unwrap()
        .on_fallback_progress(move |collected, declared| {
            sink.lock().unwrap().push((collected, declared));
        });
    let doc = AsyncDocument::with_backend_with_password(CachedBackend::new(backend), "")
        .await
        .unwrap();
    assert_eq!(doc.page_count(), 1);
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.first(),
        Some(&(0, total)),
        "the fallback announces itself before the first byte"
    );
    assert_eq!(
        seen.last(),
        Some(&(total, total)),
        "the last report covers the whole body"
    );
    assert!(
        seen.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "collected bytes only grow"
    );
    assert!(
        seen.iter().all(|&(_, declared)| declared == total),
        "the declared total never changes"
    );
}

#[tokio::test]
async fn range_honoring_server_never_reports_fallback_progress() {
    let data = simple_doc("no progress");
    let (addr, _) = spawn_server(data, true).await;
    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let backend = HttpBackend::new(format!("http://{addr}/plain.pdf"))
        .await
        .unwrap()
        .on_fallback_progress(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
    let doc = AsyncDocument::with_backend_with_password(CachedBackend::new(backend), "")
        .await
        .unwrap();
    assert_eq!(doc.page_count(), 1);
    assert_eq!(fired.load(Ordering::SeqCst), 0);
}

/// Serves a HEAD with a small declared length, but answers every GET with a
/// 206 whose `Content-Length` promises ~100 MB, delivers only `prefix`, and
/// then goes silent without closing the connection — a hostile (or merely
/// buggy) server whose Range response body is far larger than what was
/// requested and never actually finishes arriving.
async fn spawn_oversized_body_server(prefix: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let prefix = prefix.clone();
            tokio::spawn(async move {
                let _ = handle_oversized_connection(socket, prefix).await;
            });
        }
    });
    addr
}

async fn handle_oversized_connection(
    mut socket: TcpStream,
    prefix: Vec<u8>,
) -> std::io::Result<()> {
    loop {
        let Some(head) = read_request_head(&mut socket).await? else {
            return Ok(()); // client closed the connection
        };
        if head.starts_with("HEAD ") {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n\r\n",
                prefix.len()
            );
            socket.write_all(response.as_bytes()).await?;
            continue;
        }
        // Any GET (Range or not): promise a huge body, deliver only the
        // prefix, then go quiet forever without closing the connection.
        let response = "HTTP/1.1 206 Partial Content\r\n\
                         Content-Range: bytes 0-99999999/100000000\r\n\
                         Content-Length: 100000000\r\n\r\n";
        socket.write_all(response.as_bytes()).await?;
        socket.write_all(&prefix).await?;
        socket.flush().await?;
        tokio::time::sleep(Duration::from_secs(3600)).await;
        return Ok(());
    }
}

/// Serves a HEAD declaring `declared_len`, and answers every GET with a
/// plain 200 (no Range support at all, like `python3 -m http.server`)
/// promising `promised_len` bytes, delivering only `body`, then either
/// hanging forever or closing cleanly.
async fn spawn_200_server(
    declared_len: usize,
    promised_len: usize,
    body: Vec<u8>,
    hang: bool,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let _ = handle_200_connection(socket, declared_len, promised_len, body, hang).await;
            });
        }
    });
    addr
}

async fn handle_200_connection(
    mut socket: TcpStream,
    declared_len: usize,
    promised_len: usize,
    body: Vec<u8>,
    hang: bool,
) -> std::io::Result<()> {
    loop {
        let Some(head) = read_request_head(&mut socket).await? else {
            return Ok(()); // client closed the connection
        };
        if head.starts_with("HEAD ") {
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {declared_len}\r\n\r\n");
            socket.write_all(response.as_bytes()).await?;
            continue;
        }
        let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {promised_len}\r\n\r\n");
        socket.write_all(response.as_bytes()).await?;
        socket.write_all(&body).await?;
        socket.flush().await?;
        if hang {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        return Ok(());
    }
}

#[tokio::test]
async fn fallback_caps_collection_of_an_oversized_200_body() {
    let full = b"ABCDEFGH".to_vec();
    // The 200 promises ~100 MB and never finishes; collection must stop at
    // the HEAD-declared length and serve reads from what arrived.
    let addr = spawn_200_server(full.len(), 100_000_000, full.clone(), true).await;
    let backend = HttpBackend::new(format!("http://{addr}/big.bin"))
        .await
        .unwrap();
    let mut buf = [0u8; 8];
    let outcome = tokio::time::timeout(Duration::from_secs(5), backend.read_at(0, &mut buf)).await;
    let count = outcome
        .expect(
            "read_at must return once the declared length has arrived, not \
             block waiting for the rest of a never-finishing 200 body",
        )
        .unwrap();
    assert_eq!(count, buf.len(), "reads exactly the declared length");
    assert_eq!(&buf, full.as_slice(), "correct content, no error");
}

#[tokio::test]
async fn fallback_body_shorter_than_declared_length_yields_short_reads() {
    let body = b"ABCDEFGH".to_vec();
    // HEAD declared 16 bytes but the 200 delivers only 8 and closes: reads
    // past the delivered body are short, never a panic.
    let addr = spawn_200_server(16, body.len(), body.clone(), false).await;
    let backend = HttpBackend::new(format!("http://{addr}/short.bin"))
        .await
        .unwrap();
    let mut tail = [0u8; 4];
    assert_eq!(backend.read_at(12, &mut tail).await.unwrap(), 0);
    let mut head = [0u8; 8];
    assert_eq!(backend.read_at(0, &mut head).await.unwrap(), 8);
    assert_eq!(&head, body.as_slice());
}

#[tokio::test]
async fn read_at_caps_collection_of_an_oversized_response_body() {
    let prefix = b"ABCDEFGH".to_vec();
    let addr = spawn_oversized_body_server(prefix.clone()).await;
    let backend = HttpBackend::new(format!("http://{addr}/big.bin"))
        .await
        .unwrap();
    let mut buf = [0u8; 8];
    let outcome = tokio::time::timeout(Duration::from_secs(5), backend.read_at(0, &mut buf)).await;
    let count = outcome
        .expect(
            "read_at must return once buf is full, not block waiting for \
             the rest of a server's oversized/never-finishing body",
        )
        .unwrap();
    assert_eq!(count, buf.len(), "reads exactly buf.len() bytes");
    assert_eq!(&buf, prefix.as_slice(), "correct prefix content, no error");
}
