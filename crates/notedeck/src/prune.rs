use std::path::Path;
use tokio::sync::oneshot;

/// How much smaller the database got.
pub struct PruneResult {
    pub old_size: u64,
    pub new_size: u64,
}

#[derive(Default)]
pub enum PruneStatus {
    #[default]
    Idle,
    Running(oneshot::Receiver<Result<PruneResult, String>>),
    Done(PruneResult),
    Error(String),
}

impl PruneStatus {
    /// Poll a running prune job. Returns true if the status changed.
    pub fn poll(&mut self) -> bool {
        let receiver = match self {
            PruneStatus::Running(rx) => rx,
            _ => return false,
        };

        match receiver.try_recv() {
            Ok(Ok(result)) => {
                *self = PruneStatus::Done(result);
                true
            }
            Ok(Err(e)) => {
                *self = PruneStatus::Error(e);
                true
            }
            Err(oneshot::error::TryRecvError::Empty) => false,
            Err(oneshot::error::TryRecvError::Closed) => {
                *self = PruneStatus::Error("Prune job was dropped".to_string());
                true
            }
        }
    }
}

/// Tracks prune status and cached database size.
pub struct PruneState {
    pub status: PruneStatus,
    pub cached_db_size: Option<u64>,
}

impl Default for PruneState {
    fn default() -> Self {
        Self {
            status: PruneStatus::Idle,
            cached_db_size: None,
        }
    }
}

impl PruneState {
    /// Get the database size, reading from cache or refreshing from disk.
    pub fn db_size(&mut self, db_path: &Path) -> u64 {
        if let Some(size) = self.cached_db_size {
            return size;
        }
        let size = std::fs::metadata(db_path.join("data.mdb"))
            .map(|m| m.len())
            .unwrap_or(0);
        self.cached_db_size = Some(size);
        size
    }

    /// Invalidate the cached size so it gets re-read next time.
    pub fn invalidate_size(&mut self) {
        self.cached_db_size = None;
    }
}
