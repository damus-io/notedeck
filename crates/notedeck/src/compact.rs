use std::path::Path;
use tokio::sync::oneshot;

pub struct CompactResult {
    pub old_size: u64,
    pub new_size: u64,
}

#[derive(Default)]
pub enum CompactStatus {
    #[default]
    Idle,
    Running(oneshot::Receiver<Result<CompactResult, String>>),
    Done(CompactResult),
    Error(String),
}

impl CompactStatus {
    /// Poll a running compaction job. Returns true if the status changed.
    pub fn poll(&mut self) -> bool {
        let receiver = match self {
            CompactStatus::Running(rx) => rx,
            _ => return false,
        };

        match receiver.try_recv() {
            Ok(Ok(result)) => {
                *self = CompactStatus::Done(result);
                true
            }
            Ok(Err(e)) => {
                *self = CompactStatus::Error(e);
                true
            }
            Err(oneshot::error::TryRecvError::Empty) => false,
            Err(oneshot::error::TryRecvError::Closed) => {
                *self = CompactStatus::Error("Compaction job was dropped".to_string());
                true
            }
        }
    }
}

/// Tracks compaction status and cached database size.
pub struct CompactState {
    pub status: CompactStatus,
    pub cached_db_size: Option<u64>,
}

impl Default for CompactState {
    fn default() -> Self {
        Self {
            status: CompactStatus::Idle,
            cached_db_size: None,
        }
    }
}

impl CompactState {
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
