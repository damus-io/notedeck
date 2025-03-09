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
        }
    }
}

impl std::hash::Hash for JobIdOwned {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            JobIdOwned::Blurhash(s) => s.hash(state),
        }
    }
}

fn to_owned(jobid: &JobId) -> JobIdOwned {
    match jobid {
        JobId::Blurhash(s) => JobIdOwned::Blurhash(s.to_string()),
    }
}

impl hashbrown::Equivalent<JobIdOwned> for JobId<'_> {
    fn equivalent(&self, key: &JobIdOwned) -> bool {
        match (self, key) {
            (JobId::Blurhash(a), JobIdOwned::Blurhash(b)) => *a == b.as_str(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum JobIdOwned {
    Blurhash(String), // image URL
}

pub enum JobId<'a> {
    Blurhash(&'a str), // image URL
}

pub enum Job {
    ProcessBlurhash(Promise<Option<TextureHandle>>),
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Job::ProcessBlurhash(_) => write!(f, "ProcessBlurhash"),
        }
    }
}
