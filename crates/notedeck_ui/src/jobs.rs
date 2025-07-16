use egui::TextureHandle;
use hashbrown::{HashMap, hash_map::RawEntryMut};
use notedeck::JobPool;
use poll_promise::Promise;

#[derive(Default)]
pub struct JobsCache {
    jobs: HashMap<JobIdOwned, JobState>,
}

pub enum JobState {
    Pending(Promise<Option<Result<Job, JobError>>>),
    Error(JobError),
    Completed(Job),
}

pub enum JobError {
    InvalidParameters,
}

#[derive(Debug)]
pub enum JobParams<'a> {
    Blurhash(BlurhashParams<'a>),
}

#[derive(Debug)]
pub enum JobParamsOwned {
    Blurhash(BlurhashParamsOwned),
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
        }
    }
}

#[derive(Debug)]
pub struct BlurhashParams<'a> {
    pub blurhash: &'a str,
    pub url: &'a str,
    pub ctx: &'a egui::Context,
}

#[derive(Debug)]
pub struct BlurhashParamsOwned {
    pub blurhash: String,
    pub url: String,
    pub ctx: egui::Context,
}

impl JobsCache {
    /*
    pub fn get_or_insert_with<
        'a,
        F: FnOnce(Option<JobParamsOwned>) -> Result<Job, JobError> + Send + 'static,
    >(
        &'a mut self,
        job_pool: &'a mut JobPool,
        jobid: &JobId,
        params: Option<JobParams>,
        run_job: F,
    ) -> &'a mut JobState {
        match self.jobs.raw_entry_mut().from_key(jobid) {
            RawEntryMut::Occupied(entry) => 's: {
                let mut state = entry.into_mut();

                let JobState::Pending(promise) = &mut state else {
                    break 's state;
                };

                let Some(res) = promise.ready_mut() else {
                    break 's state;
                };

                let Some(res) = res.take() else {
                    tracing::error!("Failed to take the promise for job: {:?}", jobid);
                    break 's state;
                };

                *state = match res {
                    Ok(j) => JobState::Completed(j),
                    Err(e) => JobState::Error(e),
                };

                state
            }
            RawEntryMut::Vacant(entry) => {
                let owned_params = params.map(JobParams::into);
                let wrapped: Box<dyn FnOnce() -> Option<Result<Job, JobError>> + Send + 'static> =
                    Box::new(move || Some(run_job(owned_params)));

                let promise = Promise::spawn_async(job_pool.schedule(wrapped));

                let (_, state) = entry.insert(jobid.into(), JobState::Pending(promise));

                state
            }
        }
    }
    */

    pub fn get(&self, jobid: &JobId) -> Option<&JobState> {
        self.jobs.get(jobid)
    }
}

impl<'a> From<&JobId<'a>> for JobIdOwned {
    fn from(jobid: &JobId<'a>) -> Self {
        match jobid {
            JobId::Blurhash(s) => JobIdOwned::Blurhash(s.to_string()),
        }
    }
}

impl hashbrown::Equivalent<JobIdOwned> for JobId<'_> {
    fn equivalent(&self, key: &JobIdOwned) -> bool {
        match (self, key) {
            (JobId::Blurhash(a), JobIdOwned::Blurhash(b)) => *a == b.as_str(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
enum JobIdOwned {
    Blurhash(String), // image URL
}

#[derive(Debug, Hash)]
pub enum JobId<'a> {
    Blurhash(&'a str), // image URL
}

pub enum Job {
    Blurhash(Option<TextureHandle>),
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Job::Blurhash(_) => write!(f, "Blurhash"),
        }
    }
}
