//! Chrome child ownership, launch and DevTools endpoint discovery.

use crate::{Error, Result};
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Terminate and reap a managed Chrome child without treating an already
/// exited process as an error.
pub(crate) fn stop_child(child: &mut Child) -> std::io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    if let Err(kill_error) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(kill_error);
        }
        return Ok(());
    }

    child.wait()?;
    Ok(())
}

/// Own a Chrome child until it is handed to `Transport`. If the async
/// WebSocket connection future is cancelled, dropping this guard still kills
/// and reaps the process.
pub(super) struct ChildCleanupGuard {
    child: Option<Child>,
}

impl ChildCleanupGuard {
    pub(super) fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub(super) fn take(&mut self) -> Result<Child> {
        self.child
            .take()
            .ok_or_else(|| Error::transport("Chrome child ownership was already transferred"))
    }
}

impl Drop for ChildCleanupGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = stop_child(&mut child) {
                tracing::warn!("Failed to clean up an unclaimed Chrome child: {}", error);
            }
        }
    }
}

/// Launch Chrome and get the WebSocket debugging URL
pub fn launch_chrome(path: &std::path::Path, args: &[String]) -> Result<(Child, String)> {
    launch_chrome_impl(path, args, None, false, None, None)
}

pub(crate) fn launch_chrome_with_profile_dir(
    path: &std::path::Path,
    args: &[String],
    profile_dir: &Path,
    live_session: bool,
    runtime: &Path,
    cancelled: &AtomicBool,
) -> Result<(Child, String)> {
    launch_chrome_impl(
        path,
        args,
        Some(profile_dir),
        live_session,
        Some(runtime),
        Some(cancelled),
    )
}

fn debugging_port_argument(args: &[String], live_session: bool) -> Result<Option<String>> {
    if args
        .iter()
        .any(|arg| arg == "--remote-debugging-port" || arg.starts_with("--remote-debugging-port="))
    {
        return Ok(None);
    }
    let port = if live_session {
        std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port()
    } else {
        0
    };
    Ok(Some(format!("--remote-debugging-port={port}")))
}

fn launch_chrome_impl(
    path: &std::path::Path,
    args: &[String],
    profile_dir: Option<&Path>,
    live_session: bool,
    runtime: Option<&Path>,
    cancelled: Option<&AtomicBool>,
) -> Result<(Child, String)> {
    use std::process::Command;

    let mut cmd = Command::new(path);
    cmd.args(args);
    if let Some(runtime) = runtime {
        cmd.env("TMPDIR", runtime)
            .env("TMP", runtime)
            .env("TEMP", runtime);
    }
    let debugging_argument = debugging_port_argument(args, live_session)?;
    if let Some(argument) = &debugging_argument {
        cmd.arg(argument);
    }
    let effective_port = debugging_argument.as_deref().or_else(|| {
        args.iter()
            .rev()
            .find(|arg| {
                arg.as_str() == "--remote-debugging-port"
                    || arg.starts_with("--remote-debugging-port=")
            })
            .map(String::as_str)
    });
    let fallback_profile_dir = if effective_port == Some("--remote-debugging-port=0") {
        profile_dir
    } else {
        None
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped()); // We need stderr to get the DevTools URL

    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
        return Err(Error::Launch("Browser launch was cancelled".into()));
    }
    if let Some(profile) = fallback_profile_dir {
        // Chrome leaves this file behind on exit. Discovery must only consume
        // the endpoint written by the process we are about to spawn.
        if let Err(error) = std::fs::remove_file(profile.join("DevToolsActivePort")) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(Error::Launch(format!(
                    "Failed to remove stale DevToolsActivePort before launch: {error}"
                )));
            }
        }
    }
    let mut guard = ChildCleanupGuard::new(
        cmd.spawn()
            .map_err(|e| Error::Launch(format!("Failed to launch Chrome: {}", e)))?,
    );
    let child = guard
        .child
        .as_mut()
        .expect("new child guard owns its process");
    let stderr = child
        .stderr
        .take()
        .ok_or(Error::Launch("No stderr from Chrome".into()))?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drain_chrome_stderr(BufReader::new(stderr), tx));

    let ws_url = match wait_for_chrome_devtools_url(child, &rx, fallback_profile_dir, cancelled) {
        Ok(url) => url,
        Err(error) => {
            return match stop_child(child) {
                Ok(()) => {
                    guard.child = None;
                    Err(error)
                }
                Err(cleanup) => Err(Error::Launch(format!(
                    "{}; Chrome cleanup failed: {}",
                    error, cleanup
                ))),
            };
        }
    };

    tracing::info!("Chrome DevTools URL: {}", ws_url);

    Ok((guard.take()?, ws_url))
}

fn drain_chrome_stderr(
    reader: impl BufRead,
    sender: std::sync::mpsc::Sender<std::result::Result<String, String>>,
) {
    let mut sender = Some(sender);
    let mut stderr_tail = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                if let Some(sender) = sender.take() {
                    let _ = sender.send(Err(format!("failed to read Chrome stderr: {error}")));
                } else {
                    tracing::warn!("Chrome stderr reader failed after startup: {error}");
                }
                return;
            }
        };
        tracing::trace!("Chrome stderr: {}", line);
        if sender.is_none() {
            continue;
        }
        if let Some((_, endpoint)) = line.split_once("DevTools listening on ws://") {
            let _ = sender
                .take()
                .unwrap()
                .send(Ok(format!("ws://{}", endpoint.trim())));
            stderr_tail = Vec::new();
        } else {
            stderr_tail.push(line);
            if stderr_tail.len() > 20 {
                stderr_tail.remove(0);
            }
        }
    }
    if let Some(sender) = sender {
        let message = if stderr_tail.is_empty() {
            "Chrome exited before printing DevTools URL and produced no stderr".to_string()
        } else {
            format!(
                "Chrome exited before printing DevTools URL. stderr:\n{}",
                stderr_tail.join("\n")
            )
        };
        let _ = sender.send(Err(message));
    }
}

fn wait_for_chrome_devtools_url(
    child: &mut Child,
    rx: &std::sync::mpsc::Receiver<std::result::Result<String, String>>,
    profile_dir: Option<&Path>,
    cancelled: Option<&AtomicBool>,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
            return Err(Error::Launch("Browser launch was cancelled".into()));
        }
        match rx.try_recv() {
            Ok(Ok(url)) => return Ok(url),
            Ok(Err(stderr)) => {
                let status = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|status| status.to_string())
                    .unwrap_or_else(|| "still running".to_string());
                return Err(Error::Launch(format!(
                    "Failed waiting for Chrome DevTools URL: process {}; {}",
                    status, stderr
                )));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(Error::Launch(
                    "Failed waiting for Chrome DevTools URL: stderr reader exited unexpectedly"
                        .into(),
                ));
            }
        }

        if let Some(dir) = profile_dir {
            if let Some(port) = crate::cdp::discover::read_devtools_active_port(dir) {
                return crate::cdp::discover::discover_browser_ws("127.0.0.1", port).map_err(
                    |error| {
                        Error::Launch(format!(
                            "Chrome wrote DevToolsActivePort ({}) but discovery failed: {}",
                            port, error
                        ))
                    },
                );
            }
        }

        if let Ok(Some(status)) = child.try_wait() {
            let stderr = rx
                .recv_timeout(Duration::from_millis(250))
                .ok()
                .and_then(|result| result.err())
                .unwrap_or_else(|| "Chrome exited before printing DevTools URL".to_string());
            return Err(Error::Launch(format!(
                "Failed waiting for Chrome DevTools URL: process {}; {}",
                status, stderr
            )));
        }

        if Instant::now() >= deadline {
            return Err(Error::Launch(
                "Failed waiting for Chrome DevTools URL after 30s".into(),
            ));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn chrome_version_output(
    path: &Path,
    runtime: &Path,
    cancelled: &AtomicBool,
) -> Option<Vec<u8>> {
    probe_version_output(path, runtime, cancelled, Duration::from_secs(30))
}

fn probe_version_output(
    path: &Path,
    runtime: &Path,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Option<Vec<u8>> {
    use std::io::ErrorKind;
    #[cfg(not(windows))]
    use std::io::Read;
    if cancelled.load(Ordering::Acquire) {
        return None;
    }
    let child = std::process::Command::new(path)
        .env("TMPDIR", runtime)
        .env("TMP", runtime)
        .env("TEMP", runtime)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut guard = ChildCleanupGuard::new(child);
    let mut stdout = guard.child.as_mut()?.stdout.take()?;
    // A descendant may inherit stdout after the probe child exits. Never hand
    // this pipe to a detached blocking reader: cancellation must close our end.
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stdout.as_raw_fd();
        // SAFETY: stdout owns this descriptor, and no other reader uses it.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return None;
        }
    }
    let mut bytes = [0; 4096];
    let mut used = 0;
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            return None;
        }
        #[cfg(windows)]
        let output = read_available_version_stdout(&mut stdout, &mut bytes[used..]);
        #[cfg(not(windows))]
        let output = stdout.read(&mut bytes[used..]);
        match output {
            Ok(0) => return Some(bytes[..used].to_vec()),
            Ok(n) => {
                used += n;
                if used == bytes.len() {
                    return Some(bytes.to_vec());
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

#[cfg(windows)]
fn read_available_version_stdout(
    stdout: &mut std::process::ChildStdout,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    use std::{ffi::c_void, io::Read, os::windows::io::AsRawHandle};
    #[link(name = "kernel32")]
    extern "system" {
        fn PeekNamedPipe(
            pipe: *mut c_void,
            buffer: *mut c_void,
            buffer_size: u32,
            bytes_read: *mut u32,
            bytes_available: *mut u32,
            bytes_left: *mut u32,
        ) -> i32;
    }
    let mut available = 0;
    // SAFETY: stdout owns a pipe handle; only the valid available-byte output
    // pointer is used. No read occurs until bytes are available to this sole reader.
    let ok = unsafe {
        PeekNamedPipe(
            stdout.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(109) {
            // ERROR_BROKEN_PIPE: EOF.
            Ok(0)
        } else {
            Err(error)
        };
    }
    if available == 0 {
        return Err(std::io::ErrorKind::WouldBlock.into());
    }
    let count = buffer.len().min(available as usize);
    stdout.read(&mut buffer[..count])
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct ProbeFixture {
        runtime: crate::cdp::transport::RuntimeDirectory,
        cancelled: std::sync::Arc<AtomicBool>,
        finished: Option<std::sync::mpsc::Receiver<()>>,
        cleaned_up: std::sync::Arc<AtomicBool>,
    }

    #[cfg(unix)]
    impl ProbeFixture {
        fn new(body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let fixture = Self {
                runtime: crate::cdp::transport::RuntimeDirectory::create(None).unwrap(),
                cancelled: std::sync::Arc::new(AtomicBool::new(false)),
                finished: None,
                cleaned_up: std::sync::Arc::new(AtomicBool::new(false)),
            };
            let script = fixture.runtime.path().join("version-fixture");
            std::fs::write(&script, body).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            fixture
        }

        fn start(&mut self, timeout: Duration) -> tokio::task::JoinHandle<Option<Vec<u8>>> {
            let script = self.runtime.path().join("version-fixture");
            let directory = self.runtime.path().to_owned();
            let cancelled = self.cancelled.clone();
            let (finished, receiver) = std::sync::mpsc::channel();
            self.finished = Some(receiver);
            tokio::task::spawn_blocking(move || {
                let output = probe_version_output(&script, &directory, &cancelled, timeout);
                let _ = finished.send(());
                output
            })
        }

        fn release(&self) {
            self.cancelled.store(true, Ordering::Release);
            if self.runtime.path().is_dir() {
                let _ = std::fs::write(self.runtime.path().join("release"), []);
            }
        }
    }

    #[cfg(unix)]
    impl Drop for ProbeFixture {
        fn drop(&mut self) {
            // This guard owns the directory, so it remains available while both
            // the blocking probe and its stdout-inheriting descendant stop.
            self.release();
            let deadline = Instant::now() + Duration::from_secs(3);
            let stopped = self.finished.take().is_none_or(|finished| {
                !matches!(
                    finished.recv_timeout(deadline.saturating_duration_since(Instant::now())),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                )
            });
            let started = self.runtime.path().join("descendant-started");
            let exited = self.runtime.path().join("descendant-exited");
            while started.exists() && !exited.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.cleaned_up.store(
                stopped && (!started.exists() || exited.exists()),
                Ordering::Release,
            );
            // If startup raced this bounded cleanup, removing the directory
            // also releases every shell wait loop; no orphan waits forever.
        }
    }

    #[cfg(unix)]
    const INHERITED_STDOUT_FIXTURE: &str = r#"#!/bin/sh
(
    trap '' PIPE
    trap 'printf exited > "$TMPDIR/descendant-exited" 2>/dev/null' EXIT
    [ -d "$TMPDIR" ] || exit
    printf started > "$TMPDIR/descendant-started"
    while [ -d "$TMPDIR" ] && [ ! -e "$TMPDIR/release" ]; do sleep 0.02; done
    [ -d "$TMPDIR" ] || exit
    printf x 2>/dev/null
    printf '%s' "$?" > "$TMPDIR/write-status"
) &
while [ -d "$TMPDIR" ] && [ -e "$TMPDIR/hold-ready" ] && [ ! -e "$TMPDIR/release" ]; do
    sleep 0.02
done
if [ -d "$TMPDIR" ] && [ ! -e "$TMPDIR/release" ]; then
    printf ready > "$TMPDIR/ready"
fi
wait
"#;

    #[cfg(unix)]
    async fn fixture_file(path: &Path) -> String {
        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if !contents.is_empty() {
                    return contents;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    async fn assert_probe_closes_inherited_stdout(cancel: bool) {
        let mut fixture = ProbeFixture::new(INHERITED_STDOUT_FIXTURE);
        let probe = fixture.start(Duration::from_secs(if cancel { 30 } else { 1 }));
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture_file(&fixture.runtime.path().join("ready")),
        )
        .await
        .expect("fixture must start its stdout-inheriting descendant");
        if cancel {
            fixture.cancelled.store(true, Ordering::Release);
        }
        let result = tokio::time::timeout(Duration::from_secs(3), probe).await;
        // Release even a regressed probe before asserting its result.
        fixture.release();
        let status = tokio::time::timeout(
            Duration::from_secs(3),
            fixture_file(&fixture.runtime.path().join("write-status")),
        )
        .await
        .expect("descendant must report its pipe write and exit");
        assert!(result
            .expect("cancelled or expired probe must finish")
            .unwrap()
            .is_none());
        assert_ne!(
            status, "0",
            "probe left a reader blocked on inherited stdout"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_version_probe_closes_stdout_inherited_by_descendant() {
        assert_probe_closes_inherited_stdout(true).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn expired_version_probe_closes_stdout_inherited_by_descendant() {
        assert_probe_closes_inherited_stdout(false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn version_probe_fixture_cleans_up_when_readiness_fails() {
        for abort in [false, true] {
            let mut fixture = ProbeFixture::new(INHERITED_STDOUT_FIXTURE);
            let directory = fixture.runtime.path().to_owned();
            std::fs::write(directory.join("hold-ready"), []).unwrap();
            let probe = fixture.start(Duration::from_secs(30));
            tokio::time::timeout(
                Duration::from_secs(3),
                fixture_file(&directory.join("descendant-started")),
            )
            .await
            .expect("descendant must start before readiness is interrupted");
            let cleaned_up = fixture.cleaned_up.clone();
            let cancelled = fixture.cancelled.clone();
            let (waiting, ready) = tokio::sync::oneshot::channel();
            let startup = tokio::spawn(async move {
                let guard = fixture;
                let _ = waiting.send(());
                tokio::time::timeout(
                    Duration::from_millis(50),
                    fixture_file(&guard.runtime.path().join("ready")),
                )
                .await
                .expect("deliberately held fixture readiness");
            });
            ready.await.unwrap();
            if abort {
                startup.abort();
            }
            let failure = startup.await.unwrap_err();
            assert_eq!(failure.is_cancelled(), abort);
            assert_eq!(failure.is_panic(), !abort);
            assert!(cancelled.load(Ordering::Acquire));
            assert!(
                cleaned_up.load(Ordering::Acquire),
                "cleanup must finish both the probe and its descendant"
            );
            assert!(!directory.exists(), "fixture runtime must be removed");
            let _ = tokio::time::timeout(Duration::from_secs(3), probe)
                .await
                .expect("readiness failure must not leave a blocked probe")
                .unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn version_probe_caps_continuous_output_without_waiting_for_exit() {
        let mut fixture = ProbeFixture::new(
            "#!/bin/sh\nwhile [ -d \"$TMPDIR\" ]; do printf '0123456789abcdef'; done\n",
        );
        let probe = fixture.start(Duration::from_secs(30));
        let result = tokio::time::timeout(Duration::from_secs(3), probe).await;
        fixture.release();
        assert_eq!(
            result
                .expect("output cap must finish the probe before the child exits")
                .unwrap(),
            Some(b"0123456789abcdef".repeat(256))
        );
    }

    #[test]
    fn chrome_stderr_is_drained_after_the_devtools_url() {
        let input = b"startup\nDevTools listening on ws://127.0.0.1:9333/devtools/browser/test\nGPU log after startup\n";
        let mut reader = std::io::Cursor::new(input);
        let (sender, receiver) = std::sync::mpsc::channel();
        drain_chrome_stderr(&mut reader, sender);
        assert_eq!(
            receiver.recv().unwrap().unwrap(),
            "ws://127.0.0.1:9333/devtools/browser/test"
        );
        assert_eq!(reader.position(), input.len() as u64);
        assert!(receiver.try_recv().is_err());
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(receiver);
        let mut reader = std::io::Cursor::new(input);
        drain_chrome_stderr(&mut reader, sender);
        assert_eq!(reader.position(), input.len() as u64);
    }

    #[test]
    fn debugging_ports_preserve_explicit_configuration_and_native_launches() {
        for native in [false, true] {
            assert_eq!(
                debugging_port_argument(&["--remote-debugging-port=9333".into()], native).unwrap(),
                None
            );
            assert_eq!(
                debugging_port_argument(&["--remote-debugging-port=0".into()], native).unwrap(),
                None
            );
        }
        assert_eq!(
            debugging_port_argument(&[], false).unwrap(),
            Some("--remote-debugging-port=0".into())
        );
        let native = debugging_port_argument(&[], true).unwrap().unwrap();
        let port: u16 = native
            .strip_prefix("--remote-debugging-port=")
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(port, 0);
        assert!(
            debugging_port_argument(&["--remote-debugging-portability=1".into()], true)
                .unwrap()
                .is_some()
        );
    }
}
