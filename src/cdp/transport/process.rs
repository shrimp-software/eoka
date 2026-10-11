//! Chrome child ownership, launch and DevTools endpoint discovery.

use crate::{Error, Result};
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Stdio},
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
    launch_chrome_impl(path, args, None, false)
}

pub(crate) fn launch_chrome_with_profile_dir(
    path: &std::path::Path,
    args: &[String],
    profile_dir: &Path,
    live_session: bool,
) -> Result<(Child, String)> {
    launch_chrome_impl(path, args, Some(profile_dir), live_session)
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
) -> Result<(Child, String)> {
    use std::process::Command;

    let mut cmd = Command::new(path);
    cmd.args(args);
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

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Launch(format!("Failed to launch Chrome: {}", e)))?;

    let stderr = child
        .stderr
        .take()
        .ok_or(Error::Launch("No stderr from Chrome".into()))?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drain_chrome_stderr(BufReader::new(stderr), tx));

    let ws_url = wait_for_chrome_devtools_url(&mut child, &rx, fallback_profile_dir)?;

    tracing::info!("Chrome DevTools URL: {}", ws_url);

    Ok((child, ws_url))
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
            stderr_tail.clear();
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
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        match rx.try_recv() {
            Ok(Ok(url)) => return Ok(url),
            Ok(Err(stderr)) => {
                let status = child
                    .try_wait()
                    .map(|status| status.map(|s| s.to_string()))
                    .unwrap_or(None)
                    .unwrap_or_else(|| "still running".to_string());
                stop_child_after_failed_launch(child);
                return Err(Error::Launch(format!(
                    "Failed waiting for Chrome DevTools URL: process {}; {}",
                    status, stderr
                )));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                stop_child_after_failed_launch(child);
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
            stop_child_after_failed_launch(child);
            return Err(Error::Launch(format!(
                "Failed waiting for Chrome DevTools URL: process {}; {}",
                status, stderr
            )));
        }

        if Instant::now() >= deadline {
            stop_child_after_failed_launch(child);
            return Err(Error::Launch(
                "Failed waiting for Chrome DevTools URL after 30s".into(),
            ));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop_child_after_failed_launch(child: &mut Child) {
    if let Err(cleanup_error) = stop_child(child) {
        tracing::warn!(
            "Failed to clean up Chrome after launch-channel error: {}",
            cleanup_error
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
