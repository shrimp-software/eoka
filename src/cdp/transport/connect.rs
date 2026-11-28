//! WebSocket connection and transport construction.

use super::*;

impl Transport {
    /// Create a new transport connecting to Chrome via WebSocket
    pub async fn new(child: Child, ws_url: &str) -> Result<Self> {
        Self::new_with_options(child, ws_url, None, 30, false).await
    }

    /// Connect the WebSocket and return the stream.
    pub(super) async fn ws_connect(ws_url: &str, timeout: Duration) -> Result<WsStream> {
        if !ws_url.starts_with("ws://") {
            return Err(Error::transport(format!(
                "Invalid WebSocket URL (expected ws://...): {}",
                ws_url
            )));
        }
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_CDP_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_CDP_MESSAGE_BYTES));
        let (stream, _resp) = tokio::time::timeout(
            timeout,
            connect_async_with_config(ws_url, Some(config), false),
        )
        .await
        .map_err(|_| Error::Timeout("WebSocket connection exceeded its deadline".into()))?
        .map_err(|e| Error::transport(format!("WebSocket connect failed: {}", e)))?;
        tracing::debug!("WebSocket connected to {}", ws_url);
        Ok(stream)
    }

    /// Spawn the reader task and build the Transport from a connected stream.
    pub(super) fn build(
        child: Option<Child>,
        stream: WsStream,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        let mut child = child.map(ChildCleanupGuard::new);
        let shutdown_socket = duplicate_socket(&stream)?;
        let child = child.as_mut().map(ChildCleanupGuard::take).transpose()?;
        let cmd_timeout = std::time::Duration::from_secs(cdp_timeout_secs);
        tracing::debug!("CDP timeout set to {}s", cdp_timeout_secs);

        let (sink, source) = stream.split();
        let writer: SharedSink = Arc::new(Mutex::new(sink));

        let pending: Arc<PendingMap> = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let completion_routes = Arc::new(CompletionRoutes::default());
        let closed = Arc::new(AtomicBool::new(false));
        let reason = CloseReason::default();
        let lifecycle = LifecycleSink::default();
        let (reader_events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let event_tx = reader_events.downgrade();
        let (commands, command_queue) = mpsc::unbounded_channel();
        let (replies, reply_queue) = mpsc::unbounded_channel();

        let pending_clone = Arc::clone(&pending);
        let response_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_responses = Arc::clone(&response_routes);
        let request_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_requests = Arc::clone(&request_routes);
        let idle_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_idle = Arc::clone(&idle_routes);
        let request_base_auth = proxy_auth.is_some();
        // Construct the guard before spawning: abort-before-first-poll must
        // still shut down the socket while Transport retains the writer.
        let writer_failed = Arc::new(tokio::sync::Notify::new());
        let completion_state = ReaderCompletionState {
            closed: Arc::clone(&closed),
            reason: Arc::clone(&reason),
            shutdown: SocketShutdownGuard(Arc::clone(&shutdown_socket)),
            lifecycle: Arc::clone(&lifecycle),
            writer_failed: Arc::clone(&writer_failed),
        };
        let writer_state = writer::WriterState {
            closed: Arc::clone(&closed),
            reason,
            write_timeout: cmd_timeout,
            shutdown: SocketShutdownGuard(Arc::clone(&shutdown_socket)),
            failed: writer_failed,
        };
        let writer_handle = tokio::spawn(writer::writer_loop(
            Arc::clone(&writer),
            reply_queue,
            command_queue,
            writer_state,
        ));
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(
                source,
                pending_clone,
                completion_state,
                reader_events,
                replies,
                reader_idle,
                FetchInterceptor {
                    responses: reader_responses,
                    requests: reader_requests,
                    proxy_auth,
                    strip: strip_x_client_data,
                },
            )
            .await;
        });

        Ok(Self {
            target_discovery: tokio::sync::OnceCell::new(),
            child: std::sync::Mutex::new(child),
            runtime: std::sync::Mutex::new(None),
            browser_endpoint: None,
            closed,
            writer,
            commands,
            writer_handle,
            lifecycle,
            shutdown_socket,
            next_id: AtomicU64::new(COMMAND_ID_MIN),
            pending,
            completion_routes,
            event_tx,
            request_routes,
            response_routes,
            idle_routes,
            request_base_auth,
            request_base_strip: strip_x_client_data,
            cmd_timeout,
            reader_handle,
            filter_cdp,
        })
    }

    /// Create a new transport with proxy auth, X-Client-Data stripping and
    /// configurable CDP timeout.
    pub async fn new_with_options(
        child: Child,
        ws_url: &str,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        Self::new_with_runtime_options(
            child,
            ws_url,
            proxy_auth,
            cdp_timeout_secs,
            true,
            strip_x_client_data,
            None,
        )
        .await
    }

    pub(crate) async fn new_with_runtime_options(
        child: Child,
        ws_url: &str,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
        runtime: Option<RuntimeDirectory>,
    ) -> Result<Self> {
        let mut child = ChildCleanupGuard::new(child);
        let stream = Self::ws_connect(ws_url, Duration::from_secs(cdp_timeout_secs)).await?;
        let mut transport = Self::build(
            Some(child.take()?),
            stream,
            proxy_auth,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        )?;
        transport.browser_endpoint = Some(ws_url.to_owned());
        *transport
            .runtime
            .get_mut()
            .expect("new runtime mutex is unpoisoned") = runtime;
        Ok(transport)
    }

    /// Connect to an existing Chrome instance at the given WebSocket URL.
    /// Does not manage a Chrome process — caller owns the browser lifecycle.
    /// `cdp_timeout_secs` bounds connection establishment and each CDP command.
    pub async fn connect(ws_url: &str, cdp_timeout_secs: u64) -> Result<Self> {
        Self::connect_with_options(ws_url, cdp_timeout_secs, true, false).await
    }

    /// Connect to an existing Chrome with full options control. Use
    /// `filter_cdp = false` to allow `Runtime.enable` and friends — needed
    /// when driving a user-owned browser where stealth filtering is unwanted.
    /// `cdp_timeout_secs` bounds connection establishment and each CDP command.
    pub async fn connect_with_options(
        ws_url: &str,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        Self::connect_with_proxy_auth(
            ws_url,
            None,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        )
        .await
    }

    /// Connect with proxy credentials answered by the reader, as on launch.
    pub(crate) async fn connect_with_proxy_auth(
        ws_url: &str,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        let stream = Self::ws_connect(ws_url, Duration::from_secs(cdp_timeout_secs)).await?;
        let mut transport = Self::build(
            None,
            stream,
            proxy_auth,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        )?;
        transport.browser_endpoint = Some(ws_url.to_owned());
        Ok(transport)
    }
}
