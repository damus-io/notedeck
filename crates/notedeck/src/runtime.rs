use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use thiserror::Error;

const UI_THREAD_COUNT: usize = 1;
const OUTBOX_THREAD_COUNT: usize = 1;
const SMALL_MACHINE_MAX_CORES: usize = 4;
const SMALL_MACHINE_SYNC_JOB_THREADS: usize = 1;
const LARGE_MACHINE_SYNC_JOB_THREADS: usize = 2;

/// Thread allocation for Notedeck-owned runtime lanes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeThreadBudget {
    main_async_threads: usize,
    sync_job_threads: usize,
}

impl RuntimeThreadBudget {
    /// Build a runtime budget from the host's reported parallelism.
    pub fn from_available_parallelism() -> Self {
        let cores = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self::from_core_count(cores)
    }

    /// Build a runtime budget from an explicit core count.
    pub fn from_core_count(cores: usize) -> Self {
        let cores = cores.max(1);
        let sync_job_threads = if cores <= SMALL_MACHINE_MAX_CORES {
            SMALL_MACHINE_SYNC_JOB_THREADS
        } else {
            LARGE_MACHINE_SYNC_JOB_THREADS
        };
        let reserved_threads = UI_THREAD_COUNT + OUTBOX_THREAD_COUNT + sync_job_threads;
        let main_async_threads = cores.saturating_sub(reserved_threads).max(1);

        Self {
            main_async_threads,
            sync_job_threads,
        }
    }

    /// Threads used by the app-level Tokio runtime.
    pub fn main_async_threads(&self) -> usize {
        self.main_async_threads
    }

    /// Blocking worker threads used by [`crate::JobPool`].
    pub fn sync_job_threads(&self) -> usize {
        self.sync_job_threads
    }

    /// Build the app-level Tokio runtime used by chrome entrypoints.
    pub fn build_main_runtime(&self) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(self.main_async_threads)
            .thread_name("notedeck-main-async")
            .enable_all()
            .build()
            .expect("notedeck main Tokio runtime")
    }
}

/// Owner for the app-level async executor handle used by Notedeck services.
///
/// Production chrome passes the existing app Tokio handle. Test-runner mode can
/// own a small runtime because `Notedeck::init` may be called without one.
pub(crate) struct AppAsyncRuntime {
    accepts_jobs: Arc<AtomicBool>,
    owned_runtime: Option<tokio::runtime::Runtime>,
    handle: tokio::runtime::Handle,
}

impl AppAsyncRuntime {
    pub(crate) fn from_handle(handle: tokio::runtime::Handle) -> Self {
        Self {
            accepts_jobs: Arc::new(AtomicBool::new(true)),
            owned_runtime: None,
            handle,
        }
    }

    pub(crate) fn new_owned(worker_threads: usize) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(worker_threads.max(1))
            .thread_name("notedeck-test-async")
            .enable_all()
            .build()
            .expect("notedeck owned Tokio runtime");
        let handle = runtime.handle().clone();

        Self {
            accepts_jobs: Arc::new(AtomicBool::new(true)),
            owned_runtime: Some(runtime),
            handle,
        }
    }

    pub(crate) fn spawner(&self) -> AppAsyncSpawner {
        AppAsyncSpawner {
            accepts_jobs: Arc::clone(&self.accepts_jobs),
            handle: Some(self.handle.clone()),
        }
    }
}

impl Drop for AppAsyncRuntime {
    fn drop(&mut self) {
        self.accepts_jobs.store(false, Ordering::Release);
        if let Some(runtime) = self.owned_runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Clonable submission handle for app-level async tasks.
///
/// Use this instead of `tokio::spawn` when async work is submitted from code
/// that may run outside the app Tokio runtime, such as the remote bridge thread
/// or generic job dispatch. This keeps HTTP/media/NIP-11 style futures on the
/// app async executor instead of accidentally scheduling them on an isolated
/// service runtime.
///
/// Direct `tokio::spawn` is only appropriate for code already executing inside
/// the intended Tokio runtime and whose tasks are part of that runtime's own
/// lifecycle. If the caller is a reusable service or can be reached from another
/// OS thread, pass an `AppAsyncSpawner` through the owning state instead.
#[derive(Clone)]
pub(crate) struct AppAsyncSpawner {
    accepts_jobs: Arc<AtomicBool>,
    handle: Option<tokio::runtime::Handle>,
}

impl AppAsyncSpawner {
    pub(crate) fn unavailable() -> Self {
        Self {
            accepts_jobs: Arc::new(AtomicBool::new(true)),
            handle: None,
        }
    }

    /// Spawn fire-and-forget app async work on the captured app Tokio runtime.
    ///
    /// This is the bridge point used by code that cannot rely on
    /// `tokio::runtime::Handle::current()` selecting the app runtime.
    pub(crate) fn spawn<F>(&self, future: F) -> Result<(), AppAsyncSpawnError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if !self.accepts_jobs.load(Ordering::Acquire) {
            return Err(AppAsyncSpawnError::ShuttingDown);
        }

        let Some(handle) = &self.handle else {
            return Err(AppAsyncSpawnError::Unavailable);
        };

        handle.spawn(future);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum AppAsyncSpawnError {
    #[error("app async runtime is shutting down")]
    ShuttingDown,
    #[error("app async runtime is unavailable")]
    Unavailable,
}
