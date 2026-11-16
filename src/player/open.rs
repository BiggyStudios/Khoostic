//! Opening tracks off the player thread.
//!
//! Opening can take a while (Spotify fetches metadata, a file and a decryption key over the
//! network), so the controller hands jobs to one opener thread and keeps answering commands
//! meanwhile. Only the newest job matters: jobs that were overtaken are never opened, rapid
//! skipping settles on the last pick before anything is fetched, and a source that is only
//! briefly unavailable (Spotify throttling key requests) is retried a couple of times.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::audio::{AudioSource, SourceError};
use crate::model::{EntryId, Track};
use crate::player::Resolvers;

/// A play request this soon after the previous one counts as rapid skipping...
const SPAM_WINDOW: Duration = Duration::from_millis(450);
/// ...and waits this long for the next press before opening anything.
const SETTLE: Duration = Duration::from_millis(180);
/// Waits before retrying a temporarily unavailable source.
const RETRY_AFTER: [Duration; 2] = [Duration::from_millis(800), Duration::from_millis(2000)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// Start playing it.
    Play,
    /// The gapless follow-up.
    Preload,
}

#[derive(Clone)]
pub(crate) struct Job {
    /// The controller's ticket for this job; results with an outdated ticket are dropped.
    pub ticket: u64,
    pub id: EntryId,
    pub track: Arc<Track>,
    pub purpose: Purpose,
    /// Start this far in (jam guests joining mid-song).
    pub start_at: Duration,
}

pub(crate) struct Opened {
    pub ticket: u64,
    pub id: EntryId,
    pub purpose: Purpose,
    pub result: Result<Box<dyn AudioSource>, SourceError>,
}

pub(crate) fn spawn(resolvers: Resolvers) -> anyhow::Result<(Sender<Job>, Receiver<Opened>)> {
    let (jobs_tx, jobs) = crossbeam_channel::unbounded();
    let (done, done_rx) = crossbeam_channel::unbounded();
    thread::Builder::new()
        .name("khoostic-opener".into())
        .spawn(move || run(&resolvers, &jobs, &done))?;
    Ok((jobs_tx, done_rx))
}

/// The job to keep when `newer` arrives while `current` waits: the newest, except that a
/// preload never pushes out a pending play.
fn pick(current: Job, newer: Job) -> Job {
    if current.purpose == Purpose::Play && newer.purpose == Purpose::Preload {
        current
    } else {
        newer
    }
}

fn drain(mut job: Job, jobs: &Receiver<Job>) -> Job {
    while let Ok(newer) = jobs.try_recv() {
        job = pick(job, newer);
    }
    job
}

fn run(resolvers: &Resolvers, jobs: &Receiver<Job>, done: &Sender<Opened>) {
    let mut last_play: Option<Instant> = None;
    let mut next: Option<Job> = None;
    loop {
        let job = match next.take() {
            Some(j) => j,
            None => match jobs.recv() {
                Ok(j) => j,
                Err(_) => return,
            },
        };
        let mut job = drain(job, jobs);
        if job.purpose == Purpose::Play {
            if last_play.is_some_and(|t| t.elapsed() < SPAM_WINDOW) {
                while let Ok(newer) = jobs.recv_timeout(SETTLE) {
                    job = pick(job, newer);
                }
            }
            last_play = Some(Instant::now());
        }

        let mut attempt = 0;
        let result = loop {
            match resolvers.open(&job.track).map(|mut s| {
                if !job.start_at.is_zero() {
                    let _ = s.seek(job.start_at);
                }
                s
            }) {
                Err(e) if e.is_temporary() && attempt < RETRY_AFTER.len() => {
                    tracing::info!(track = %job.track.title, "retrying after: {e}");
                    match jobs.recv_timeout(RETRY_AFTER[attempt]) {
                        // Something newer was asked for: drop this one.
                        Ok(newer) => {
                            next = Some(pick(job.clone(), newer));
                            break None;
                        }
                        Err(RecvTimeoutError::Timeout) => attempt += 1,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                r => break Some(r),
            }
        };
        let Some(result) = result else { continue };
        let opened = Opened {
            ticket: job.ticket,
            id: job.id,
            purpose: job.purpose,
            result,
        };
        if done.send(opened).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::model::TrackLocator;
    use crate::player::SourceResolver;

    /// Counts opens; fails every one (temporarily for "flaky" paths).
    struct Counting(Arc<AtomicUsize>);

    impl SourceResolver for Counting {
        fn accepts(&self, _: &TrackLocator) -> bool {
            true
        }

        fn open(&self, track: &Track) -> Result<Box<dyn AudioSource>, SourceError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(30));
            if track.title == "flaky" {
                Err(SourceError::Unavailable("key service busy".into()))
            } else {
                Err(SourceError::Unsupported("test".into()))
            }
        }
    }

    fn job(ticket: u64, title: &str, purpose: Purpose) -> Job {
        let mut track = Track::from_path(format!("C:/{title}.flac"));
        track.title = title.into();
        Job {
            ticket,
            id: EntryId(ticket),
            track: Arc::new(track),
            purpose,
            start_at: Duration::ZERO,
        }
    }

    #[test]
    fn rapid_skips_open_only_the_first_and_the_last() {
        let opens = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = spawn(Resolvers::new().with(Counting(opens.clone()))).unwrap();
        for t in 0..20 {
            tx.send(job(t, "x", Purpose::Play)).unwrap();
            thread::sleep(Duration::from_millis(15));
        }
        let mut tickets = vec![];
        while let Ok(o) = rx.recv_timeout(Duration::from_millis(600)) {
            tickets.push(o.ticket);
        }
        assert_eq!(tickets.last(), Some(&19));
        assert!(opens.load(Ordering::SeqCst) <= 3, "{tickets:?}");
    }

    #[test]
    fn temporary_failures_are_retried_unless_overtaken() {
        let opens = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = spawn(Resolvers::new().with(Counting(opens.clone()))).unwrap();
        tx.send(job(1, "flaky", Purpose::Play)).unwrap();
        let o = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(o.result.is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 3, "first try + two retries");

        opens.store(0, Ordering::SeqCst);
        tx.send(job(2, "flaky", Purpose::Play)).unwrap();
        thread::sleep(Duration::from_millis(200));
        tx.send(job(3, "solid", Purpose::Play)).unwrap();
        let o = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(o.ticket, 3, "the overtaken job reports nothing");
    }

    #[test]
    fn a_preload_never_pushes_out_a_play() {
        let kept = pick(job(1, "a", Purpose::Play), job(2, "b", Purpose::Preload));
        assert_eq!(kept.ticket, 1);
        let kept = pick(job(1, "a", Purpose::Preload), job(2, "b", Purpose::Play));
        assert_eq!(kept.ticket, 2);
    }
}
