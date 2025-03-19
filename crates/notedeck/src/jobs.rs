use egui::TextureHandle;
use hashbrown::{hash_map::RawEntryMut, HashMap};
use poll_promise::Promise;

#[derive(Default)]
pub struct Jobs {
    jobs: HashMap<JobIdOwned, Job>,
}

impl Jobs {
    pub fn get_or_insert_with<'a, T: FnOnce() -> Job>(
        &'a mut self,
        jobid: &JobId,
        default: T,
    ) -> &'a mut Job {
        match self.jobs.raw_entry_mut().from_key(jobid) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => entry.insert(to_owned(jobid), default()).1,
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

fn to_owned(jobid: &JobId) -> JobIdOwned {
    match jobid {
        JobId::Blurhash(s) => JobIdOwned::Blurhash(s.to_string()),
        JobId::NWCBalance(s) => JobIdOwned::NWCBalance(s.to_string()),
        JobId::NWCInvoice(s) => JobIdOwned::NWCInvoice(s.to_string()),
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
    Blurhash(String), // image URL
    #[allow(dead_code)]
    NWCBalance(String), // Wallet's URI
    #[allow(dead_code)]
    NWCInvoice(String), // LN invoice
}

pub enum JobId<'a> {
    Blurhash(&'a str),   // image URL
    NWCBalance(&'a str), // Wallet's URI
    NWCInvoice(&'a str), // LN invoice
}

pub enum Job {
    ProcessBlurhash(Promise<Option<TextureHandle>>),
    GetNWCBalance(Promise<Result<u64, nwc::Error>>),
    PayNWCInvoice(Promise<Result<nwc::nostr::nips::nip47::PayInvoiceResponse, nwc::Error>>),
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
