//! Real-time audio analysis that drives the visualizers.
//!
//! The output callback taps the exact samples sent to the speakers into a lock-free ring.
//! A dedicated thread runs an FFT over a sliding window and publishes an [`AnalysisFrame`]
//! through an `ArcSwap`, so any number of readers (TUI meters, GPU particle systems, ...)
//! can grab the latest frame without ever blocking audio.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::beat::BeatTracker;

pub const BANDS: usize = 64;
pub const WAVEFORM_LEN: usize = 512;
const FFT_SIZE: usize = 2048;
const HOP: usize = 512;
const MIN_FREQ: f32 = 30.0;
const MAX_FREQ: f32 = 16_000.0;
const FLOOR_DB: f32 = -60.0;
/// Level that maps to a full bar. Below 0 dBFS because real music rarely peaks a single band.
const CEIL_DB: f32 = -8.0;
/// Pink-noise style tilt so highs aren't permanently dwarfed by bass.
const TILT_DB_PER_OCTAVE: f32 = 3.0;

#[derive(Clone, Debug)]
pub struct AnalysisFrame {
    /// Increments with every published frame.
    pub seq: u64,
    /// Log-spaced spectrum, smoothed, normalized to `0..=1`.
    pub bands: [f32; BANDS],
    /// Slowly falling peak markers for `bands`.
    pub peaks: [f32; BANDS],
    /// Most recent mono samples, oldest first.
    pub waveform: [f32; WAVEFORM_LEN],
    /// Loudness of the last hop (linear RMS).
    pub rms: f32,
    /// Coarse energy groups, `0..=1`.
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    /// The latest beat's strength on a beat, decaying towards `0` afterwards.
    pub beat: f32,
    /// Number of beats since the analyzer started; changes exactly once per beat (tracked
    /// beats: on the tempo, not on every sudden sound; see [`super::beat`]).
    pub beat_count: u64,
    /// How hard the latest beat hit: ~0.7 usually, up to ~1.25 for accents (several things
    /// landing on it), ~0.25 for a "ghost" beat kept going through a quiet part.
    pub beat_strength: f32,
    /// The song's tempo, 0 while unknown.
    pub bpm: f32,
    /// How far into the current beat (0 at a beat, 1 at the next); 0 without a tempo.
    pub beat_phase: f32,
    /// Every percussive hit, not only beats (hats, snares, a drum loop's every hit): a count
    /// that changes once per hit, and the latest hit's strength next to the song's usual
    /// (0.2..1).
    pub onset_count: u64,
    pub onset_strength: f32,
    /// How busy the rhythm is (0..1; hits per second, 8 and up is full).
    pub busy: f32,
    /// Loudness next to the song's own loud parts (0..1), whatever the mix: a quiet song
    /// playing out is as "full" as a loud one.
    pub energy: f32,
}

impl Default for AnalysisFrame {
    fn default() -> Self {
        Self {
            seq: 0,
            bands: [0.0; BANDS],
            peaks: [0.0; BANDS],
            waveform: [0.0; WAVEFORM_LEN],
            rms: 0.0,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
            beat: 0.0,
            beat_count: 0,
            beat_strength: 0.0,
            bpm: 0.0,
            beat_phase: 0.0,
            onset_count: 0,
            onset_strength: 0.0,
            busy: 0.0,
            energy: 0.0,
        }
    }
}

pub struct Analyzer {
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    window: Vec<f32>,
    window_norm: f32,
    /// Circular history of mono samples, length `FFT_SIZE`.
    history: Vec<f32>,
    history_pos: usize,
    fft_in: Vec<f32>,
    fft_out: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    band_bins: [(usize, usize); BANDS],
    band_tilt: [f32; BANDS],
    frame: AnalysisFrame,
    beats: BeatTracker,
    /// This hop's band levels before smoothing (what the beat tracker reads).
    raw: [f32; BANDS],
    /// The song's recent loud level (a slowly falling peak of the RMS), and hops per second.
    loud: f32,
    hop_rate: f32,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        let window_norm = 2.0 / window.iter().sum::<f32>();
        let bin_hz = sample_rate as f32 / FFT_SIZE as f32;
        let max_freq = MAX_FREQ.min(sample_rate as f32 / 2.0);
        let nbins = FFT_SIZE / 2 + 1;

        let mut band_bins = [(0, 0); BANDS];
        let mut band_tilt = [0.0; BANDS];
        let mut centers = [0.0; BANDS];
        for (b, (bins, tilt)) in band_bins.iter_mut().zip(band_tilt.iter_mut()).enumerate() {
            let f0 = MIN_FREQ * (max_freq / MIN_FREQ).powf(b as f32 / BANDS as f32);
            let f1 = MIN_FREQ * (max_freq / MIN_FREQ).powf((b + 1) as f32 / BANDS as f32);
            let lo = ((f0 / bin_hz).floor() as usize).clamp(1, nbins - 1);
            let hi = ((f1 / bin_hz).ceil() as usize).clamp(lo + 1, nbins);
            *bins = (lo, hi);
            let center = (f0 * f1).sqrt();
            centers[b] = center;
            *tilt = TILT_DB_PER_OCTAVE * (center / 1000.0).log2();
        }

        Self {
            fft_scratch: fft.make_scratch_vec(),
            fft_in: fft.make_input_vec(),
            fft_out: fft.make_output_vec(),
            fft,
            window,
            window_norm,
            history: vec![0.0; FFT_SIZE],
            history_pos: 0,
            band_bins,
            band_tilt,
            frame: AnalysisFrame::default(),
            beats: BeatTracker::new(sample_rate as f32 / HOP as f32, &centers),
            raw: [0.0; BANDS],
            loud: 0.0,
            hop_rate: sample_rate as f32 / HOP as f32,
        }
    }

    pub fn frame(&self) -> &AnalysisFrame {
        &self.frame
    }

    /// Pushes one hop of mono samples and recomputes the frame.
    pub fn process_hop(&mut self, mono: &[f32]) -> &AnalysisFrame {
        debug_assert_eq!(mono.len(), HOP);
        let mut sq = 0.0;
        for &s in mono {
            self.history[self.history_pos] = s;
            self.history_pos = (self.history_pos + 1) % FFT_SIZE;
            sq += s * s;
        }
        let f = &mut self.frame;
        f.rms = (sq / mono.len() as f32).sqrt();

        // Waveform = tail of history.
        for (i, w) in f.waveform.iter_mut().enumerate() {
            let idx = (self.history_pos + FFT_SIZE - WAVEFORM_LEN + i) % FFT_SIZE;
            *w = self.history[idx];
        }

        // Windowed FFT over the history, oldest sample first.
        for i in 0..FFT_SIZE {
            self.fft_in[i] = self.history[(self.history_pos + i) % FFT_SIZE] * self.window[i];
        }
        self.fft
            .process_with_scratch(&mut self.fft_in, &mut self.fft_out, &mut self.fft_scratch)
            .expect("fft sizes are fixed");

        let norm = self.window_norm;
        let mag = |c: &Complex<f32>| c.norm() * norm;

        for b in 0..BANDS {
            let (lo, hi) = self.band_bins[b];
            let peak = self.fft_out[lo..hi].iter().map(mag).fold(0.0, f32::max);
            let db = 20.0 * (peak + 1e-9).log10() + self.band_tilt[b];
            let target = ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0);
            self.raw[b] = target;
            let cur = f.bands[b];
            // Fast attack, slow release: punchy but not flickery.
            let k = if target > cur { 0.65 } else { 0.18 };
            let v = cur + (target - cur) * k;
            // Snap to zero so idle decay terminates instead of crawling through denormals.
            f.bands[b] = if v < 1e-3 { 0.0 } else { v };
            f.peaks[b] = (f.peaks[b] - 0.008).max(f.bands[b]);
        }

        let third = BANDS / 3;
        let avg = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
        f.bass = avg(&f.bands[..third / 2 + 4]);
        f.mid = avg(&f.bands[third..2 * third]);
        f.treble = avg(&f.bands[2 * third..]);

        match self.beats.push(&self.raw) {
            Some(beat) => {
                f.beat = beat.strength;
                f.beat_strength = beat.strength;
                f.beat_count += 1;
            }
            None => f.beat = if f.beat < 1e-3 { 0.0 } else { f.beat * 0.88 },
        }
        f.bpm = self.beats.bpm().unwrap_or(0.0);
        f.beat_phase = self.beats.phase();
        f.onset_count = self.beats.onsets;
        f.onset_strength = self.beats.onset_strength;
        f.busy = self.beats.busy();
        // Loudness against a peak that falls over ~8 s (with a floor, so silence isn't "full").
        self.loud = f
            .rms
            .max(self.loud * (-1.0 / (8.0 * self.hop_rate)).exp())
            .max(0.01);
        let energy = (f.rms / self.loud).clamp(0.0, 1.0);
        f.energy += (energy - f.energy) * (1.0 - (-1.0 / (0.12 * self.hop_rate)).exp());

        f.seq += 1;
        &self.frame
    }
}

/// Spawns the analysis thread reading stereo samples from `input`.
pub(crate) fn spawn(
    mut input: rtrb::Consumer<f32>,
    sample_rate: u32,
    out: Arc<ArcSwap<AnalysisFrame>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("khoostic-analysis".into())
        .spawn(move || {
            let mut analyzer = Analyzer::new(sample_rate);
            let mut hop = Vec::with_capacity(HOP);
            let mut stereo = vec![0.0f32; HOP * 2];
            let hop_time = Duration::from_secs_f64(HOP as f64 / sample_rate as f64);
            let mut last_data = Instant::now();
            let mut last_idle_hop = Instant::now();

            while !stop.load(Ordering::Relaxed) {
                let want = ((HOP - hop.len()) * 2).min(input.slots()) & !1;
                if want > 0 {
                    let (got, _) = input.pop_partial_slice(&mut stereo[..want]);
                    hop.extend(got.as_chunks::<2>().0.iter().map(|f| (f[0] + f[1]) * 0.5));
                    last_data = Instant::now();
                } else if last_data.elapsed() > hop_time * 2 {
                    // Nothing is playing: feed silence at the hop rate so visuals decay smoothly.
                    let f = analyzer.frame();
                    let lit = f.rms > 0.0 || f.beat > 0.001 || f.peaks.iter().any(|&b| b > 0.0);
                    if lit && last_idle_hop.elapsed() >= hop_time {
                        hop.resize(HOP, 0.0);
                        last_idle_hop = Instant::now();
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                } else {
                    thread::sleep(Duration::from_millis(2));
                }

                if hop.len() == HOP {
                    let frame = analyzer.process_hop(&hop);
                    out.store(Arc::new(frame.clone()));
                    hop.clear();
                }
            }
        })
        .expect("spawn analysis thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * amp)
            .collect()
    }

    #[test]
    fn sine_lights_up_the_matching_band() {
        let mut a = Analyzer::new(48_000);
        let s = sine(1000.0, 48_000, HOP * 32, 0.5);
        for hop in s.as_chunks::<HOP>().0 {
            a.process_hop(hop);
        }
        let f = a.frame();
        let loudest = (0..BANDS)
            .max_by(|&x, &y| f.bands[x].total_cmp(&f.bands[y]))
            .unwrap();
        let (lo, hi) = a.band_bins[loudest];
        let (lo_hz, hi_hz) = (lo as f32 * 48_000.0 / 2048.0, hi as f32 * 48_000.0 / 2048.0);
        // Hann leakage spreads a tone over ~2 bins, so allow the neighbouring band to win.
        assert!(
            lo_hz * 0.85 <= 1000.0 && 1000.0 <= hi_hz * 1.15,
            "loudest band {lo_hz}..{hi_hz}"
        );
        // Bands far away from the tone must stay dark.
        assert!(f.bands[..BANDS / 4].iter().all(|&b| b < 0.3));
        assert!(f.bands[loudest] > 0.8);
        assert!((f.rms - 0.5 / 2f32.sqrt()).abs() < 0.01);
    }

    #[test]
    fn silence_is_dark() {
        let mut a = Analyzer::new(44_100);
        for _ in 0..20 {
            a.process_hop(&[0.0; HOP]);
        }
        assert!(a.frame().bands.iter().all(|&b| b == 0.0));
        assert_eq!(a.frame().beat_count, 0);
    }

    #[test]
    fn kick_drum_pattern_triggers_beats() {
        let rate = 48_000;
        let mut a = Analyzer::new(rate);
        // 120 BPM: a 60 Hz burst every 0.5 s on top of quiet noise-free silence.
        let total = rate as usize * 6;
        let mut signal = vec![0.0f32; total];
        for beat in (0..total).step_by(rate as usize / 2) {
            for (i, s) in signal[beat..(beat + 4800).min(total)]
                .iter_mut()
                .enumerate()
            {
                let env = (-(i as f32) / 1200.0).exp();
                *s = (i as f32 * 60.0 * std::f32::consts::TAU / rate as f32).sin() * env * 0.8;
            }
        }
        for hop in signal.as_chunks::<HOP>().0 {
            a.process_hop(hop);
        }
        let beats = a.frame().beat_count;
        assert!(
            (9..=13).contains(&beats),
            "detected {beats} beats, expected ~12"
        );
    }
}
