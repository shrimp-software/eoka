//! Request capture example: record a page's HTTP traffic and read a response body.
//!
//! Serves a tiny local site, so no network access is needed.
//! Run with: cargo run --example request_capture

use std::time::Duration;

use eoka::network::RequestCaptureOptions;
use eoka::{Browser, ResponseBody, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Minimal HTTP server: `/api` returns JSON, everything else a page that calls it.
async fn serve() -> std::io::Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/", listener.local_addr()?);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0; 8192];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let (content_type, body) = if path.starts_with("/api") {
                    ("application/json", r#"{"message":"hello from the API"}"#)
                } else {
                    (
                        "text/html",
                        "<!doctype html><title>demo</title><script>fetch('/api/greeting', {method: 'POST', body: 'hi'})</script>",
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    Ok(url)
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = serve().await?;
    let browser = Browser::launch().await?;
    let page = browser.new_blank_page().await?;

    // Start capturing before triggering the traffic.
    let capture = page
        .capture_requests(RequestCaptureOptions::default())
        .await?;
    page.goto(&url).await?;

    let api = capture
        .wait_for(
            |r| r.url.ends_with("/api/greeting") && r.complete,
            Duration::from_secs(10),
        )
        .await?;
    println!(
        "{} {} -> {:?} (post data: {:?})",
        api.method, api.url, api.status, api.post_data
    );
    if let ResponseBody::Text(body) = capture.response_body(&api.request_id).await? {
        println!("API response body: {body}");
    }

    for request in capture.stop().await {
        println!(
            "{:>4} {:<8} {:?} {}",
            request.method,
            request.resource_type.as_deref().unwrap_or("?"),
            request.status,
            request.url
        );
    }
    browser.close().await
}
