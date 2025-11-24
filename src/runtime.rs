use boa_engine::job::{Job, JobExecutor, NativeAsyncJob, PromiseJob, TimeoutJob};
use boa_engine::{Context, JsResult};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

/// Job queue that uses tokio for async execution
pub struct TokioJobQueue {
    async_jobs: RefCell<VecDeque<NativeAsyncJob>>,
    promise_jobs: RefCell<VecDeque<PromiseJob>>,
    timeout_jobs: RefCell<BTreeMap<boa_engine::context::time::JsInstant, TimeoutJob>>,
}

impl TokioJobQueue {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            async_jobs: RefCell::default(),
            promise_jobs: RefCell::default(),
            timeout_jobs: RefCell::default(),
        })
    }

    fn drain_timeout_jobs(&self, context: &mut Context) {
        let now = context.clock().now();

        let mut timeouts_borrow = self.timeout_jobs.borrow_mut();
        let mut jobs_to_keep = timeouts_borrow.split_off(&now);
        jobs_to_keep.retain(|_, job| !job.is_cancelled());
        let jobs_to_run = std::mem::replace(&mut *timeouts_borrow, jobs_to_keep);
        drop(timeouts_borrow);

        for job in jobs_to_run.into_values() {
            if let Err(e) = job.call(context) {
                log::error!("Timeout job error: {}", e);
            }
        }
    }

    pub fn drain_jobs(&self, context: &mut Context) {
        // Run the timeout jobs first
        self.drain_timeout_jobs(context);

        // Run promise jobs
        let jobs = std::mem::take(&mut *self.promise_jobs.borrow_mut());
        for job in jobs {
            if let Err(e) = job.call(context) {
                log::error!("Promise job error: {}", e);
            }
        }

        context.clear_kept_objects();
    }

    /// Run all pending jobs (blocking version for sync contexts)
    pub fn run_all_jobs(&self, context: &mut Context) -> JsResult<()> {
        loop {
            self.drain_jobs(context);

            // Check if we have any pending jobs
            if self.promise_jobs.borrow().is_empty()
                && self.timeout_jobs.borrow().is_empty()
                && self.async_jobs.borrow().is_empty()
            {
                break;
            }

            // Small yield to allow timeouts to fire
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(())
    }
}

impl JobExecutor for TokioJobQueue {
    fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context) {
        match job {
            Job::PromiseJob(job) => self.promise_jobs.borrow_mut().push_back(job),
            Job::AsyncJob(job) => self.async_jobs.borrow_mut().push_back(job),
            Job::TimeoutJob(t) => {
                let now = context.clock().now();
                self.timeout_jobs.borrow_mut().insert(now + t.timeout(), t);
            }
            _ => {
                log::warn!("Unsupported job type");
            }
        }
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        // Just drain once per call from Boa
        self.drain_jobs(context);
        Ok(())
    }
}
