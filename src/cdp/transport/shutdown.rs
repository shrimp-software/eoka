//! Socket shutdown, owned Chrome exit and transport teardown.

use super::*;

/// Shuts down the shared TCP connection without acquiring the WebSocket writer.
/// Each guard must shut down independently of other retained socket owners.
pub(super) struct SocketShutdownGuard(pub(super) Arc<std::net::TcpStream>);

impl Drop for SocketShutdownGuard {
    fn drop(&mut self) {
        // Shutdown affects every duplicated descriptor, including the socket
        // retained by SplitSink. Repeated shutdown after EOF is harmless.
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

pub(super) fn duplicate_socket(stream: &WsStream) -> Result<Arc<std::net::TcpStream>> {
    let MaybeTlsStream::Plain(socket) = stream.get_ref() else {
        return Err(Error::transport(
            "CDP transport requires a plain ws:// socket",
        ));
    };
    #[cfg(unix)]
    let socket = {
        use std::os::fd::AsFd;
        socket.as_fd().try_clone_to_owned()
    };
    #[cfg(windows)]
    let socket = {
        use std::os::windows::io::AsSocket;
        socket.as_socket().try_clone_to_owned()
    };
    let socket = socket
        .map(std::net::TcpStream::from)
        .map_err(|error| Error::transport(format!("Failed to duplicate CDP socket: {error}")))?;
    Ok(Arc::new(socket))
}

impl Transport {
    pub(crate) async fn wait_for_owned_exit(&self, timeout: Duration) -> Result<bool> {
        let deadline = tokio::time::Instant::now() + timeout;
        let exited = || -> Result<bool> {
            Ok(
                match self
                    .child
                    .lock()
                    .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?
                    .as_mut()
                {
                    None => true,
                    Some(child) => child.try_wait()?.is_some(),
                },
            )
        };
        let polled = crate::cdp::poll::poll_async(
            Some(deadline),
            Duration::from_millis(25),
            || match exited() {
                Ok(false) => None,
                result => Some(result),
            },
        )
        .await;
        polled.unwrap_or(Ok(false))
    }

    /// Close the transport and terminate any owned Chrome process.
    ///
    /// Waiting for the writer and sending the best-effort WebSocket close frame
    /// share the configured command timeout. TCP shutdown follows that attempt,
    /// independently of the writer lock, and also occurs if this future is
    /// cancelled. Pending requests are failed and owned cleanup is attempted
    /// even if the deadline expires. Borrowed Chrome processes are not terminated.
    /// Reaping Chrome and removing its runtime directory run on a blocking thread.
    pub async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        interception::close_routes(&self.request_routes, &self.response_routes);
        // If this future is cancelled during the graceful attempt, shutdown
        // must still happen without waiting for the writer or the reader task.
        let shutdown = SocketShutdownGuard(Arc::clone(&self.shutdown_socket));
        // Bound both writer acquisition and the best-effort close-frame write
        // so a stalled sink cannot prevent pending failure or owned cleanup.
        match tokio::time::timeout(self.cmd_timeout, async {
            self.writer.lock().await.send(Message::Close(None)).await
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!("WebSocket close frame was not sent: {}", error);
            }
            Err(error) => {
                tracing::debug!("WebSocket close frame exceeded its deadline: {}", error);
            }
        }
        drop(shutdown);
        self.stop_tasks("CDP transport closed");
        self.cleanup_owned_blocking().await
    }

    fn stop_tasks(&self, context: &str) {
        self.reader_handle.abort();
        self.writer_handle.abort();
        network_idle::disconnect(&self.idle_routes);
        fail_pending(&self.pending, context);
    }

    /// Move owned cleanup off the async runtime; leftovers are restored on failure
    /// so a later attempt (including Drop) can retry.
    async fn cleanup_owned_blocking(&self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?
            .take();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| Error::transport("Chrome runtime mutex poisoned"))?
            .take();
        if child.is_none() && runtime.is_none() {
            return Ok(());
        }
        let (result, child, runtime) = tokio::task::spawn_blocking(move || {
            let result = cleanup_parts(&mut child, &mut runtime);
            (result, child, runtime)
        })
        .await
        .map_err(|error| Error::transport(format!("Owned Chrome cleanup task failed: {error}")))?;
        if child.is_some() {
            if let Ok(mut slot) = self.child.lock() {
                *slot = child;
            }
        }
        if runtime.is_some() {
            if let Ok(mut slot) = self.runtime.lock() {
                *slot = runtime;
            }
        }
        result
    }

    /// Synchronous owned cleanup, for Drop where awaiting is impossible.
    pub(crate) fn cleanup_owned(&self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| Error::transport("Chrome runtime mutex poisoned"))?;
        cleanup_parts(&mut child, &mut runtime)
    }
}

/// Reap the child, then remove the runtime directory. Each part becomes `None`
/// once cleaned, so a failed attempt keeps what still needs cleanup.
fn cleanup_parts(child: &mut Option<Child>, runtime: &mut Option<RuntimeDirectory>) -> Result<()> {
    if let Some(process) = child.as_mut() {
        stop_child(process)?;
        *child = None;
    }
    if let Some(directory) = runtime.as_mut() {
        directory.cleanup()?;
        *runtime = None;
    }
    Ok(())
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        interception::close_routes(&self.request_routes, &self.response_routes);
        let _ = self.shutdown_socket.shutdown(std::net::Shutdown::Both);
        self.stop_tasks("CDP transport dropped");

        if let Err(error) = self.cleanup_owned() {
            tracing::warn!(
                "Failed to clean up Chrome while dropping transport: {}",
                error
            );
        }
    }
}
