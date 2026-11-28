//! The bounded `chrome --version` probe.

use super::*;

pub(crate) fn chrome_version_output(
    path: &Path,
    runtime: &Path,
    cancelled: &AtomicBool,
) -> Option<Vec<u8>> {
    probe_version_output(path, runtime, cancelled, Duration::from_secs(30))
}

pub(super) fn probe_version_output(
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
    let mut command = Command::new(path);
    command
        .env("TMPDIR", runtime)
        .env("TMP", runtime)
        .env("TEMP", runtime)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = spawn_executable(&mut command, Some(cancelled)).ok()?;
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
