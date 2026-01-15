#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod inner {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::mpsc::{self, Receiver, TryRecvError},
        thread,
        time::{Duration, Instant},
    };

    use arti::proxy;
    use arti_client::{config::TorClientConfig, TorClient};
    use tokio::sync::oneshot;
    use tor_config::Listen;
    use tor_config_path::CfgPath;
    use tor_rtcompat::{PreferredRuntime, ToplevelBlockOn};

    use crate::{DataPath, DataPathType};

    /// Default SOCKS proxy port for Tor connections.
    ///
    /// Port 9150 is the standard SOCKS port for Tor Browser. If another Tor
    /// instance (like Tor Browser) is already using this port, startup will fail.
    /// Future enhancement: probe for available port or provide user configuration.
    const DEFAULT_SOCKS_PORT: u16 = 9150;

    /// Timeout duration for graceful shutdown of the Tor runtime thread.
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    /// Internal signaling for bootstrap completion status.
    enum ReadyState {
        Ok,
        Err(String),
    }

    /// Directory paths for Tor client persistent storage.
    #[derive(Clone)]
    struct TorDirs {
        /// Directory for cached consensus and descriptor data
        cache: PathBuf,
        /// Directory for guard state and other persistent state
        state: PathBuf,
    }

    /// Handle to the background Tor runtime thread.
    ///
    /// Provides shutdown signaling and thread management. When dropped,
    /// automatically attempts graceful shutdown with timeout.
    struct TorHandle {
        /// Channel to signal shutdown to the runtime
        shutdown: Option<oneshot::Sender<()>>,
        /// Handle to the background thread running the Arti runtime
        thread: Option<thread::JoinHandle<()>>,
        /// The SOCKS port this instance is listening on
        socks_port: u16,
    }

    impl TorHandle {
        /// Stop the Tor runtime thread with a timeout to prevent indefinite blocking.
        ///
        /// Sends the shutdown signal and waits up to `SHUTDOWN_TIMEOUT` for the
        /// thread to exit gracefully. If the thread doesn't respond in time, it
        /// is abandoned to prevent blocking the main thread.
        fn stop(&mut self) {
            // Send shutdown signal
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }

            // Wait for thread with timeout to prevent indefinite blocking
            if let Some(handle) = self.thread.take() {
                let start = Instant::now();
                while !handle.is_finished() {
                    if start.elapsed() >= SHUTDOWN_TIMEOUT {
                        tracing::warn!(
                            "Tor runtime did not shut down within {:?}, abandoning thread",
                            SHUTDOWN_TIMEOUT
                        );
                        // Thread is abandoned but will eventually terminate
                        return;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                // Thread finished, join to clean up resources
                let _ = handle.join();
            }
        }
    }

    impl Drop for TorHandle {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Overall Tor connection status used throughout the UI.
    #[derive(Clone, Debug)]
    pub enum TorStatus {
        Disabled,
        Starting,
        Running { socks_port: u16 },
        Failed(String),
        Unsupported,
    }

    /// Host-side Tor client that owns the runtime thread and SOCKS proxy.
    pub struct TorManager {
        dirs: TorDirs,
        status: TorStatus,
        handle: Option<TorHandle>,
        ready_rx: Option<Receiver<ReadyState>>,
    }

    impl TorManager {
        /// Create a new TorManager with storage directories under the given data path.
        pub fn new(data_path: &DataPath) -> Self {
            let cache = data_path.path(DataPathType::Cache).join("tor");
            let state = data_path.path(DataPathType::Setting).join("tor");
            Self {
                dirs: TorDirs { cache, state },
                status: TorStatus::Disabled,
                handle: None,
                ready_rx: None,
            }
        }

        /// Returns true if Tor is supported on this platform.
        pub const fn is_supported() -> bool {
            true
        }

        /// Get the current Tor connection status.
        pub fn status(&self) -> TorStatus {
            self.status.clone()
        }

        /// Enable or disable the Tor client.
        ///
        /// When enabling, spawns the Arti runtime in a background thread.
        /// When disabling, signals shutdown and waits for graceful termination.
        pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
            if enabled {
                self.start()
            } else {
                self.stop();
                Ok(())
            }
        }

        /// Get the SOCKS proxy address if Tor is running.
        ///
        /// Returns `Some("127.0.0.1:<port>")` when connected, `None` otherwise.
        pub fn socks_proxy(&self) -> Option<String> {
            if let TorStatus::Running { socks_port } = self.status {
                Some(format!("127.0.0.1:{socks_port}"))
            } else {
                None
            }
        }

        /// Poll for status updates from the background Tor runtime.
        ///
        /// Should be called each frame to check for bootstrap completion or errors.
        /// Updates `self.status` when the runtime signals ready or fails.
        pub fn poll(&mut self) {
            if let Some(rx) = &self.ready_rx {
                match rx.try_recv() {
                    Ok(ReadyState::Ok) => {
                        if let Some(handle) = &self.handle {
                            self.status = TorStatus::Running {
                                socks_port: handle.socks_port,
                            };
                        } else {
                            self.status =
                                TorStatus::Failed("Tor runtime handle missing".to_owned());
                        }
                        self.ready_rx = None;
                    }
                    Ok(ReadyState::Err(err)) => {
                        self.status = TorStatus::Failed(err);
                        self.ready_rx = None;
                        self.drop_handle();
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        self.status =
                            TorStatus::Failed("Tor runtime exited unexpectedly".to_owned());
                        self.ready_rx = None;
                        self.drop_handle();
                    }
                }
            }
        }

        /// Start the runtime thread if we aren't already doing so.
        fn start(&mut self) -> Result<(), String> {
            match self.status {
                TorStatus::Starting | TorStatus::Running { .. } => return Ok(()),
                _ => {}
            }

            fs::create_dir_all(&self.dirs.cache)
                .map_err(|err| format!("failed to create tor cache dir: {err}"))?;
            fs::create_dir_all(&self.dirs.state)
                .map_err(|err| format!("failed to create tor state dir: {err}"))?;

            let (ready_tx, ready_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let dirs = self.dirs.clone();
            let socks_port = DEFAULT_SOCKS_PORT;

            let thread = match thread::Builder::new()
                .name("notedeck-tor".into())
                .spawn(move || {
                    if let Err(err) = run_tor_runtime(dirs, socks_port, ready_tx, shutdown_rx) {
                        tracing::error!("tor runtime exited: {err}");
                    }
                }) {
                Ok(handle) => handle,
                Err(err) => {
                    let msg = format!("failed to spawn tor runtime: {err}");
                    self.status = TorStatus::Failed(msg.clone());
                    return Err(msg);
                }
            };

            self.handle = Some(TorHandle {
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
                socks_port,
            });
            self.ready_rx = Some(ready_rx);
            self.status = TorStatus::Starting;
            Ok(())
        }

        fn stop(&mut self) {
            self.ready_rx = None;
            self.status = TorStatus::Disabled;
            self.drop_handle();
        }

        fn drop_handle(&mut self) {
            if let Some(mut handle) = self.handle.take() {
                handle.stop();
            }
        }
    }

    impl Drop for TorManager {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Launch the blocking Arti task inside the runtime and wait for shutdown.
    ///
    /// Bootstraps the Tor client, starts the SOCKS proxy, verifies the port
    /// is listening, and then signals ready. The ready signal is only sent
    /// after confirming the proxy is accepting connections.
    fn run_tor_runtime(
        dirs: TorDirs,
        socks_port: u16,
        ready_tx: mpsc::Sender<ReadyState>,
        shutdown_rx: oneshot::Receiver<()>,
    ) -> Result<(), String> {
        let runtime =
            PreferredRuntime::create().map_err(|err| format!("failed to create runtime: {err}"))?;
        let runtime_handle = runtime.clone();
        let ready_tx_clone = ready_tx.clone();

        match runtime_handle.block_on(async move {
            let runtime = runtime;
            let client_config = build_client_config(&dirs)?;
            let tor_client = TorClient::with_runtime(runtime.clone())
                .config(client_config)
                .create_bootstrapped()
                .await
                .map_err(|err| format!("tor bootstrap failed: {err}"))?;

            let listen = Listen::new_localhost(socks_port);
            let proxy_future = proxy::run_proxy(runtime.clone(), tor_client, listen, None);
            tokio::pin!(proxy_future);

            // Give the proxy a moment to bind the socket, then verify it's listening
            // before signaling ready. Poll once to start the proxy.
            tokio::select! {
                biased;
                res = &mut proxy_future => {
                    // Proxy exited immediately - likely a binding error
                    return res.map_err(|err| format!("tor proxy failed: {err}"));
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Proxy is running, verify the port is actually listening
                    if verify_port_listening(socks_port).await {
                        let _ = ready_tx_clone.send(ReadyState::Ok);
                    } else {
                        return Err(format!("SOCKS port {socks_port} not accepting connections"));
                    }
                }
            }

            // Continue running the proxy until shutdown
            tokio::select! {
                res = &mut proxy_future => {
                    res.map_err(|err| format!("tor proxy failed: {err}"))
                }
                _ = shutdown_rx => Ok(()),
            }
        }) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = ready_tx.send(ReadyState::Err(err.clone()));
                Err(err)
            }
        }
    }

    /// Verify that a TCP port is accepting connections.
    async fn verify_port_listening(port: u16) -> bool {
        use tokio::net::TcpStream;
        let addr = format!("127.0.0.1:{port}");
        TcpStream::connect(&addr).await.is_ok()
    }

    /// Build the Tor client configuration with cache and state directories.
    ///
    /// Configures the Arti client to use notedeck's data directories for
    /// persistent storage of consensus data, cached descriptors, and guard state.
    ///
    /// # Arguments
    /// * `dirs` - The directory paths for cache and state storage
    ///
    /// # Returns
    /// A configured `TorClientConfig` or an error message
    fn build_client_config(dirs: &TorDirs) -> Result<TorClientConfig, String> {
        let mut builder = TorClientConfig::builder();
        builder
            .storage()
            .cache_dir(CfgPath::new(path_to_string(&dirs.cache)?))
            .state_dir(CfgPath::new(path_to_string(&dirs.state)?));
        builder
            .build()
            .map_err(|err| format!("failed to build tor config: {err}"))
    }

    /// Convert a Path to a String, returning an error for non-UTF8 paths.
    ///
    /// Required because Arti's CfgPath expects a String, not a Path reference.
    fn path_to_string(path: &Path) -> Result<String, String> {
        path.to_str()
            .map(|s| s.to_owned())
            .ok_or_else(|| "invalid unicode path".to_owned())
    }

    pub use TorManager as Manager;
    pub use TorStatus as Status;
}

#[cfg(any(target_arch = "wasm32", target_os = "android"))]
mod inner {
    use crate::DataPath;

    #[derive(Clone, Debug)]
    pub enum TorStatus {
        Disabled,
        Starting,
        Running { socks_port: u16 },
        Failed(String),
        Unsupported,
    }

    pub struct TorManager;

    impl TorManager {
        pub fn new(_data_path: &DataPath) -> Self {
            Self
        }

        pub const fn is_supported() -> bool {
            false
        }

        pub fn status(&self) -> TorStatus {
            TorStatus::Unsupported
        }

        pub fn set_enabled(&mut self, _enabled: bool) -> Result<(), String> {
            Ok(())
        }

        pub fn socks_proxy(&self) -> Option<String> {
            None
        }

        pub fn poll(&mut self) {}
    }

    pub use TorManager as Manager;
    pub use TorStatus as Status;
}

pub use inner::Manager as TorManager;
pub use inner::Status as TorStatus;
