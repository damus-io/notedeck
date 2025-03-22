use std::{future::Future, sync::Arc};

use crossbeam::queue::ArrayQueue;
use egui::TextureHandle;
use hashbrown::{hash_map::RawEntryMut, HashMap};
use tokio::sync::Semaphore;

pub struct Jobs {
    semaphore: Arc<Semaphore>,
    pending: Arc<ArrayQueue<CompletedJob>>,
    jobs: HashMap<JobIdOwned, JobState>,
}

pub enum JobState {
    Pending,
    Error(JobError),
    Completed(Job),
}

pub enum JobError {
    InvalidParameters,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs::new(10)
    }
}

struct CompletedJob {
    id: JobIdOwned,
    job: Result<Job, JobError>,
}

#[derive(Debug)]
pub enum JobParams<'a> {
    Blurhash(BlurhashParams<'a>),
    NWCInvoice(NWCInvoiceParams<'a>),
}

pub enum JobParamsOwned {
    Blurhash(BlurhashParamsOwned),
    NWCInvoice(NWCInvoiceParamsOwned),
}

impl<'a> From<BlurhashParams<'a>> for BlurhashParamsOwned {
    fn from(params: BlurhashParams<'a>) -> Self {
        BlurhashParamsOwned {
            blurhash: params.blurhash.to_owned(),
            url: params.url.to_owned(),
            ctx: params.ctx.clone(),
        }
    }
}

impl<'a> From<JobParams<'a>> for JobParamsOwned {
    fn from(params: JobParams<'a>) -> Self {
        match params {
            JobParams::Blurhash(bp) => JobParamsOwned::Blurhash(bp.into()),
            JobParams::NWCInvoice(nwcinvoice_params) => {
                JobParamsOwned::NWCInvoice(nwcinvoice_params.into())
            }
        }
    }
}

#[derive(Debug)]
pub struct BlurhashParams<'a> {
    pub blurhash: &'a str,
    pub url: &'a str,
    pub ctx: &'a egui::Context,
}

pub struct BlurhashParamsOwned {
    pub blurhash: String,
    pub url: String,
    pub ctx: egui::Context,
}

#[derive(Debug)]
pub struct NWCInvoiceParams<'a> {
    pub invoice: &'a str,
}

impl<'a> From<NWCInvoiceParams<'a>> for NWCInvoiceParamsOwned {
    fn from(value: NWCInvoiceParams<'a>) -> Self {
        Self {
            invoice: value.invoice.into(),
        }
    }
}

pub struct NWCInvoiceParamsOwned {
    pub invoice: String,
}

impl Jobs {
    pub fn new(size: usize) -> Self {
        Self {
            jobs: Default::default(),
            semaphore: Arc::new(Semaphore::const_new(size)),
            pending: Arc::new(ArrayQueue::new(size)),
        }
    }

    pub fn get_or_insert_with<
        'a,
        T: FnOnce(Option<JobParamsOwned>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Job, JobError>> + Send,
    >(
        &'a mut self,
        jobid: &JobId,
        params: Option<JobParams>,
        run_job: T,
    ) -> &'a mut JobState {
        self.move_completed();
        match self.jobs.raw_entry_mut().from_key(jobid) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => {
                let (id, state) = entry.insert(jobid.into(), JobState::Pending);

                let semaphore = self.semaphore.clone();

                let id = id.clone();
                let perform_job = Box::new(run_job);

                let pending = self.pending.clone();

                let params = params.map(|p| p.into());
                tokio::spawn(async move {
                    let _ = semaphore.acquire().await;

                    let job = perform_job(params).await;
                    pending.push(CompletedJob { id, job })
                });

                state
            }
        }
    }

    fn move_completed(&mut self) {
        while let Some(completed) = self.pending.pop() {
            let state = match completed.job {
                Ok(j) => JobState::Completed(j),
                Err(e) => JobState::Error(e),
            };

            self.jobs.insert(completed.id, state);
        }
    }
}

// The hash of each JobId case must match the corresponding JobIdOwned case.
// Otherwise, odd things start happening in the HashMap.
impl std::hash::Hash for JobId<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            JobId::Blurhash(s) => s.hash(state),
            JobId::NWCBalance(s) => s.hash(state),
            JobId::NWCInvoice(s) => s.hash(state),
        }
    }
}

impl std::hash::Hash for JobIdOwned {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            JobIdOwned::Blurhash(s) => s.hash(state),
            JobIdOwned::NWCBalance(s) => s.hash(state),
            JobIdOwned::NWCInvoice(s) => s.hash(state),
        }
    }
}

impl<'a> From<&JobId<'a>> for JobIdOwned {
    fn from(jobid: &JobId<'a>) -> Self {
        match jobid {
            JobId::Blurhash(s) => JobIdOwned::Blurhash(s.to_string()),
            JobId::NWCBalance(s) => JobIdOwned::NWCBalance(s.to_string()),
            JobId::NWCInvoice(s) => JobIdOwned::NWCInvoice(s.to_string()),
        }
    }
}

impl hashbrown::Equivalent<JobIdOwned> for JobId<'_> {
    fn equivalent(&self, key: &JobIdOwned) -> bool {
        match (self, key) {
            (JobId::Blurhash(a), JobIdOwned::Blurhash(b)) => *a == b.as_str(),
            (JobId::NWCBalance(a), JobIdOwned::NWCBalance(b)) => *a == b.as_str(),
            (JobId::NWCInvoice(a), JobIdOwned::NWCInvoice(b)) => *a == b.as_str(),
            (_, _) => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum JobIdOwned {
    Blurhash(String),   // image URL
    NWCBalance(String), // Wallet's URI
    NWCInvoice(String), // LN invoice
}

pub enum JobId<'a> {
    Blurhash(&'a str),   // image URL
    NWCBalance(&'a str), // Wallet's URI
    NWCInvoice(&'a str), // LN invoice
}

pub enum Job {
    ProcessBlurhash(Option<TextureHandle>),
    GetNWCBalance(Result<u64, nwc::Error>),
    PayNWCInvoice(Result<nwc::nostr::nips::nip47::PayInvoiceResponse, nwc::Error>),
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Job::ProcessBlurhash(_) => write!(f, "ProcessBlurhash"),
            Job::GetNWCBalance(_) => write!(f, "GetNWCBalance"),
            Job::PayNWCInvoice(_) => write!(f, "PayNWCInvoice"),
        }
    }
}
