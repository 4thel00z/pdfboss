#![cfg(feature = "http")]

//! The HTTP backend against a local mock server: a Range-honoring server
//! yields a working document; a Range-refusing server (200 with the full
//! body) yields `Error::RangeUnsupported`.

use std::net::SocketAddr;
use std::time::Duration;

use pdfboss_aio::{AsyncDocument, Backend, Error, HttpBackend};
use pdfboss_testkit::simple_doc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Serves `data` from a minimal hand-rolled HTTP/1.1 responder.
/// `honor_range` selects whether GETs with a Range header receive 206
/// slices or the full 200 body.
async fn spawn_server(data: Vec<u8>, honor_range: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((socket, peer)) = listener.accept().await else {
                break;
            };
            let payload = data.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_connection(socket, payload, honor_range).await {
                    eprintln!("mock server, peer {peer}: {err}");
                }
            });
        }
    });
    addr
}

async fn handle_connection(
    mut socket: TcpStream,
    data: Vec<u8>,
    honor_range: bool,
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
    let addr = spawn_server(data, true).await;
    let doc = AsyncDocument::open_url(format!("http://{addr}/remote.pdf"))
        .await
        .unwrap();
    assert_eq!(doc.version(), sync_doc.version());
    assert_eq!(doc.page_count(), sync_doc.page_count());
    assert_eq!(doc.metadata().await.unwrap(), sync_doc.metadata());
}

#[tokio::test]
async fn range_refusing_server_yields_range_unsupported() {
    let data = simple_doc("no ranges");
    let addr = spawn_server(data, false).await;
    match AsyncDocument::open_url(format!("http://{addr}/no-ranges.pdf")).await {
        Err(Error::RangeUnsupported) => {}
        Err(other) => panic!("expected RangeUnsupported, got {other:?}"),
        Ok(doc) => panic!(
            "expected failure, opened a document with {} pages",
            doc.page_count()
        ),
    }
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
