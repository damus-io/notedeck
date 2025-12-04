use crossbeam::channel;
use std::future::Future;
use tokio::sync::oneshot::{self, Receiver};

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct JobPool {
    tx: channel::Sender<Job>,
}

impl Default for JobPool {
    fn default() -> Self {
        JobPool::new(2)
    }
}

impl JobPool {
    pub fn new(num_threads: usize) -> Self {
        let (tx, rx) = channel::unbounded::<Job>();
        for i in 0..num_threads {
            let rx = rx.clone();
            std::thread::spawn(move || {
                for job in rx.iter() {
                    tracing::trace!("Starting job on thread {i}");
                    job();
                    tracing::trace!("Finished job on thread {i}");
                }
            });
        }

        Self { tx }
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
        let (tx_result, rx_result) = oneshot::channel::<T>();

        let job = Box::new(move || {
            let output = job();
            let _ = tx_result.send(output);
        });

        self.push_job(job);

        rx_result
    }

    pub fn schedule_no_output(&self, job: impl FnOnce() + Send + 'static) {
        self.push_job(Box::new(job));
    }

    fn push_job(&self, job: Job) {
        if let Err(e) = self.tx.send(job) {
            tracing::error!("job queue closed unexpectedly: {e}");
        }
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
