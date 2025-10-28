use std::{
    future::Future,
    sync::{
        mpsc::{self, Sender},
        Arc, Mutex,
    },
};
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct JobPool {
    tx: Sender<Job>,
}

impl Default for JobPool {
    fn default() -> Self {
        JobPool::new(2)
    }
}

impl JobPool {
    pub fn new(num_threads: usize) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();

        // TODO(jb55) why not mpmc here !???
        let arc_rx = Arc::new(Mutex::new(rx));
        for _ in 0..num_threads {
            let arc_rx_clone = arc_rx.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let Ok(unlocked) = arc_rx_clone.lock() else {
                        continue;
                    };
                    let Ok(job) = unlocked.recv() else {
                        continue;
                    };

                    job
                };

                job();
            });
        }

        Self { tx }
    }

    pub fn schedule<F, T>(&self, job: F) -> impl Future<Output = T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx_result, rx_result) = oneshot::channel::<T>();

        let job = Box::new(move || {
            let output = job();
            let _ = tx_result.send(output);
        });

        self.tx
            .send(job)
            .expect("receiver should not be deallocated");

        async move {
            rx_result.await.unwrap_or_else(|_| {
                panic!("Worker thread or channel dropped before returning the result.")
            })
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
