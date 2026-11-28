#[cfg(unix)]
use super::version::probe_version_output;
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
        poll_blocking(Some(deadline), Duration::from_millis(10), || {
            (!started.exists() || exited.exists()).then_some(())
        });
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
    crate::cdp::poll::poll_async(None, Duration::from_millis(10), || {
        std::fs::read_to_string(path)
            .ok()
            .filter(|contents| !contents.is_empty())
    })
    .await
    .expect("polling without a deadline only ends with a value")
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

#[cfg(target_os = "linux")]
#[test]
fn spawn_waits_for_a_busy_executable_instead_of_failing() {
    use std::os::unix::fs::OpenOptionsExt;
    let runtime = crate::cdp::transport::RuntimeDirectory::create(None).unwrap();
    let script = runtime.path().join("busy-fixture");
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&script)
        .unwrap();
    std::io::Write::write_all(&mut writer, b"#!/bin/sh\nexit 0\n").unwrap();
    // An open write descriptor makes exec fail with ETXTBSY, as when a
    // concurrently forked child still holds a just-written file.
    assert_eq!(
        Command::new(&script).spawn().unwrap_err().kind(),
        std::io::ErrorKind::ExecutableFileBusy
    );
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(writer);
    });
    let mut child = spawn_executable(&mut Command::new(&script), None).unwrap();
    assert!(child.wait().unwrap().success());
    release.join().unwrap();

    let cancelled = AtomicBool::new(true);
    let _writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&script)
        .unwrap();
    let started = Instant::now();
    assert_eq!(
        spawn_executable(&mut Command::new(&script), Some(&cancelled))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::ExecutableFileBusy
    );
    assert!(started.elapsed() < Duration::from_millis(100));
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
