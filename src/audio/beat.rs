//! Beat tracking for the visualizers: hits that land on the beat, not on every sudden sound.
//!
//! 1. **Onsets.** Every hop, the rise of each band's level over the hop before last (compared
//!    with its neighbours, so a sliding vocal doesn't count) is summed into an onset strength:
//!    kicks, snares, a shouted "yeah" all show up as peaks.
//! 2. **Tempo.** The autocorrelation of the last ~6 s of onset strength finds the beat period
//!    (60–200 BPM, nudged toward common tempos, checked against its double). The tempo only
//!    switches when a new one holds for a while.
//! 3. **Phase.** The tracker predicts the next beat. An onset close to the prediction *is* the
//!    beat: it fires at once, its strength (how loud the onset is next to recent beats) makes
//!    accented beats hit harder, and the prediction is pulled toward it. Onsets elsewhere are
//!    not beats. When a beat passes with nothing there (a break, a quiet verse) a soft "ghost"
//!    beat keeps the pulse going.
//!
//! Until a tempo is found (the first seconds of a song) strong onsets are beats on their own.

use super::analysis::BANDS;

/// Tempo range searched.
const MIN_BPM: f32 = 60.0;
const MAX_BPM: f32 = 200.0;
/// Onset history the tempo is estimated from.
const HISTORY_SECS: f32 = 6.0;
/// Re-estimate the tempo this often.
const ESTIMATE_EVERY_SECS: f32 = 0.33;
/// How far (as a share of the period) an onset may be from a predicted beat and still be it.
const WINDOW: f32 = 0.17;
/// How far a beat on time pulls the prediction toward itself.
const PULL: f64 = 0.35;
/// Tempo confidence needed to follow the tempo, and below which it's let go again (strong
/// onsets are beats on their own then).
const LOCK: f32 = 0.18;
const UNLOCK: f32 = 0.09;

/// What happened this hop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    /// About 0.7 for an ordinary beat, more for accented ones, ~0.25 for a ghost.
    pub strength: f32,
    /// Predicted, with nothing heard there.
    pub ghost: bool,
}

pub(crate) struct BeatTracker {
    /// Hops per second.
    rate: f32,
    weights: [f32; BANDS],
    weight_sum: f32,
    /// Levels one and two hops ago.
    prev: [[f32; BANDS]; 2],
    hop: u64,
    /// Onset strength, newest last (ring).
    odf: Vec<f32>,
    odf_pos: usize,
    odf_filled: usize,
    odf_mean: f32,
    armed: bool,
    last_onset: u64,
    /// Strong onsets (for anchoring the phase).
    last_strong: Option<(u64, f32)>,
    /// Beat period in hops (0: not known).
    period: f32,
    pub confidence: f32,
    /// Following the tempo (see [`LOCK`]).
    locked: bool,
    /// A different tempo that keeps coming up, and how many estimates in a row.
    candidate: Option<(f32, u32)>,
    since_estimate: u32,
    next_beat: f64,
    /// A real beat already fired in the current window.
    fired: bool,
    /// Windows in a row with no beat heard.
    misses: u32,
    /// Estimates in a row where another grid fit clearly better.
    regrid: u32,
    /// Typical onset strength of recent beats.
    beat_level: f32,
    /// When the last beat (real or ghost) fired, for the phase.
    last_beat: f64,
}

impl BeatTracker {
    /// `centers`: each band's center frequency.
    pub(crate) fn new(hops_per_sec: f32, centers: &[f32; BANDS]) -> Self {
        // Kicks carry the beat most often; the rest still counts (snares, vocals, chords).
        let weights = centers.map(|hz| {
            if hz < 150.0 {
                1.6
            } else if hz < 4000.0 {
                1.0
            } else {
                0.6
            }
        });
        let len = (HISTORY_SECS * hops_per_sec) as usize;
        Self {
            rate: hops_per_sec,
            weight_sum: weights.iter().sum(),
            weights,
            prev: [[0.0; BANDS]; 2],
            hop: 0,
            odf: vec![0.0; len],
            odf_pos: 0,
            odf_filled: 0,
            odf_mean: 0.0,
            armed: true,
            last_onset: 0,
            last_strong: None,
            period: 0.0,
            confidence: 0.0,
            locked: false,
            candidate: None,
            since_estimate: 0,
            next_beat: 0.0,
            fired: false,
            misses: 0,
            regrid: 0,
            beat_level: 0.0,
            last_beat: 0.0,
        }
    }

    /// The tempo, once there is one.
    pub(crate) fn bpm(&self) -> Option<f32> {
        (self.period > 0.0 && self.locked).then(|| 60.0 * self.rate / self.period)
    }

    /// How far into the current beat we are (0 at a beat, 1 at the next).
    pub(crate) fn phase(&self) -> f32 {
        if self.period <= 0.0 {
            return 0.0;
        }
        ((self.hop as f64 - self.last_beat) / self.period as f64).clamp(0.0, 1.0) as f32
    }

    /// One hop of band levels (0..1, unsmoothed). Returns a beat when one lands on this hop.
    pub(crate) fn push(&mut self, levels: &[f32; BANDS]) -> Option<Beat> {
        self.hop += 1;
        let t = self.hop;

        // Onset strength: rises against the hop before last, neighbours included.
        let old = &self.prev[0];
        let mut flux = 0.0;
        for b in 0..BANDS {
            let lo = b.saturating_sub(1);
            let hi = (b + 1).min(BANDS - 1);
            let reference = old[lo].max(old[b]).max(old[hi]);
            flux += self.weights[b] * (levels[b] - reference).max(0.0);
        }
        flux /= self.weight_sum;
        self.prev[0] = self.prev[1];
        self.prev[1] = *levels;

        self.odf[self.odf_pos] = flux;
        self.odf_pos = (self.odf_pos + 1) % self.odf.len();
        self.odf_filled = (self.odf_filled + 1).min(self.odf.len());
        self.odf_mean += (flux - self.odf_mean) / self.rate; // ~1 s average

        let threshold = self.odf_mean * 1.5 + 0.004;
        let gap_ok = t - self.last_onset >= (self.rate * 0.1) as u64;
        let onset = self.armed && flux > threshold && gap_ok;
        if onset {
            self.armed = false;
            self.last_onset = t;
            if flux > self.odf_mean * 2.2 {
                self.last_strong = Some((t, flux));
            }
        } else if flux < threshold * 0.7 {
            self.armed = true;
        }

        self.since_estimate += 1;
        if self.since_estimate as f32 >= ESTIMATE_EVERY_SECS * self.rate
            && self.odf_filled as f32 >= self.rate * 3.0
        {
            self.since_estimate = 0;
            self.estimate_tempo();
        }

        let strength = |level: f32| {
            if level <= 0.0 {
                0.7
            } else {
                (0.7 * flux / level).clamp(0.35, 1.25)
            }
        };

        // No tempo yet: strong onsets are the beats.
        if self.period <= 0.0 || !self.locked {
            let spaced = t as f64 - self.last_beat >= (self.rate * 0.25) as f64;
            if onset && spaced && flux > self.odf_mean * 2.0 {
                let s = strength(self.beat_level);
                self.learn_level(flux);
                self.last_beat = t as f64;
                return Some(Beat {
                    strength: s,
                    ghost: false,
                });
            }
            return None;
        }

        let tol = (self.period * WINDOW).max(2.5) as f64;
        let mut beat = None;
        if onset && !self.fired {
            let err = t as f64 - self.next_beat;
            if err.abs() <= tol {
                beat = Some(Beat {
                    strength: strength(self.beat_level),
                    ghost: false,
                });
                self.learn_level(flux);
                self.fired = true;
                self.misses = 0;
                self.last_beat = t as f64;
                self.next_beat += PULL * err;
            }
        }
        if t as f64 > self.next_beat + tol {
            if !self.fired {
                self.misses += 1;
                self.last_beat = self.next_beat;
                beat = Some(Beat {
                    strength: 0.25,
                    ghost: true,
                });
                // Lost the beat (strong hits keep landing elsewhere): start again from them.
                if self.misses >= 4
                    && let Some((at, _)) = self.last_strong
                    && t - at < self.period as u64
                {
                    self.next_beat = at as f64;
                    self.misses = 0;
                }
            }
            self.next_beat += self.period as f64;
            while self.next_beat + tol < t as f64 {
                self.next_beat += self.period as f64;
            }
            self.fired = false;
        }
        beat
    }

    /// If a clearly better grid than the one being followed keeps showing up, switch to it.
    fn check_grid(&mut self) {
        let Some((ago, score)) = self.best_grid() else {
            return;
        };
        let t = self.hop as f64;
        let p = self.period as f64;
        // Where the followed grid puts a beat, as hops ago (0..p).
        let ours = (t - self.last_beat).rem_euclid(p);
        let diff = (ours - ago).abs().min(p - (ours - ago).abs());
        if diff <= (p * WINDOW as f64).max(2.5) {
            self.regrid = 0;
            return;
        }
        let x = self.raw_history();
        let n = x.len() as f64;
        let mut sum = 0.0;
        let mut k = 0;
        let mut i = n - 1.0 - ours;
        while i >= 0.0 {
            sum += x[i.round() as usize];
            k += 1;
            i -= p;
        }
        let current = sum / k.max(1) as f32;
        if score > current * 1.3 {
            self.regrid += 1;
            if self.regrid >= 2 {
                self.regrid = 0;
                self.anchor();
            }
        } else {
            self.regrid = 0;
        }
    }

    fn learn_level(&mut self, flux: f32) {
        self.beat_level = if self.beat_level <= 0.0 {
            flux
        } else {
            self.beat_level + (flux - self.beat_level) * 0.15
        };
    }

    /// The onset history in time order.
    fn raw_history(&self) -> Vec<f32> {
        let n = self.odf_filled;
        let len = self.odf.len();
        let start = (self.odf_pos + len - n) % len;
        (0..n).map(|i| self.odf[(start + i) % len]).collect()
    }

    /// The beat grid (at the current period) that lines up the most onset strength over the
    /// history: how many hops ago its latest beat was, and how much it lines up (per beat).
    fn best_grid(&self) -> Option<(f64, f32)> {
        let x = self.raw_history();
        let n = x.len();
        let p = self.period as f64;
        if p < 2.0 || (n as f64) < p * 2.0 {
            return None;
        }
        let at = |i: f64| -> f32 {
            let i = i.round() as isize;
            (i - 1..=i + 1)
                .filter(|j| *j >= 0 && (*j as usize) < n)
                .map(|j| x[j as usize])
                .fold(0.0, f32::max)
        };
        let mut best = (0.0, f32::MIN);
        for o in 0..p.ceil() as usize {
            let mut sum = 0.0;
            let mut k = 0;
            let mut i = (n - 1) as f64 - o as f64;
            while i >= 0.0 {
                sum += at(i);
                k += 1;
                i -= p;
            }
            let score = sum / k.max(1) as f32;
            if score > best.1 {
                best = (o as f64, score);
            }
        }
        Some(best)
    }

    /// The onset history in time order, mean removed.
    fn history(&self) -> Vec<f32> {
        let mut x = self.raw_history();
        let mean = x.iter().sum::<f32>() / x.len() as f32;
        x.iter_mut().for_each(|v| *v -= mean);
        x
    }

    fn estimate_tempo(&mut self) {
        let x = self.history();
        let n = x.len();
        let ac = |lag: usize| -> f32 {
            if lag >= n {
                return 0.0;
            }
            x[..n - lag]
                .iter()
                .zip(&x[lag..])
                .map(|(a, b)| a * b)
                .sum::<f32>()
                / (n - lag) as f32
        };
        let energy = ac(0);
        if energy <= 1e-9 {
            return;
        }
        let lo = (60.0 * self.rate / MAX_BPM).floor() as usize;
        let hi = (60.0 * self.rate / MIN_BPM).ceil() as usize;
        // Each lag's own correlation, backed by its double (beats come in bars), weighed by
        // how common the tempo is.
        let score = |lag: usize| -> f32 {
            let bpm = 60.0 * self.rate / lag as f32;
            let octaves = (bpm / 120.0).log2();
            let prior = (-0.5 * (octaves / 0.85).powi(2)).exp();
            (ac(lag) + 0.5 * ac(2 * lag).max(0.0)) * prior
        };
        let scores: Vec<f32> = (lo..=hi).map(score).collect();
        let Some((i, &best)) = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) else {
            return;
        };
        if best <= 0.0 {
            return;
        }
        // Between whole hops.
        let mut lag = (lo + i) as f32;
        if i > 0 && i + 1 < scores.len() {
            let (a, b, c) = (scores[i - 1], scores[i], scores[i + 1]);
            let d = a - 2.0 * b + c;
            if d.abs() > 1e-12 {
                lag += (0.5 * (a - c) / d).clamp(-0.5, 0.5);
            }
        }
        // How well the tempo we follow still fits, next to the best one.
        let current = (self.period > 0.0)
            .then(|| self.period.round() as usize)
            .filter(|l| (lo..=hi).contains(l))
            .map_or(0.0, |l| scores[l - lo]);
        let conf = (ac(lo + i) / energy).clamp(0.0, 1.0);
        self.confidence += (conf - self.confidence) * 0.3;
        if self.confidence >= LOCK {
            self.locked = true;
        } else if self.confidence < UNLOCK {
            self.locked = false;
        }

        if self.period <= 0.0 {
            self.period = lag;
            self.anchor();
            return;
        }
        self.check_grid();
        let ratio = lag / self.period;
        let near = |r: f32| (ratio / r - 1.0).abs() < 0.05;
        if near(1.0) {
            self.period += (lag - self.period) * 0.25;
            self.candidate = None;
        } else {
            // Half, double, or a triplet feel of the same groove: only a long-held change counts
            // (otherwise the tempo would flip back and forth inside one song).
            let related = [2.0, 0.5, 1.5, 2.0 / 3.0, 3.0, 1.0 / 3.0]
                .into_iter()
                .any(near);
            let needed = if related { 15 } else { 4 };
            // A related tempo must also fit clearly better than ours (a breakdown in half-time
            // or triplets is still the same song).
            let clearly = !related || best > current * 1.5;
            let count = match self.candidate {
                Some((c, k)) if clearly && (lag / c - 1.0).abs() < 0.05 => k + 1,
                _ if clearly => 1,
                _ => 0,
            };
            self.candidate = Some((lag, count));
            if count >= needed {
                self.period = lag;
                self.candidate = None;
                self.anchor();
            }
        }
    }

    /// Lines the predicted beats up with the grid that fits the recent onsets best.
    fn anchor(&mut self) {
        let t = self.hop as f64;
        let from = match self.best_grid() {
            // The newest history entry is this hop.
            Some((ago, _)) => t - ago,
            None => self
                .last_strong
                .map_or(self.last_onset as f64, |(at, _)| at as f64),
        };
        let p = self.period as f64;
        let k = ((t - from) / p).ceil().max(1.0);
        self.next_beat = from + k * p;
        self.last_beat = self.next_beat - p;
        self.fired = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0 / 512.0;

    fn centers() -> [f32; BANDS] {
        std::array::from_fn(|b| 30.0 * (16_000.0f32 / 30.0).powf((b as f32 + 0.5) / BANDS as f32))
    }

    /// Band levels for a song at `bpm`: a kick on every beat, plus whatever `extra` adds
    /// (hops since start -> extra levels).
    fn song(bpm: f32, secs: f32, extra: impl Fn(usize, &mut [f32; BANDS])) -> Vec<[f32; BANDS]> {
        let period = 60.0 * RATE / bpm;
        (0..(secs * RATE) as usize)
            .map(|h| {
                let mut l = [0.1f32; BANDS];
                let since = h as f32 % period;
                let kick = (-(since) / 4.0).exp();
                for v in &mut l[..8] {
                    *v += 0.7 * kick;
                }
                extra(h, &mut l);
                l
            })
            .collect()
    }

    fn run(levels: &[[f32; BANDS]]) -> (BeatTracker, Vec<(usize, Beat)>) {
        let mut t = BeatTracker::new(RATE, &centers());
        let beats = levels
            .iter()
            .enumerate()
            .filter_map(|(h, l)| t.push(l).map(|b| (h, b)))
            .collect();
        (t, beats)
    }

    #[test]
    fn finds_common_tempos() {
        for bpm in [90.0, 120.0, 128.0, 140.0, 174.0] {
            let (t, _) = run(&song(bpm, 12.0, |_, _| {}));
            let got = t.bpm().expect("a tempo");
            // A double/half of the real tempo is the same groove; anything else is wrong.
            let ok = [1.0, 2.0, 0.5]
                .iter()
                .any(|m| (got / (bpm * m) - 1.0).abs() < 0.03);
            assert!(ok, "{bpm} BPM came out as {got}");
        }
    }

    #[test]
    fn off_beat_sounds_are_not_beats_but_accents_hit_harder() {
        let bpm = 120.0;
        let period = 60.0 * RATE / bpm;
        // Hi-hats and vocal bursts halfway between beats, and every 4th beat a loud "yeah"
        // across the mids on top of the kick.
        let levels = song(bpm, 16.0, |h, l| {
            let since = h as f32 % period;
            let off = ((since - period / 2.0).abs() < 1.0) as u8 as f32;
            for v in &mut l[40..60] {
                *v += 0.3 * off;
            }
            let beat_no = (h as f32 / period) as usize;
            if beat_no.is_multiple_of(4) && since < 3.0 {
                for v in &mut l[20..44] {
                    *v += 0.6;
                }
            }
        });
        let (t, beats) = run(&levels);
        assert!(t.bpm().is_some());
        // After the tempo locks (say 5 s in), every beat sits on a kick.
        let late: Vec<_> = beats
            .iter()
            .filter(|(h, b)| *h as f32 > 5.0 * RATE && !b.ghost)
            .collect();
        assert!(late.len() >= 18, "{} beats", late.len());
        for (h, _) in &late {
            let since = *h as f32 % period;
            let off = since.min(period - since);
            assert!(off <= 3.0, "beat {off} hops from a kick");
        }
        let accent: Vec<f32> = late
            .iter()
            .filter(|(h, _)| ((*h as f32 / period).round() as usize).is_multiple_of(4))
            .map(|(_, b)| b.strength)
            .collect();
        let plain: Vec<f32> = late
            .iter()
            .filter(|(h, _)| !((*h as f32 / period).round() as usize).is_multiple_of(4))
            .map(|(_, b)| b.strength)
            .collect();
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        assert!(
            mean(&accent) > mean(&plain) * 1.15,
            "accents {} vs {}",
            mean(&accent),
            mean(&plain)
        );
    }

    #[test]
    fn a_break_keeps_a_soft_pulse_and_the_beat_comes_back_on_time() {
        let bpm = 128.0;
        let period = 60.0 * RATE / bpm;
        // Kicks drop out from 8 s to 11 s.
        let levels = song(bpm, 16.0, |h, l| {
            let s = h as f32 / RATE;
            if (8.0..11.0).contains(&s) {
                l.iter_mut().for_each(|v| *v = 0.1);
            }
        });
        let (_, beats) = run(&levels);
        let ghosts = beats
            .iter()
            .filter(|(h, b)| b.ghost && (8.3..10.8).contains(&(*h as f32 / RATE)))
            .count();
        assert!(ghosts >= 4, "{ghosts} ghost beats in the break");
        let after: Vec<_> = beats
            .iter()
            .filter(|(h, b)| !b.ghost && *h as f32 / RATE > 11.5)
            .collect();
        assert!(after.len() >= 8);
        for (h, _) in after {
            let since = *h as f32 % period;
            assert!(since.min(period - since) <= 3.0);
        }
    }
}
