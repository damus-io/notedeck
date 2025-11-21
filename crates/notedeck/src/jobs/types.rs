use std::{future::Future, pin::Pin};

pub enum JobOutput<T> {
    Complete(CompleteResponse<T>),
    Next(JobRun<T>),
}

impl<T> JobOutput<T> {
    pub fn complete(response: T) -> Self {
        JobOutput::Complete(CompleteResponse::new(response))
    }
}

pub struct CompleteResponse<T> {
    pub(crate) response: T,
    pub(crate) run_no_output: Option<NoOutputRun>,
}

pub struct JobComplete<K, T> {
    pub job_id: JobId<K>,
    pub response: T,
}

impl<T> CompleteResponse<T> {
    pub fn new(response: T) -> Self {
        Self {
            response,
            run_no_output: None,
        }
    }

    pub fn run_no_output(mut self, run: NoOutputRun) -> Self {
        self.run_no_output = Some(run);
        self
    }
}

pub enum NoOutputRun {
    Sync(Box<dyn FnOnce() + Send + 'static>),
    Async(Pin<Box<dyn Future<Output = ()> + Send + 'static>>),
}

pub(crate) type SyncJob<T> = Box<dyn FnOnce() -> JobOutput<T> + Send + 'static>;
pub(crate) type AsyncJob<T> = Pin<Box<dyn Future<Output = JobOutput<T>> + Send + 'static>>;

pub enum JobRun<T> {
    Sync(SyncJob<T>),
    Async(AsyncJob<T>),
}

pub struct JobPackage<K, T> {
    pub(crate) id: JobIdAccessible<K>,
    pub(crate) run: RunType<T>,
}

impl<K, T> JobPackage<K, T> {
    pub fn new(id: String, job_kind: K, run: RunType<T>) -> Self {
        Self {
            id: JobIdAccessible::new_public(id, job_kind),
            run,
        }
    }
}

pub enum RunType<T> {
    NoOutput(NoOutputRun),
    Output(JobRun<T>),
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct JobId<K> {
    pub id: String,
    pub job_kind: K,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub(crate) enum JobAccess {
    Public,   // Jobs requested outside the cache
    Internal, // Jobs requested inside the cache
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub(crate) struct JobIdAccessible<K> {
    pub access: JobAccess,
    pub job_id: JobId<K>,
}

impl<K> JobIdAccessible<K> {
    pub fn new_public(id: String, job_kind: K) -> Self {
        Self {
            job_id: JobId { id, job_kind },
            access: JobAccess::Public,
        }
    }

    pub fn into_internal(mut self) -> Self {
        self.access = JobAccess::Internal;
        self
    }
}
