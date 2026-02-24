use std::{
    collections::HashSet,
    fmt::Debug,
    hash::Hash,
    sync::mpsc::{Receiver, Sender},
};

use crossbeam::queue::SegQueue;

use crate::jobs::types::{
    AsyncJob, JobAccess, JobComplete, JobId, JobIdAccessible, JobOutput, JobPackage, JobRun,
    NoOutputRun, RunType,
};
use crate::jobs::JobPool;

type CompletionQueue<K, T> = std::sync::Arc<SegQueue<JobComplete<K, T>>>;

pub struct JobCache<K, T: 'static> {
    receive_new_jobs: Receiver<JobPackage<K, T>>,
    running: HashSet<JobId<K>>,
    send_new_jobs: Sender<JobPackage<K, T>>,
    completed: CompletionQueue<K, T>,
}

impl<K, T> JobCache<K, T>
where
    K: Hash + Eq + Clone + Debug + Send + 'static,
    T: Send + 'static,
{
    pub fn new(
        receive_new_jobs: Receiver<JobPackage<K, T>>,
        send_new_jobs: Sender<JobPackage<K, T>>,
    ) -> Self {
        Self {
            receive_new_jobs,
            send_new_jobs,
            completed: Default::default(),
            running: Default::default(),
        }
    }

    #[profiling::function]
    pub fn run_received(&mut self, pool: &mut JobPool, mut pre_action: impl FnMut(&JobId<K>)) {
        for pkg in self.receive_new_jobs.try_iter() {
            let id = &pkg.id;
            if JobAccess::Public == id.access && self.running.contains(&id.job_id) {
                tracing::warn!("Ignoring request to run {id:?} since it's already running");
                continue;
            }
            self.running.insert(id.job_id.clone());

            let job_run = match pkg.run {
                RunType::NoOutput(run) => {
                    no_output_run(pool, run);
                    continue;
                }
                RunType::Output(job_run) => job_run,
            };

            pre_action(&id.job_id);

            run_received_job(
                job_run,
                pool,
                self.send_new_jobs.clone(),
                self.completed.clone(),
                pkg.id,
            );
        }
    }

    #[profiling::function]
    pub fn deliver_all_completed(&mut self, mut deliver_complete: impl FnMut(JobComplete<K, T>)) {
        while let Some(res) = self.completed.pop() {
            tracing::trace!("Got completed: {:?}", res.job_id);
            let id = res.job_id.clone();
            deliver_complete(res);
            self.running.remove(&id);
        }
    }

    pub fn sender(&self) -> &Sender<JobPackage<K, T>> {
        &self.send_new_jobs
    }
}

#[profiling::function]
fn run_received_job<K, T>(
    job_run: JobRun<T>,
    pool: &mut JobPool,
    send_new_jobs: Sender<JobPackage<K, T>>,
    completion_queue: CompletionQueue<K, T>,
    id: JobIdAccessible<K>,
) where
    K: Hash + Eq + Clone + Debug + Send + 'static,
    T: Send + 'static,
{
    match job_run {
        JobRun::Sync(run) => {
            run_sync(pool, send_new_jobs, completion_queue, id, run);
        }
        JobRun::Async(run) => {
            run_async(send_new_jobs, completion_queue, id, run);
        }
    }
}

#[profiling::function]
fn run_sync<F, K, T>(
    job_pool: &mut JobPool,
    send_new_jobs: Sender<JobPackage<K, T>>,
    completion_queue: CompletionQueue<K, T>,
    id: JobIdAccessible<K>,
    run_job: F,
) where
    F: FnOnce() -> JobOutput<T> + Send + 'static,
    K: Hash + Eq + Clone + Debug + Send + 'static,
    T: Send + 'static,
{
    let id_c = id.clone();
    let wrapped: Box<dyn FnOnce() + Send + 'static> = {
        profiling::scope!("box gen");
        Box::new(move || {
            let res = run_job();
            match res {
                JobOutput::Complete(complete_response) => {
                    completion_queue.push(JobComplete {
                        job_id: id.job_id.clone(),
                        response: complete_response.response,
                    });
                    if let Some(run) = complete_response.run_no_output {
                        if let Err(e) = send_new_jobs.send(JobPackage {
                            id: id.into_internal(),
                            run: RunType::NoOutput(run),
                        }) {
                            tracing::error!("{e}");
                        }
                    }
                }
                JobOutput::Next(job_run) => {
                    if let Err(e) = send_new_jobs.send(JobPackage {
                        id: id.into_internal(),
                        run: RunType::Output(job_run),
                    }) {
                        tracing::error!("{e}");
                    }
                }
            }
        })
    };

    tracing::trace!("Spawning sync job: {id_c:?}");
    job_pool.schedule_no_output(wrapped);
}

#[profiling::function]
fn run_async<K, T>(
    send_new_jobs: Sender<JobPackage<K, T>>,
    completion_queue: CompletionQueue<K, T>,
    id: JobIdAccessible<K>,
    run_job: AsyncJob<T>,
) where
    K: Hash + Eq + Clone + Debug + Send + 'static,
    T: Send + 'static,
{
    tracing::trace!("Spawning async job: {id:?}");
    tokio::spawn(async move {
        {
            let res = run_job.await;
            match res {
                JobOutput::Complete(complete_response) => {
                    completion_queue.push(JobComplete {
                        job_id: id.job_id.clone(),
                        response: complete_response.response,
                    });
                    if let Some(run) = complete_response.run_no_output {
                        if let Err(e) = send_new_jobs.send(JobPackage {
                            id: id.into_internal(),
                            run: RunType::NoOutput(run),
                        }) {
                            tracing::error!("{e}");
                        }
                    }
                }
                JobOutput::Next(job_run) => {
                    if let Err(e) = send_new_jobs.send(JobPackage {
                        id: id.into_internal(),
                        run: RunType::Output(job_run),
                    }) {
                        tracing::error!("{e}");
                    }
                }
            }
        }
    });
}

#[profiling::function]
fn no_output_run(pool: &mut JobPool, run: NoOutputRun) {
    match run {
        NoOutputRun::Sync(c) => {
            tracing::trace!("Spawning no output sync job");
            pool.schedule_no_output(c);
        }
        NoOutputRun::Async(f) => {
            tracing::trace!("Spawning no output async sync job");
            tokio::spawn(f);
        }
    }
}
