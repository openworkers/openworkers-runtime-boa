//! Job executor for a single worker.
//!
//! Boa's `SimpleJobExecutor` spins the CPU until every timer is due and cannot be
//! stopped early, which holds a request open for the delay of a timer nobody awaits.

use boa_engine::Context;
use boa_engine::JsResult;
use boa_engine::context::time::JsInstant;
use boa_engine::job::GenericJob;
use boa_engine::job::IntervalJob;
use boa_engine::job::Job;
use boa_engine::job::JobExecutor;
use boa_engine::job::NativeAsyncJob;
use boa_engine::job::PromiseJob;
use boa_engine::job::TimeoutJob;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

/// Why [`WorkerJobs::run_until`] stopped.
pub enum Stop {
    /// The caller's condition held, or nothing was left to run.
    Idle,
    /// The wall clock budget ran out.
    Timeout,
}

enum ClockJob {
    Timeout(TimeoutJob),
    Interval(IntervalJob),
}

impl ClockJob {
    fn cancelled(&self) -> bool {
        match self {
            ClockJob::Timeout(job) => job.cancelled(),
            ClockJob::Interval(job) => job.cancelled(),
        }
    }
}

#[derive(Default)]
pub struct WorkerJobs {
    promise: RefCell<VecDeque<PromiseJob>>,
    generic: RefCell<VecDeque<GenericJob>>,
    futures: RefCell<VecDeque<NativeAsyncJob>>,
    clock: RefCell<BTreeMap<JsInstant, Vec<ClockJob>>>,
}

impl WorkerJobs {
    /// Drops everything still queued, cancelling the timers among it.
    pub fn clear(&self) {
        self.promise.borrow_mut().clear();
        self.generic.borrow_mut().clear();
        self.futures.borrow_mut().clear();
        self.clock.borrow_mut().clear();
    }

    /// Runs queued work until `done` holds, nothing is left to run, or `deadline`
    /// passes.
    pub async fn run_until(
        &self,
        context: &RefCell<&mut Context>,
        deadline: Option<Instant>,
        done: &dyn Fn() -> bool,
    ) -> JsResult<Stop> {
        loop {
            self.run_ready(&mut context.borrow_mut())?;

            if done() {
                return Ok(Stop::Idle);
            }

            let budget = match deadline {
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(left) => Some(left),
                    None => return Ok(Stop::Timeout),
                },
                None => None,
            };

            // One at a time, so an outbound fetch settles before the next starts
            let job = self.futures.borrow_mut().pop_front();

            if let Some(job) = job {
                match budget {
                    Some(budget) => match tokio::time::timeout(budget, job.call(context)).await {
                        Ok(result) => result?,
                        Err(_) => return Ok(Stop::Timeout),
                    },
                    None => job.call(context).await?,
                };

                continue;
            }

            let Some(wait) = self.until_next_clock_job(&context.borrow()) else {
                return Ok(Stop::Idle);
            };

            match budget {
                Some(budget) if budget <= wait => {
                    tokio::time::sleep(budget).await;
                    return Ok(Stop::Timeout);
                }
                _ => tokio::time::sleep(wait).await,
            }
        }
    }

    /// Runs the microtasks, then the timers that have come due, then whatever
    /// those timers queued.
    fn run_ready(&self, context: &mut Context) -> JsResult<()> {
        self.drain_microtasks(context)?;

        for job in self.due_clock_jobs(context) {
            match job {
                ClockJob::Timeout(job) => {
                    job.call(context)?;
                }
                ClockJob::Interval(job) => {
                    let next = context.clock().now() + job.interval();
                    job.call(context)?;
                    self.clock
                        .borrow_mut()
                        .entry(next)
                        .or_default()
                        .push(ClockJob::Interval(job));
                }
            }
        }

        self.drain_microtasks(context)
    }

    fn drain_microtasks(&self, context: &mut Context) -> JsResult<()> {
        loop {
            let promise = self.promise.borrow_mut().pop_front();

            if let Some(job) = promise {
                job.call(context)?;
                continue;
            }

            let Some(job) = self.generic.borrow_mut().pop_front() else {
                context.clear_kept_objects();
                return Ok(());
            };

            job.call(context)?;
        }
    }

    fn due_clock_jobs(&self, context: &mut Context) -> Vec<ClockJob> {
        let now = context.clock().now();
        let mut clock = self.clock.borrow_mut();
        let later = clock.split_off(&now);
        let due = std::mem::replace(&mut *clock, later);

        due.into_values()
            .flatten()
            .filter(|job| !job.cancelled())
            .collect()
    }

    fn until_next_clock_job(&self, context: &Context) -> Option<Duration> {
        let next = *self.clock.borrow().keys().next()?;
        Some((next - context.clock().now()).into())
    }
}

impl JobExecutor for WorkerJobs {
    fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context) {
        let now = context.clock().now();

        match job {
            Job::PromiseJob(job) => self.promise.borrow_mut().push_back(job),
            Job::GenericJob(job) => self.generic.borrow_mut().push_back(job),
            Job::AsyncJob(job) => self.futures.borrow_mut().push_back(job),
            Job::TimeoutJob(job) => self
                .clock
                .borrow_mut()
                .entry(now + job.timeout())
                .or_default()
                .push(ClockJob::Timeout(job)),
            Job::IntervalJob(job) => self
                .clock
                .borrow_mut()
                .entry(now + job.interval())
                .or_default()
                .push(ClockJob::Interval(job)),
            // FinalizationRegistry cleanup is optional, and its job never completes
            _ => {}
        }
    }

    /// Runs what is ready; waiting on timers and futures needs [`Self::run_until`].
    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        self.run_ready(context)
    }
}
