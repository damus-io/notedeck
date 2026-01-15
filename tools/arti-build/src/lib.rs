//! JNI wrapper for Arti Tor client on Android.
//!
//! This crate provides a native Android interface to the Arti Tor client,
//! exposing functions for initializing Tor, starting a SOCKS5 proxy, and
//! managing the connection lifecycle.

use std::net::SocketAddr;
use std::panic;
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use jni::objects::{GlobalRef, JClass, JObject, JString};
use jni::sys::{jboolean, jint, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use arti_client::{config::TorClientConfigBuilder, DataStream, TorClient};
use tor_config_path::CfgPath;
use tor_rtcompat::PreferredRuntime;

/// Global state for the Arti client
static STATE: OnceLock<Mutex<ArtiState>> = OnceLock::new();

/// Global reference to the JavaVM for callbacks
static JVM: OnceLock<jni::JavaVM> = OnceLock::new();

/// Global log callback reference
static LOG_CALLBACK: OnceLock<Mutex<Option<GlobalRef>>> = OnceLock::new();

struct ArtiState {
    runtime: Option<Runtime>,
    client: Option<TorClient<PreferredRuntime>>,
    proxy_task: Option<JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    socks_port: Option<u16>,
}

impl Default for ArtiState {
    fn default() -> Self {
        Self {
            runtime: None,
            client: None,
            proxy_task: None,
            shutdown_tx: None,
            socks_port: None,
        }
    }
}

fn get_state() -> &'static Mutex<ArtiState> {
    STATE.get_or_init(|| Mutex::new(ArtiState::default()))
}

fn log_to_java(message: &str) {
    if let Some(callback_lock) = LOG_CALLBACK.get() {
        if let Ok(callback_guard) = callback_lock.lock() {
            if let Some(callback) = callback_guard.as_ref() {
                if let Some(jvm) = JVM.get() {
                    if let Ok(mut env) = jvm.attach_current_thread() {
                        let msg = env.new_string(message).ok();
                        if let Some(msg) = msg {
                            let _ = env.call_method(
                                callback.as_obj(),
                                "onLog",
                                "(Ljava/lang/String;)V",
                                &[(&msg).into()],
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Initialize the Arti Tor client with the specified data directories.
///
/// # Safety
/// This function is called from JNI and must handle all errors gracefully.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_initialize(
    mut env: JNIEnv,
    _class: JClass,
    cache_dir: JString,
    state_dir: JString,
) -> jboolean {
    // Store JVM reference on first call
    if JVM.get().is_none() {
        if let Ok(jvm) = env.get_java_vm() {
            let _ = JVM.set(jvm);
        }
    }

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let cache_path: String = env
            .get_string(&cache_dir)
            .map(|s| s.into())
            .map_err(|e| anyhow!("Failed to get cache_dir: {}", e))?;
        let state_path: String = env
            .get_string(&state_dir)
            .map(|s| s.into())
            .map_err(|e| anyhow!("Failed to get state_dir: {}", e))?;

        let mut state = get_state().lock().map_err(|e| anyhow!("Lock error: {}", e))?;

        // Create runtime if needed
        if state.runtime.is_none() {
            let runtime = Runtime::new().map_err(|e| anyhow!("Failed to create runtime: {}", e))?;
            state.runtime = Some(runtime);
        }

        let runtime = state.runtime.as_ref().unwrap();

        // Bootstrap Tor client
        let client = runtime.block_on(async {
            let mut config_builder = TorClientConfigBuilder::default();
            config_builder
                .storage()
                .cache_dir(CfgPath::new(cache_path.clone()))
                .state_dir(CfgPath::new(state_path.clone()));

            let config = config_builder
                .build()
                .map_err(|e| anyhow!("Config build failed: {}", e))?;

            log_to_java(&format!(
                "Bootstrapping Tor client (cache: {}, state: {})",
                cache_path, state_path
            ));

            TorClient::create_bootstrapped(config)
                .await
                .map_err(|e| anyhow!("Bootstrap failed: {}", e))
        })?;

        state.client = Some(client);
        log_to_java("Tor client initialized successfully");

        Ok::<_, anyhow::Error>(())
    }));

    match result {
        Ok(Ok(())) => JNI_TRUE,
        Ok(Err(e)) => {
            log_to_java(&format!("Initialize error: {}", e));
            let _ = env.throw_new("java/lang/RuntimeException", format!("{}", e));
            JNI_FALSE
        }
        Err(_) => {
            log_to_java("Initialize panic");
            let _ = env.throw_new("java/lang/RuntimeException", "Panic during initialization");
            JNI_FALSE
        }
    }
}

/// Start the SOCKS5 proxy on the specified port.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_startSocksProxy(
    mut env: JNIEnv,
    _class: JClass,
    port: jint,
) -> jboolean {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut state = get_state().lock().map_err(|e| anyhow!("Lock error: {}", e))?;

        // Check if already running
        if state.proxy_task.is_some() {
            return Ok(());
        }

        let client = state
            .client
            .clone()
            .ok_or_else(|| anyhow!("Tor client not initialized"))?;

        let runtime = state
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow!("Runtime not initialized"))?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let port = port as u16;
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;

        log_to_java(&format!("Starting SOCKS proxy on {}", addr));

        let task = runtime.spawn(run_socks_proxy(client, addr, shutdown_rx));

        state.proxy_task = Some(task);
        state.shutdown_tx = Some(shutdown_tx);
        state.socks_port = Some(port);

        log_to_java(&format!("SOCKS proxy started on port {}", port));

        Ok::<_, anyhow::Error>(())
    }));

    match result {
        Ok(Ok(())) => JNI_TRUE,
        Ok(Err(e)) => {
            log_to_java(&format!("StartSocksProxy error: {}", e));
            let _ = env.throw_new("java/lang/RuntimeException", format!("{}", e));
            JNI_FALSE
        }
        Err(_) => {
            log_to_java("StartSocksProxy panic");
            let _ = env.throw_new("java/lang/RuntimeException", "Panic during proxy start");
            JNI_FALSE
        }
    }
}

/// Stop the SOCKS5 proxy.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_stop(
    mut env: JNIEnv,
    _class: JClass,
) {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut state = get_state().lock().map_err(|e| anyhow!("Lock error: {}", e))?;

        // Send shutdown signal
        if let Some(tx) = state.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // Abort the task if still running
        if let Some(task) = state.proxy_task.take() {
            task.abort();
        }

        state.socks_port = None;
        log_to_java("SOCKS proxy stopped");

        Ok::<_, anyhow::Error>(())
    }));

    if let Ok(Err(e)) = result {
        log_to_java(&format!("Stop error: {}", e));
        let _ = env.throw_new("java/lang/RuntimeException", format!("{}", e));
    }
}

/// Get the current SOCKS proxy port, or -1 if not running.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_getSocksPort(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    let result = panic::catch_unwind(|| {
        if let Ok(state) = get_state().lock() {
            state.socks_port.map(|p| p as jint).unwrap_or(-1)
        } else {
            -1
        }
    });

    result.unwrap_or(-1)
}

/// Check if the Tor client is initialized and ready.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_isInitialized(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    let result = panic::catch_unwind(|| {
        if let Ok(state) = get_state().lock() {
            if state.client.is_some() {
                JNI_TRUE
            } else {
                JNI_FALSE
            }
        } else {
            JNI_FALSE
        }
    });

    result.unwrap_or(JNI_FALSE)
}

/// Get the Arti version string.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_getVersion<'local>(
    env: JNIEnv<'local>,
    _class: JClass,
) -> jstring {
    let version = env!("CARGO_PKG_VERSION");
    env.new_string(version)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Set the log callback for receiving log messages in Java.
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_tor_ArtiNative_setLogCallback(
    env: JNIEnv,
    _class: JClass,
    callback: JObject,
) {
    // Store JVM reference
    if JVM.get().is_none() {
        if let Ok(jvm) = env.get_java_vm() {
            let _ = JVM.set(jvm);
        }
    }

    let callback_lock = LOG_CALLBACK.get_or_init(|| Mutex::new(None));

    if let Ok(mut guard) = callback_lock.lock() {
        if callback.is_null() {
            *guard = None;
        } else {
            // new_global_ref takes &self, not &mut self in jni 0.21
            if let Ok(global_ref) = env.new_global_ref(&callback) {
                *guard = Some(global_ref);
            }
        }
    }
}

/// Run the SOCKS5 proxy server.
async fn run_socks_proxy(
    client: TorClient<PreferredRuntime>,
    addr: SocketAddr,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            log_to_java(&format!("Failed to bind SOCKS proxy: {}", e));
            return;
        }
    };

    log_to_java(&format!("SOCKS proxy listening on {}", addr));

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, peer_addr)) => {
                        let client = client.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_socks_connection(client, stream).await {
                                log_to_java(&format!("Connection from {} failed: {}", peer_addr, e));
                            }
                        });
                    }
                    Err(e) => {
                        log_to_java(&format!("Accept error: {}", e));
                    }
                }
            }
            _ = &mut shutdown_rx => {
                log_to_java("SOCKS proxy shutting down");
                break;
            }
        }
    }
}

/// Handle a single SOCKS5 connection.
async fn handle_socks_connection(
    client: TorClient<PreferredRuntime>,
    mut stream: TcpStream,
) -> Result<()> {
    // SOCKS5 handshake - read greeting
    let mut buf = [0u8; 2];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| anyhow!("Failed to read greeting: {}", e))?;

    if buf[0] != 0x05 {
        return Err(anyhow!("Not SOCKS5"));
    }

    let nmethods = buf[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream
        .read_exact(&mut methods)
        .await
        .map_err(|e| anyhow!("Failed to read methods: {}", e))?;

    // Send response - no auth required
    stream
        .write_all(&[0x05, 0x00])
        .await
        .map_err(|e| anyhow!("Failed to send auth response: {}", e))?;

    // Read connection request
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| anyhow!("Failed to read request header: {}", e))?;

    if header[0] != 0x05 || header[1] != 0x01 {
        // Only support CONNECT command
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Err(anyhow!("Unsupported SOCKS command"));
    }

    // Parse address
    let (host, port) = match header[3] {
        0x01 => {
            // IPv4
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            let port = u16::from_be_bytes(port_buf);
            let ip = std::net::Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]);
            (ip.to_string(), port)
        }
        0x03 => {
            // Domain name
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            let port = u16::from_be_bytes(port_buf);
            (String::from_utf8_lossy(&domain).to_string(), port)
        }
        0x04 => {
            // IPv6
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await?;
            let port = u16::from_be_bytes(port_buf);
            let ip = std::net::Ipv6Addr::from(addr);
            (ip.to_string(), port)
        }
        _ => {
            stream
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            return Err(anyhow!("Unsupported address type"));
        }
    };

    // Connect through Tor
    let tor_stream: DataStream = match client.connect((&host[..], port)).await {
        Ok(s) => s,
        Err(e) => {
            stream
                .write_all(&[0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            return Err(anyhow!("Tor connect failed: {}", e));
        }
    };

    // Send success response
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .await?;

    // Bidirectional copy
    let (mut client_read, mut client_write) = stream.into_split();
    let (mut tor_read, mut tor_write) = tor_stream.split();

    let client_to_tor = async {
        let mut buf = [0u8; 8192];
        loop {
            let n = client_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            tor_write.write_all(&buf[..n]).await?;
        }
        Ok::<_, std::io::Error>(())
    };

    let tor_to_client = async {
        let mut buf = [0u8; 8192];
        loop {
            let n = tor_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            client_write.write_all(&buf[..n]).await?;
        }
        Ok::<_, std::io::Error>(())
    };

    tokio::select! {
        _ = client_to_tor => {}
        _ = tor_to_client => {}
    }

    Ok(())
}
