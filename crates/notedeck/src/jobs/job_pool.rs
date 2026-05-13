use crossbeam::channel;
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::oneshot::{self, Receiver};

type Job = Box<dyn FnOnce() + Send + 'static>;
type JobTx = channel::Sender<JobMessage>;

enum JobMessage {
    Run(Job),
    Shutdown,
}

/// Shared submission handle for the host job pool.
#[derive(Clone)]
pub(crate) struct JobSpawner {
    tx: JobTx,
    accepts_jobs: Arc<AtomicBool>,
}

/// Worker pool for short host-side background jobs.
pub struct JobPool {
    tx: JobTx,
    workers: Vec<std::thread::JoinHandle<()>>,
    accepts_jobs: Arc<AtomicBool>,
}

impl Drop for JobPool {
    fn drop(&mut self) {
        // Stop accepting new submissions before pushing worker shutdown markers
        // so pool teardown does not depend on external sender clones dropping.
        self.accepts_jobs.store(false, Ordering::Release);
        for _ in 0..self.workers.len() {
            let _ = self.tx.send(JobMessage::Shutdown);
        }
        for worker in self.workers.drain(..) {
            worker.join().ok();
        }
    }
}

impl Default for JobPool {
    fn default() -> Self {
        JobPool::new(2)
    }
}

impl JobPool {
    pub fn new(num_threads: usize) -> Self {
        let (tx, rx) = channel::unbounded::<JobMessage>();
        let accepts_jobs = Arc::new(AtomicBool::new(true));
        let mut workers = Vec::with_capacity(num_threads);
        for i in 0..num_threads {
            let rx = rx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("job-pool-{i}"))
                .spawn(move || {
                    while let Ok(message) = rx.recv() {
                        let JobMessage::Run(job) = message else {
                            break;
                        };
                        tracing::trace!("Starting job on thread {i}");
                        job();
                        tracing::trace!("Finished job on thread {i}");
                    }
                })
                .expect("failed to spawn job pool worker");
            workers.push(handle);
        }

        Self {
            tx,
            workers,
            accepts_jobs,
        }
    }

    pub fn schedule<F, T>(&self, job: F) -> impl Future<Output = T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let rx_result = self.schedule_receivable(job);
        async move {
            rx_result.await.unwrap_or_else(|_| {
                panic!("Worker thread or channel dropped before returning the result.")
            })
        }
    }

    pub fn schedule_receivable<F, T>(&self, job: F) -> Receiver<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        schedule_receivable_on(&self.tx, &self.accepts_jobs, job)
    }

    pub fn schedule_no_output(&self, job: impl FnOnce() + Send + 'static) {
        push_job(&self.tx, &self.accepts_jobs, Box::new(job));
    }

    /// Create a clonable submission handle for this pool.
    pub(crate) fn spawner(&self) -> JobSpawner {
        JobSpawner {
            tx: self.tx.clone(),
            accepts_jobs: Arc::clone(&self.accepts_jobs),
        }
    }
}

impl JobSpawner {
    pub(crate) fn schedule_receivable<F, T>(&self, job: F) -> Receiver<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        schedule_receivable_on(&self.tx, &self.accepts_jobs, job)
    }
}

fn schedule_receivable_on<F, T>(tx: &JobTx, accepts_jobs: &AtomicBool, job: F) -> Receiver<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx_result, rx_result) = oneshot::channel::<T>();

    let job = Box::new(move || {
        let output = job();
        let _ = tx_result.send(output);
    });

    push_job(tx, accepts_jobs, job);

    rx_result
}

fn push_job(tx: &JobTx, accepts_jobs: &AtomicBool, job: Job) {
    if !accepts_jobs.load(Ordering::Acquire) {
        tracing::debug!("dropping job submission after pool shutdown");
        return;
    }

    if let Err(e) = tx.send(JobMessage::Run(job)) {
        tracing::error!("job queue closed unexpectedly: {e}");
    }
}

#[cfg(test)]
mod tests {
    use crate::jobs::JobPool;

    fn test_fn(a: u32, b: u32) -> u32 {
        a + b
    }

    #[tokio::test]
    async fn test() {
        let pool = JobPool::default();

        // Now each job can return different T
        let future_str = pool.schedule(|| -> String { "hello from string job".into() });

        let a = 5;
        let b = 6;
        let future_int = pool.schedule(move || -> u32 { test_fn(a, b) });

        println!("(Meanwhile we can do more async work) ...");

        let s = future_str.await;
        let i = future_int.await;

        println!("Got string: {:?}", s);
        println!("Got integer: {}", i);
    }
}
