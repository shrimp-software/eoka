use serde_json::{json, Value};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

const SIZES: [usize; 3] = [256, 18_000, 32_768];
const MAX_HEADERS: usize = 16_384;
const MAX_BODY: usize = 32_768;

struct BinaryPostFixture {
    url: String,
    task: JoinHandle<()>,
}

impl BinaryPostFixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            // Dropping this JoinSet aborts every client when the fixture is dropped.
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept(), if clients.len() < 16 => {
                        let (socket, _) = accepted.unwrap();
                        clients.spawn(async move {
                            let _ = tokio::time::timeout(
                                Duration::from_secs(5),
                                serve(socket),
                            )
                            .await;
                        });
                    }
                    completed = clients.join_next(), if !clients.is_empty() => {
                        completed.unwrap().unwrap();
                    }
                }
            }
        });
        Self { url, task }
    }
}

impl Drop for BinaryPostFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn invalid_request(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

async fn serve(mut socket: TcpStream) -> io::Result<()> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let header_end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            if end + 4 > MAX_HEADERS {
                return Err(invalid_request("request headers too large"));
            }
            break end + 4;
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(invalid_request("request headers too large"));
        }
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            return Err(invalid_request("incomplete request headers"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| invalid_request("invalid request headers"))?;
    let mut lines = headers.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split_whitespace();
    let method = request.next().unwrap_or_default();
    let target = request.next().unwrap_or_default();
    let mut content_length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid_request("invalid header"))?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(invalid_request("fixture requires Content-Length"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(invalid_request("duplicate Content-Length"));
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| invalid_request("invalid Content-Length"))?,
            );
        }
    }
    let (status, content_type, response) = if method == "GET" {
        (
            "200 OK",
            "text/html",
            b"<!doctype html><title>POST fixture</title>".to_vec(),
        )
    } else if method == "POST" {
        let (kind, size) = target
            .strip_prefix("/post/")
            .and_then(|path| path.split_once('/'))
            .ok_or_else(|| invalid_request("invalid POST target"))?;
        let size = size
            .parse::<usize>()
            .map_err(|_| invalid_request("invalid payload size"))?;
        if !SIZES.contains(&size) || !matches!(kind, "text" | "binary") {
            return Err(invalid_request("unexpected payload case"));
        }
        let content_length = content_length
            .filter(|length| *length <= MAX_BODY)
            .ok_or_else(|| invalid_request("missing or oversized Content-Length"))?;
        // Own the path-derived values before extending the request buffer.
        let expected = payload(kind, size);
        let mut body = bytes.split_off(header_end);
        if body.len() > content_length {
            return Err(invalid_request("body exceeds Content-Length"));
        }
        let received = body.len();
        body.resize(content_length, 0);
        socket.read_exact(&mut body[received..]).await?;
        let status = if body == expected {
            "200 OK"
        } else {
            "422 Unprocessable Content"
        };
        // Echo the actual bytes, not the expected body, for an independent JS comparison.
        (status, "application/octet-stream", body)
    } else {
        return Err(invalid_request("unexpected method"));
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    );
    socket.write_all(headers.as_bytes()).await?;
    socket.write_all(&response).await?;
    socket.shutdown().await
}

fn payload(kind: &str, size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| {
            if kind == "text" {
                ((index * 17 + 31) % 95 + 32) as u8
            } else {
                // Every 256 bytes includes all byte values, including invalid UTF-8.
                ((index * 73 + 19) % 256) as u8
            }
        })
        .collect()
}

const POST_SCRIPT: &str = r#"async ({ api, kind, size }) => {
    const expected = Uint8Array.from({ length: size }, (_, index) =>
        kind === 'text' ? (index * 17 + 31) % 95 + 32 : (index * 73 + 19) % 256);
    const body = kind === 'text'
        ? Array.from(expected, byte => String.fromCharCode(byte)).join('')
        : expected;
    const url = `/post/${kind}/${size}`;
    let status, received;
    if (api === 'fetch') {
        const controller = new AbortController();
        const deadline = setTimeout(() => controller.abort(), 3000);
        try {
            const response = await fetch(url, {
                method: 'POST', body, signal: controller.signal,
            });
            status = response.status;
            received = new Uint8Array(await response.arrayBuffer());
        } finally {
            clearTimeout(deadline);
        }
    } else {
        const response = await new Promise((resolve, reject) => {
            const xhr = new XMLHttpRequest();
            xhr.open('POST', url);
            xhr.responseType = 'arraybuffer';
            xhr.timeout = 3000;
            xhr.onload = () => resolve({ status: xhr.status, body: xhr.response });
            xhr.onerror = () => reject(new Error('XHR network error'));
            xhr.ontimeout = () => reject(new Error('XHR deadline exceeded'));
            xhr.onabort = () => reject(new Error('XHR aborted'));
            xhr.send(body);
        });
        status = response.status;
        received = new Uint8Array(response.body);
    }
    return {
        status,
        length: received.length,
        matches: received.length === expected.length &&
            received.every((byte, index) => byte === expected[index]),
    };
}"#;

#[tokio::test]
#[ignore = "requires Chrome; local binary POST integrity fixture"]
async fn interception_preserves_text_and_binary_posts_for_xhr_and_fetch() {
    let fixture = BinaryPostFixture::start().await;
    for strip_x_client_data in [false, true] {
        let browser = eoka::Browser::launch_with(|config| {
            config.patch_binary = false;
            config.strip_x_client_data = strip_x_client_data;
            config.cdp_timeout = 5;
        })
        .await
        .unwrap();
        let outcome = async {
            let page = browser
                .new_page(&fixture.url)
                .await
                .map_err(|error| format!("opening fixture: {error}"))?;
            for api in ["xhr", "fetch"] {
                for kind in ["text", "binary"] {
                    for size in SIZES {
                        let case = json!({ "api": api, "kind": kind, "size": size });
                        let script = format!("({POST_SCRIPT})({case})");
                        let result: Value = page.evaluate(&script).await.map_err(|error| {
                            format!("strip={strip_x_client_data}, {case}: {error}")
                        })?;
                        let expected = json!({ "status": 200, "length": size, "matches": true });
                        if result != expected {
                            return Err(format!(
                                "strip={strip_x_client_data}, {case}: expected {expected}, got {result}"
                            ));
                        }
                    }
                }
            }
            Ok::<(), String>(())
        }
        .await;
        // A failing request must not leave Chrome running while the test panics.
        let closed = browser.close().await;
        assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
        closed.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires Chrome; JavaScript UTF-16 result regression"]
async fn javascript_lone_surrogates_return_without_stalling_the_transport() {
    let browser = eoka::Browser::launch_with(|config| {
        config.patch_binary = false;
        config.cdp_timeout = 5;
    })
    .await
    .unwrap();
    let outcome = async {
        let page = browser.new_page("about:blank").await?;
        let value: Value = page
            .evaluate(
                r#"({
                    high: "\uD800",
                    low: "\uDC00",
                    paired: "\uD834\uDD1E",
                    literal: "\\uD800",
                    nested: ["before\uD800after", "\uD800A"]
                })"#,
            )
            .await?;
        let next: i32 = page.evaluate("6 * 7").await?;
        Ok::<_, eoka::Error>((value, next))
    }
    .await;
    let closed = browser.close().await;
    let (value, next) = outcome.unwrap();
    assert_eq!(
        value,
        json!({
            "high": "\u{fffd}",
            "low": "\u{fffd}",
            "paired": "\u{1d11e}",
            "literal": "\\uD800",
            "nested": ["before\u{fffd}after", "\u{fffd}A"]
        })
    );
    assert_eq!(next, 42);
    closed.unwrap();
}
