//! HTTP discovery for Chrome's DevTools endpoint.
//!
//! Chrome started with `--remote-debugging-port=N` exposes a small HTTP API:
//! - `GET /json/version` — browser-level info, including `webSocketDebuggerUrl`
//! - `GET /json` — list of page targets, each with its own `webSocketDebuggerUrl`
//!
//! Chrome rejects requests whose `Host` header isn't `localhost`/`127.0.0.1` as
//! anti-DNS-rebinding. We always send `Host: localhost` to stay compatible.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{Error, Result};

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Read the pid out of `<user_data_dir>/SingletonLock`. Chrome writes this
/// as a symlink whose target is `<hostname>-<pid>` — a POSIX-only mechanism
/// (Windows Chrome uses a named mutex instead), so this is `None` outside
/// Unix. Also `None` when no lock is present (nothing else has this profile
/// open).
pub(crate) fn read_singleton_lock(user_data_dir: &std::path::Path) -> Option<u32> {
    #[cfg(unix)]
    {
        let target = std::fs::read_link(user_data_dir.join("SingletonLock")).ok()?;
        target.to_str()?.rsplit('-').next()?.parse().ok()
    }
    #[cfg(not(unix))]
    {
        let _ = user_data_dir;
        None
    }
}

/// Whether `pid` refers to a live process.
pub(crate) fn pid_is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // No /proc to stat outside Linux; shell out to `kill -0` rather than
        // pulling in a libc/nix dependency for one syscall. A no-op (and
        // `false`) on platforms without a `kill` binary, e.g. Windows.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

/// Read the port Chrome is listening on from `<user_data_dir>/DevToolsActivePort`,
/// written when Chrome is launched with `--remote-debugging-port` (including
/// `=0`, which is how Eoka always launches). Returns `None` if the file is
/// missing (that Chrome wasn't given a debug port).
pub(crate) fn read_devtools_active_port(user_data_dir: &std::path::Path) -> Option<u16> {
    let contents = std::fs::read_to_string(user_data_dir.join("DevToolsActivePort")).ok()?;
    contents.lines().next()?.trim().parse().ok()
}

/// Read a process's argv. Used to detect a headless/headed mismatch before
/// attaching to a Chrome we didn't spawn ourselves. `None` if it can't be
/// determined (unsupported platform, or the process/tool is unreadable).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn read_process_argv(pid: u32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
        Some(
            raw.split(|&b| b == 0)
                .filter(|piece| !piece.is_empty())
                .map(|piece| String::from_utf8_lossy(piece).into_owned())
                .collect(),
        )
    }
    #[cfg(target_os = "macos")]
    {
        // No /proc/<pid>/cmdline on macOS; shell out to `ps` instead.
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn read_process_argv(_: u32) -> Option<Vec<String>> {
    None
}

/// One entry from `GET /json` — a single browsing target (tab, iframe, worker).
#[derive(Debug, Clone, Deserialize)]
pub struct PageTarget {
    /// Target ID.
    pub id: String,
    /// Target title.
    #[serde(default)]
    pub title: String,
    /// Target type (e.g. "page", "iframe", "worker").
    #[serde(rename = "type", default)]
    pub kind: String,
    /// URL loaded in the target.
    #[serde(default)]
    pub url: String,
    /// Per-target DevTools WebSocket URL.
    #[serde(rename = "webSocketDebuggerUrl", default)]
    pub web_socket_debugger_url: String,
}

/// Resolve the browser-level DevTools WebSocket URL for a running Chrome.
///
/// ```no_run
/// let url = eoka::cdp::discover::discover_browser_ws("127.0.0.1", 9222)?;
/// # Ok::<(), eoka::Error>(())
/// ```
pub fn discover_browser_ws(host: &str, port: u16) -> Result<String> {
    let body = http_get(host, port, "/json/version")?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::transport(format!("Invalid JSON from /json/version: {}", e)))?;
    let raw = value
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::transport("No webSocketDebuggerUrl in /json/version"))?;
    Ok(rewrite_ws_url_host(raw, host, port))
}

/// List all current targets (tabs, workers, iframes) for a running Chrome.
pub fn discover_pages(host: &str, port: u16) -> Result<Vec<PageTarget>> {
    let body = http_get(host, port, "/json")?;
    let mut targets: Vec<PageTarget> = serde_json::from_str(&body)
        .map_err(|e| Error::transport(format!("Invalid JSON from /json: {}", e)))?;
    for target in &mut targets {
        target.web_socket_debugger_url =
            rewrite_ws_url_host(&target.web_socket_debugger_url, host, port);
    }
    Ok(targets)
}

/// Chrome builds `webSocketDebuggerUrl` from the request's `Host` header
/// rather than the address it's actually listening on. We send a bare
/// `Host: localhost` (no port) to satisfy Chrome's anti-DNS-rebinding check,
/// so the URL Chrome echoes back is missing the port (`ws://localhost/...`).
/// Keep the path Chrome returned but rebuild the host/port from what we
/// actually connected to.
fn rewrite_ws_url_host(raw: &str, host: &str, port: u16) -> String {
    let after_scheme = raw.split("://").nth(1).unwrap_or(raw);
    let path = after_scheme
        .find('/')
        .map(|i| &after_scheme[i..])
        .unwrap_or("");
    format!("ws://{}:{}{}", host, port, path)
}

/// Probe `start..=end` on `127.0.0.1` and return the first port whose Chrome
/// answers `/json/version`, alongside its WebSocket URL.
pub fn auto_connect(start: u16, end: u16) -> Option<(u16, String)> {
    (start..=end).find_map(|port| {
        discover_browser_ws("127.0.0.1", port)
            .ok()
            .map(|u| (port, u))
    })
}

/// Minimal blocking HTTP/1.1 GET.
///
/// Chrome's DevTools HTTP server doesn't honor a `Connection: close` request
/// header — it replies with `Content-Length` and keeps the connection open
/// for keep-alive regardless. Reading until EOF would therefore always block
/// for the full read timeout and then discard an already-complete response,
/// so this reads exactly `Content-Length` bytes past the header instead.
/// Response progress does not renew the deadline: a slow-drip peer must not
/// keep discovery (or the blocking launch worker) alive indefinitely.
fn http_get(host: &str, port: u16, path: &str) -> Result<String> {
    let addr = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect(&addr)
        .map_err(|e| Error::transport_io(format!("Failed to connect to {}", addr), e))?;
    let deadline = Instant::now() + HTTP_TIMEOUT;
    stream
        .set_write_timeout(Some(HTTP_TIMEOUT))
        .map_err(|e| Error::transport_io("set_write_timeout", e))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        path
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| Error::transport_io("HTTP write", e))?;

    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_double_crlf(&buf) {
            break pos;
        }
        if read_more(&mut stream, &mut buf, &mut chunk, deadline)? == 0 {
            return Err(Error::transport(
                "Chrome closed the connection before headers",
            ));
        }
    };

    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let status_line = header_text.lines().next().unwrap_or("");
    if !(status_line.starts_with("HTTP/1.1 200") || status_line.starts_with("HTTP/1.0 200")) {
        return Err(Error::transport(format!(
            "HTTP {} from Chrome at {}: {}",
            path, addr, status_line
        )));
    }

    let content_length = header_text.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("content-length:")?
            .trim()
            .parse::<usize>()
            .ok()
    });
    let body_start = header_end + 4;

    if let Some(content_length) = content_length {
        let body_end = body_start + content_length;
        while buf.len() < body_end {
            if read_more(&mut stream, &mut buf, &mut chunk, deadline)? == 0 {
                return Err(Error::transport(
                    "Chrome closed the connection before sending the full response body",
                ));
            }
        }
        Ok(String::from_utf8_lossy(&buf[body_start..body_end]).into_owned())
    } else {
        // No Content-Length (unexpected for this endpoint) — fall back to
        // reading until the peer actually closes.
        while read_more(&mut stream, &mut buf, &mut chunk, deadline)? > 0 {}
        Ok(String::from_utf8_lossy(&buf[body_start..]).into_owned())
    }
}

/// Read one chunk from `stream` into `buf`, returning the number of bytes
/// read (`0` on EOF).
fn read_more(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    chunk: &mut [u8],
    deadline: Instant,
) -> Result<usize> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::Timeout(
            "HTTP discovery response exceeded its deadline".into(),
        ));
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|e| Error::transport_io("set_read_timeout", e))?;
    let n = stream.read(chunk).map_err(|error| {
        if matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            Error::Timeout("HTTP discovery response exceeded its deadline".into())
        } else {
            Error::transport_io("HTTP read", error)
        }
    })?;
    buf.extend_from_slice(&chunk[..n]);
    Ok(n)
}

/// Find the byte offset of the first `\r\n\r\n` in `buf`, if any.
fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

#[cfg(test)]
mod url_rewrite_tests {
    use super::rewrite_ws_url_host;

    #[test]
    fn replaces_portless_host_chrome_echoes_back() {
        let raw = "ws://localhost/devtools/browser/1a65aa96-763b-40ff-bbdf-925f53bc4a66";
        assert_eq!(
            rewrite_ws_url_host(raw, "127.0.0.1", 41015),
            "ws://127.0.0.1:41015/devtools/browser/1a65aa96-763b-40ff-bbdf-925f53bc4a66"
        );
    }

    #[test]
    fn replaces_host_that_already_has_a_port() {
        let raw = "ws://127.0.0.1:9222/devtools/browser/GUID";
        assert_eq!(
            rewrite_ws_url_host(raw, "127.0.0.1", 9222),
            "ws://127.0.0.1:9222/devtools/browser/GUID"
        );
    }

    #[test]
    fn handles_missing_path() {
        assert_eq!(
            rewrite_ws_url_host("ws://localhost", "127.0.0.1", 9222),
            "ws://127.0.0.1:9222"
        );
    }
}

#[cfg(test)]
mod http_get_regression_tests {
    use super::*;
    use std::net::TcpListener;

    /// Spawn a fake DevTools HTTP server that replies once and then holds
    /// the connection open (never sends EOF), regardless of what the
    /// request asked for. This is exactly how real Chrome behaves — it
    /// ignores our `Connection: close` request header.
    fn spawn_fake_devtools_server(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();

            // Never close: a naive "read until EOF" client would hang here
            // for the full read timeout instead of stopping at Content-Length.
            std::thread::sleep(Duration::from_secs(30));
        });
        port
    }

    /// Regression test: this exact scenario (Chrome 152, `--headless=new`)
    /// used to make `discover_browser_ws` block for the full `HTTP_TIMEOUT`
    /// and then fail, because `http_get` read until EOF instead of stopping
    /// at `Content-Length`.
    #[test]
    fn discover_browser_ws_does_not_hang_on_keep_alive_server() {
        let port = spawn_fake_devtools_server(
            r#"{"webSocketDebuggerUrl":"ws://localhost/devtools/browser/regression-guid"}"#,
        );

        let start = std::time::Instant::now();
        let ws_url = discover_browser_ws("127.0.0.1", port).expect("should succeed without EOF");
        let elapsed = start.elapsed();

        assert!(
            elapsed < HTTP_TIMEOUT,
            "took {:?}, looks like it waited for connection close instead of Content-Length",
            elapsed
        );
        assert_eq!(
            ws_url,
            format!("ws://127.0.0.1:{}/devtools/browser/regression-guid", port)
        );
    }

    /// Same regression, exercised through `discover_pages` (`GET /json`,
    /// a JSON array rather than a single object).
    #[test]
    fn discover_pages_does_not_hang_on_keep_alive_server() {
        let port = spawn_fake_devtools_server(
            r#"[{"id":"1","type":"page","url":"about:blank","webSocketDebuggerUrl":"ws://localhost/devtools/page/regression-guid"}]"#,
        );

        let start = std::time::Instant::now();
        let pages = discover_pages("127.0.0.1", port).expect("should succeed without EOF");
        assert!(start.elapsed() < HTTP_TIMEOUT, "took {:?}", start.elapsed());

        assert_eq!(pages.len(), 1);
        assert_eq!(
            pages[0].web_socket_debugger_url,
            format!("ws://127.0.0.1:{}/devtools/page/regression-guid", port)
        );
    }

    fn assert_slow_response_has_one_deadline(prefix: &'static [u8]) {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut first_byte = [0];
            stream.read_exact(&mut first_byte).unwrap();
            stream.write_all(prefix).unwrap();
            while !stopped.load(Ordering::Acquire) {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (sent, received) = mpsc::channel();
        let client = std::thread::spawn(move || {
            let _ = sent.send(http_get("127.0.0.1", port, "/json/version"));
        });
        let result = received.recv_timeout(HTTP_TIMEOUT + Duration::from_secs(1));
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        client.join().unwrap();
        assert!(
            matches!(result, Ok(Err(Error::Timeout(_)))),
            "continuous progress must not reset the HTTP discovery deadline: {result:?}"
        );
    }

    #[test]
    fn discovery_bounds_slow_drip_headers() {
        assert_slow_response_has_one_deadline(b"HTTP/1.1 200 OK\r\nX-Padding: ");
    }

    #[test]
    fn discovery_bounds_slow_drip_content_length_body() {
        assert_slow_response_has_one_deadline(b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n");
    }

    #[test]
    fn discovery_bounds_slow_drip_eof_body() {
        assert_slow_response_has_one_deadline(b"HTTP/1.1 200 OK\r\n\r\n");
    }
}

#[cfg(all(test, unix))]
mod singleton_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn temp_profile_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "eoka-discover-test-{}-{}-{}",
            std::process::id(),
            name,
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_singleton_lock_parses_trailing_pid() {
        let dir = temp_profile_dir("lock");
        symlink("some-host-54321", dir.join("SingletonLock")).unwrap();
        assert_eq!(read_singleton_lock(&dir), Some(54321));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_singleton_lock_missing_is_none() {
        let dir = temp_profile_dir("no-lock");
        assert!(read_singleton_lock(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pid_is_alive_true_for_self() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn read_devtools_active_port_parses_first_line() {
        let dir = temp_profile_dir("port");
        std::fs::write(
            dir.join("DevToolsActivePort"),
            "37469\n/devtools/browser/00000000-0000-0000-0000-000000000000\n",
        )
        .unwrap();
        assert_eq!(read_devtools_active_port(&dir), Some(37469));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_devtools_active_port_missing_is_none() {
        let dir = temp_profile_dir("no-port");
        assert!(read_devtools_active_port(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_process_argv_reads_own_cmdline() {
        let argv = read_process_argv(std::process::id()).expect("own /proc entry must exist");
        assert!(!argv.is_empty());
    }
}
