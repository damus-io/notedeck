#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod inner {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::mpsc::{self, Receiver, TryRecvError},
        thread,
    };

    use arti::proxy;
    use arti_client::{config::TorClientConfig, TorClient};
    use tokio::sync::oneshot;
    use tor_config::Listen;
    use tor_config_path::CfgPath;
    use tor_rtcompat::{PreferredRuntime, ToplevelBlockOn};

    use crate::{DataPath, DataPathType};

    const DEFAULT_SOCKS_PORT: u16 = 9150;

    enum ReadyState {
        Ok,
        Err(String),
    }

    #[derive(Clone)]
    struct TorDirs {
        cache: PathBuf,
        state: PathBuf,
    }

    struct TorHandle {
        shutdown: Option<oneshot::Sender<()>>,
        thread: Option<thread::JoinHandle<()>>,
        socks_port: u16,
    }

    impl TorHandle {
        fn stop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.thread.take() {
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

        pub const fn is_supported() -> bool {
            true
        }

        pub fn status(&self) -> TorStatus {
            self.status.clone()
        }

        pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
            if enabled {
                self.start()
            } else {
                self.stop();
                Ok(())
            }
        }

        pub fn socks_proxy(&self) -> Option<String> {
            if let TorStatus::Running { socks_port } = self.status {
                Some(format!("127.0.0.1:{socks_port}"))
            } else {
                None
            }
        }

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

            let _ = ready_tx_clone.send(ReadyState::Ok);

            let listen = Listen::new_localhost(socks_port);
            let proxy_future = proxy::run_proxy(runtime.clone(), tor_client, listen, None);
            tokio::pin!(proxy_future);
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
