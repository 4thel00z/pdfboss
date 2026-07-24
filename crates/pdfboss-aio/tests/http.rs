#![cfg(feature = "http")]

//! The HTTP backend against a local mock server: a Range-honoring server
//! yields a working document; a Range-refusing server (200 with the full
//! body) yields `Error::RangeUnsupported`.

use std::net::SocketAddr;

use pdfboss_aio::{AsyncDocument, Error};
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
