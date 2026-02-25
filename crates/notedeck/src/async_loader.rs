//! Async loader helpers for background NostrDB work.

use crossbeam_channel as chan;
use nostrdb::Ndb;
use std::sync::Arc;
use std::thread;

/// Handle for a background async loader that processes commands on worker threads.
pub struct AsyncLoader<Cmd, Msg> {
    cmd_tx: Option<chan::Sender<Cmd>>,
    msg_rx: Option<chan::Receiver<Msg>>,
}

impl<Cmd, Msg> AsyncLoader<Cmd, Msg>
where
    Cmd: Send + 'static,
    Msg: Send + 'static,
{
    /// Create an uninitialized loader handle.
    pub fn new() -> Self {
        Self {
            cmd_tx: None,
            msg_rx: None,
        }
    }

    /// Start the loader workers if they have not been started yet.
    pub fn start(
        &mut self,
        egui_ctx: egui::Context,
        ndb: Ndb,
        workers: usize,
        worker_name: &str,
        handler: impl Fn(Cmd, &egui::Context, &Ndb, &chan::Sender<Msg>) + Send + Sync + 'static,
    ) -> bool {
        if self.cmd_tx.is_some() {
            return false;
        }

        let (cmd_tx, cmd_rx) = chan::unbounded::<Cmd>();
        let (msg_tx, msg_rx) = chan::unbounded::<Msg>();

        self.cmd_tx = Some(cmd_tx.clone());
        self.msg_rx = Some(msg_rx);

        let handler = Arc::new(handler);
        let workers = workers.max(1);
        for idx in 0..workers {
            let cmd_rx = cmd_rx.clone();
            let msg_tx = msg_tx.clone();
            let egui_ctx = egui_ctx.clone();
            let ndb = ndb.clone();
            let handler = handler.clone();
            let name = if workers == 1 {
                worker_name.to_string()
            } else {
                format!("{worker_name}-{idx}")
            };

            thread::Builder::new()
                .name(name)
                .spawn(move || {
                    while let Ok(cmd) = cmd_rx.recv() {
                        (handler)(cmd, &egui_ctx, &ndb, &msg_tx);
                    }
                })
                .expect("failed to spawn async loader worker");
        }

        true
    }

    /// Send a loader command if the worker pool is available.
    pub fn send(&self, cmd: Cmd) {
        let Some(tx) = &self.cmd_tx else {
            return;
        };

        let _ = tx.send(cmd);
    }

    /// Try to receive the next loader message without blocking.
    pub fn try_recv(&self) -> Option<Msg> {
        let Some(rx) = &self.msg_rx else {
            return None;
        };

        rx.try_recv().ok()
    }
}

impl<Cmd, Msg> Default for AsyncLoader<Cmd, Msg>
where
    Cmd: Send + 'static,
    Msg: Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Compute a worker count based on available parallelism, clamped to a max.
pub fn worker_count(max_workers: usize) -> usize {
    let available = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let max_workers = max_workers.max(1);
    available.clamp(1, max_workers)
}
