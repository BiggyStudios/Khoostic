//! Opening tracks off the player thread.
//!
//! Opening can take a while (Spotify fetches metadata, a decryption key and the file over the
//! network), so the controller hands jobs to one opener thread and keeps answering commands
//! meanwhile. Only the newest job matters: jobs that were overtaken are never opened.
//!
//! Spotify rations decryption keys (a few in a row, then a pause of ~20 s), so songs from the
//! network are handled with care: while the user skips through them nothing is fetched until
//! they stay on one for a moment, follow-ups are only fetched once a song has been playing a
//! while, and a "wait" from the source is waited out quietly instead of failing the song.

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::RwLock;

use crate::audio::{AudioSource, SourceError};
use crate::model::{EntryId, Track};
use crate::player::Resolvers;

/// A play request this soon after the previous one counts as skipping...
const SKIPPING: Duration = Duration::from_millis(2500);
/// ...and a network song is only fetched once no newer pick came for this long, plus
/// `DWELL_STEP` for every network song fetched in the last `RECENT` (the more keys were asked
/// for lately, the surer we want to be that this is the song).
const DWELL: Duration = Duration::from_millis(650);
const DWELL_STEP: Duration = Duration::from_millis(350);
const DWELL_MAX: Duration = Duration::from_millis(2500);
const RECENT: Duration = Duration::from_secs(20);
/// Local files are cheap: rapid presses only settle briefly.
const LOCAL_SKIPPING: Duration = Duration::from_millis(450);
const LOCAL_DWELL: Duration = Duration::from_millis(150);
/// A network follow-up is fetched this long after it was asked for (so never while skipping).
const PRELOAD_AFTER: Duration = Duration::from_secs(12);
/// Waits before retrying a source that is briefly unreachable.
const RETRY_AFTER: [Duration; 2] = [Duration::from_millis(800), Duration::from_millis(2000)];
/// A source that keeps asking to wait is given up on after this long.
const GIVE_UP: Duration = Duration::from_secs(120);

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

/// `held` shows until when a play job waits because its source asked to (for the player bar).
pub(crate) fn spawn(
    resolvers: Resolvers,
    held: Arc<RwLock<Option<Instant>>>,
) -> anyhow::Result<(Sender<Job>, Receiver<Opened>)> {
    let (jobs_tx, jobs) = crossbeam_channel::unbounded();
    let (done, done_rx) = crossbeam_channel::unbounded();
    thread::Builder::new()
        .name("khoostic-opener".into())
        .spawn(move || run(&resolvers, &jobs, &done, &held))?;
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

/// Waits `d` unless a job comes first (returned).
enum Wait {
    Done,
    Newer(Job),
    Closed,
}

fn wait(jobs: &Receiver<Job>, d: Duration) -> Wait {
    match jobs.recv_timeout(d) {
        Ok(j) => Wait::Newer(j),
        Err(RecvTimeoutError::Timeout) => Wait::Done,
        Err(RecvTimeoutError::Disconnected) => Wait::Closed,
    }
}

fn run(
    resolvers: &Resolvers,
    jobs: &Receiver<Job>,
    done: &Sender<Opened>,
    held: &RwLock<Option<Instant>>,
) {
    // When the latest play request came in (to tell skipping from picking a song).
    let mut last_play: Option<Instant> = None;
    // Network songs opened to play lately.
    let mut fetched: VecDeque<Instant> = VecDeque::new();
    let mut next: Option<Job> = None;
    'jobs: loop {
        let job = match next.take() {
            Some(j) => j,
            None => match jobs.recv() {
                Ok(j) => j,
                Err(_) => return,
            },
        };
        let job = drain(job, jobs);
        let network = resolvers.network(&job.track);

        // Hold off: until skipping stops, or (follow-ups from the network) for a while.
        let hold = match job.purpose {
            Purpose::Play => {
                let since = last_play.map(|t| t.elapsed());
                last_play = Some(Instant::now());
                while fetched.front().is_some_and(|t| t.elapsed() > RECENT) {
                    fetched.pop_front();
                }
                let (window, dwell) = if network {
                    let busy = DWELL_STEP * fetched.len() as u32;
                    (SKIPPING, (DWELL + busy).min(DWELL_MAX))
                } else {
                    (LOCAL_SKIPPING, LOCAL_DWELL)
                };
                if since.is_some_and(|s| s < window) {
                    dwell
                } else {
                    Duration::ZERO
                }
            }
            Purpose::Preload if network => PRELOAD_AFTER,
            Purpose::Preload => Duration::ZERO,
        };
        if !hold.is_zero() {
            match wait(jobs, hold) {
                Wait::Done => {}
                Wait::Newer(newer) => {
                    next = Some(pick(job, newer));
                    continue 'jobs;
                }
                Wait::Closed => return,
            }
        }

        if network && job.purpose == Purpose::Play {
            fetched.push_back(Instant::now());
        }
        let started = Instant::now();
        let mut attempt = 0;
        let result = loop {
            let opened = resolvers.open(&job.track).map(|mut s| {
                if !job.start_at.is_zero() {
                    let _ = s.seek(job.start_at);
                }
                s
            });
            let mut asked = false;
            let pause = match &opened {
                // The source says when: wait that long (quietly; the song shows as loading).
                Err(SourceError::RetryLater { after, .. }) if started.elapsed() < GIVE_UP => {
                    asked = true;
                    *after + Duration::from_millis(250)
                }
                Err(e) if e.is_temporary() && attempt < RETRY_AFTER.len() => {
                    attempt += 1;
                    RETRY_AFTER[attempt - 1]
                }
                _ => break opened,
            };
            tracing::info!(track = %job.track.title, ?pause, "waiting to retry");
            if asked && job.purpose == Purpose::Play {
                *held.write() = Some(Instant::now() + pause);
            }
            let waited = wait(jobs, pause);
            *held.write() = None;
            match waited {
                Wait::Done => {}
                Wait::Newer(newer) => {
                    next = Some(pick(job.clone(), newer));
                    continue 'jobs;
                }
                Wait::Closed => return,
            }
        };
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

    /// Like Spotify: from the network, and refusing (for a moment) when the title says so.
    struct Streamed(Arc<AtomicUsize>);

    impl SourceResolver for Streamed {
        fn accepts(&self, _: &TrackLocator) -> bool {
            true
        }

        fn open(&self, track: &Track) -> Result<Box<dyn AudioSource>, SourceError> {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            if track.title == "refused" && n == 0 {
                return Err(SourceError::RetryLater {
                    after: Duration::from_millis(300),
                    message: "wait".into(),
                });
            }
            Err(SourceError::Unsupported("test".into()))
        }

        fn network(&self) -> bool {
            true
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
        let (tx, rx) = spawn(
            Resolvers::new().with(Counting(opens.clone())),
            Default::default(),
        )
        .unwrap();
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
    fn skipping_steadily_through_network_songs_fetches_none_of_the_skipped_ones() {
        let opens = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(RwLock::new(None));
        let (tx, rx) = spawn(Resolvers::new().with(Streamed(opens.clone())), held).unwrap();
        // A press every 0.5 s, slower than local-file skipping but faster than the dwell.
        for t in 0..8 {
            tx.send(job(t, "x", Purpose::Play)).unwrap();
            thread::sleep(Duration::from_millis(500));
        }
        let mut tickets = vec![];
        while let Ok(o) = rx.recv_timeout(Duration::from_millis(1500)) {
            tickets.push(o.ticket);
        }
        assert_eq!(
            tickets,
            vec![0, 7],
            "the first press, then where the skipping stopped"
        );
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_refusal_is_waited_out_and_shown_not_reported() {
        let opens = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(RwLock::new(None));
        let (tx, rx) = spawn(Resolvers::new().with(Streamed(opens.clone())), held.clone()).unwrap();
        tx.send(job(1, "refused", Purpose::Play)).unwrap();
        thread::sleep(Duration::from_millis(150));
        assert!(held.read().is_some(), "the wait shows");
        let o = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            matches!(o.result, Err(SourceError::Unsupported(_))),
            "tried again after"
        );
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert!(held.read().is_none());
    }

    #[test]
    fn temporary_failures_are_retried_unless_overtaken() {
        let opens = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = spawn(
            Resolvers::new().with(Counting(opens.clone())),
            Default::default(),
        )
        .unwrap();
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
