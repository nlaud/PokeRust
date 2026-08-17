//! A bounded worker pool for the exact solver.
//!
//! `simulate_turn` causes most of the search cost. One matrix cell holds one
//! independent subtree, so many cells can run at the same time.
//!
//! This module holds three parts:
//!
//! 1. [`WorkerPool`], a count of permits that bounds the extra threads.
//! 2. [`run_jobs`], one batch of jobs over a fixed set of workers.
//! 3. [`job_seed`], the random seed of one job.
//!
//! # The permit count
//!
//! [`shared`] returns the pool of this process. Its capacity comes from
//! [`std::thread::available_parallelism`]. Every solve of the process draws from
//! that one count, so the total extra thread count stays bounded.
//!
//! [`WorkerPool::acquire`] never blocks. It returns the permits that are free,
//! and it can return none. A batch with no permit runs on the calling thread
//! alone. The pool therefore slows a solve down, and it never stops one.
//!
//! The calling thread takes no permit. It runs jobs beside the extra threads, so
//! a batch with `n` permits uses `n + 1` threads.
//!
//! # Determinism
//!
//! [`run_jobs`] returns one value for each job, in job order. The thread
//! schedule selects which worker runs which job. It does not select the order of
//! the answer.
//!
//! A job that reads the random generator must install its own. The simulator
//! generator is a thread-local override, so an extra thread starts with none.
//! [`job_seed`] builds a seed from the identity of the job. The seed does not
//! depend on the thread schedule.
//!
//! Read the `search` module for the rules that make one cell value exact.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::thread;

/// The permits that one process gives to every solve together.
///
/// A permit stands for one extra thread. The pool holds a free count and a
/// capacity. `acquire` lowers the free count, and the returned guard raises it
/// again.
#[derive(Debug)]
pub struct WorkerPool {
    free: AtomicUsize,
    capacity: usize,
}

impl WorkerPool {
    /// A pool with `capacity` permits, all free.
    ///
    /// The tests build their own pool. A private pool cannot see the permits of
    /// another test, so a test result does not depend on the test schedule.
    pub fn new(capacity: usize) -> Self {
        WorkerPool {
            free: AtomicUsize::new(capacity),
            capacity,
        }
    }

    /// The permit count of an empty pool.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The permits that are free now.
    ///
    /// Another thread can take one of these permits before the caller reads the
    /// answer. Only a test that holds the whole pool can trust this number.
    pub fn free(&self) -> usize {
        self.free.load(Ordering::Acquire)
    }

    /// Takes up to `wanted` permits, and never blocks.
    ///
    /// The guard returns the permits when it drops. A caller that gets zero
    /// permits must run its work on its own thread.
    pub fn acquire(&self, wanted: usize) -> Permits<'_> {
        let mut free = self.free.load(Ordering::Acquire);
        loop {
            let take = wanted.min(free);
            if take == 0 {
                return Permits { pool: self, count: 0 };
            }
            match self.free.compare_exchange_weak(
                free,
                free - take,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Permits { pool: self, count: take },
                Err(actual) => free = actual,
            }
        }
    }
}

/// The permits of one batch.
///
/// The guard returns the permits to its pool when it drops. A batch must hold
/// the guard until every extra thread ends.
#[derive(Debug)]
pub struct Permits<'a> {
    pool: &'a WorkerPool,
    count: usize,
}

impl Permits<'_> {
    /// The permit count of this guard.
    pub fn count(&self) -> usize {
        self.count
    }
}

impl Drop for Permits<'_> {
    fn drop(&mut self) {
        if self.count > 0 {
            self.pool.free.fetch_add(self.count, Ordering::AcqRel);
        }
    }
}

/// The pool of this process.
///
/// The capacity comes from [`std::thread::available_parallelism`]. A machine
/// that reports no count gives a capacity of 1.
pub fn shared() -> &'static WorkerPool {
    static SHARED: OnceLock<WorkerPool> = OnceLock::new();
    SHARED.get_or_init(|| {
        let cores = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        WorkerPool::new(cores)
    })
}

/// The seed of one job.
///
/// `root` names the position that owns the batch. The other three fields name
/// the job inside that position. The same five inputs always give the same
/// seed, and the thread schedule changes none of them.
pub fn job_seed(root: u64, depth: u8, round: usize, row: usize, col: usize) -> u64 {
    // SplitMix64. It spreads a small change of one field across every output
    // bit, and it needs no allocation.
    let mut mixed = root;
    for field in [depth as u64, round as u64, row as u64, col as u64] {
        mixed = mix(mixed ^ field.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }
    mix(mixed)
}

/// One SplitMix64 finalizer round.
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Runs `jobs` jobs over `workers`, and returns one value for each job.
///
/// Worker 0 runs on the calling thread. Each other worker runs on its own
/// thread, and every thread ends before this function returns. An atomic index
/// gives the next job to the first free worker.
///
/// The answer holds one value for each job index, in job order. The caller reads
/// that order, not the order of completion.
///
/// One worker holds one `&mut W` for the whole batch. A worker therefore keeps
/// its own caches and counters between two jobs.
///
/// # Panics
///
/// A panic inside a job stops the batch. This function then raises the same
/// panic on the calling thread.
pub(crate) fn run_jobs<W, T, F>(workers: &mut [&mut W], jobs: usize, run: F) -> Vec<T>
where
    W: Send,
    T: Send,
    F: Fn(&mut W, usize) -> T + Sync,
{
    if jobs == 0 || workers.is_empty() {
        return Vec::new();
    }
    if workers.len() == 1 {
        let worker = &mut *workers[0];
        return (0..jobs).map(|job| run(worker, job)).collect();
    }

    let next = AtomicUsize::new(0);
    let (first, rest) = workers.split_at_mut(1);
    let collected: Vec<Vec<(usize, T)>> = thread::scope(|scope| {
        let handles: Vec<_> = rest
            .iter_mut()
            .map(|worker| {
                let worker: &mut W = worker;
                let next = &next;
                let run = &run;
                scope.spawn(move || take_jobs(worker, jobs, next, run))
            })
            .collect();
        // The calling thread is worker 0. It works beside the extra threads
        // rather than waiting for them.
        let mine = take_jobs(&mut *first[0], jobs, &next, &run);

        let mut collected = Vec::with_capacity(1 + handles.len());
        collected.push(mine);
        for handle in handles {
            match handle.join() {
                Ok(done) => collected.push(done),
                // Raise the original panic. A lost job would otherwise become a
                // missing cell, and the caller could not name the cause.
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        collected
    });

    let mut slots: Vec<Option<T>> = (0..jobs).map(|_| None).collect();
    for worker in collected {
        for (job, value) in worker {
            slots[job] = Some(value);
        }
    }
    slots
        .into_iter()
        .map(|slot| slot.expect("the job index rises by one, so every job runs one time"))
        .collect()
}

/// Runs jobs from the shared index until the index passes the job count.
fn take_jobs<W, T, F>(worker: &mut W, jobs: usize, next: &AtomicUsize, run: &F) -> Vec<(usize, T)>
where
    F: Fn(&mut W, usize) -> T,
{
    let mut done = Vec::new();
    loop {
        let job = next.fetch_add(1, Ordering::Relaxed);
        if job >= jobs {
            return done;
        }
        done.push((job, run(worker, job)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_returns_one_value_for_each_job_in_job_order() {
        let mut counters = [0usize, 0, 0, 0];
        let mut workers: Vec<&mut usize> = counters.iter_mut().collect();
        let values = run_jobs(&mut workers, 200, |count: &mut usize, job| {
            *count += 1;
            job * 2
        });

        assert_eq!(values.len(), 200);
        for (job, value) in values.iter().enumerate() {
            assert_eq!(*value, job * 2, "job {job}");
        }
        assert_eq!(counters.iter().sum::<usize>(), 200);
    }

    #[test]
    fn one_worker_runs_every_job_on_the_calling_thread() {
        let mut only = 0usize;
        let mut workers: Vec<&mut usize> = vec![&mut only];
        let values = run_jobs(&mut workers, 5, |count: &mut usize, job| {
            *count += 1;
            job
        });
        assert_eq!(values, vec![0, 1, 2, 3, 4]);
        assert_eq!(only, 5);
    }
}
