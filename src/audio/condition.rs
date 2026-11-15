//! Converts whatever a source produces into the engine's internal format:
//! interleaved stereo `f32` at the output device's sample rate.
//!
//! Resampling uses rubato's synchronous FFT resampler. Its group delay is trimmed from the
//! start and the tail is flushed to exactly `round(frames_in * ratio)` frames, so track lengths
//! are preserved and gapless playback stays gapless after resampling.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

pub const CHANNELS: usize = 2;
const CHUNK_FRAMES: usize = 1024;

pub struct Conditioner {
    in_rate: u32,
    out_rate: u32,
    resampler: Option<Fft<f32>>,
    /// Stereo frames waiting for a full resampler input chunk.
    pending_in: Vec<f32>,
    scratch_out: Vec<f32>,
    delay_left: usize,
    frames_in: u64,
    frames_out: u64,
}

impl Conditioner {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let resampler = (in_rate != out_rate).then(|| {
            Fft::<f32>::new(
                in_rate as usize,
                out_rate as usize,
                CHUNK_FRAMES,
                CHANNELS,
                FixedSync::Input,
            )
            .expect("valid resampler parameters")
        });
        let (delay, max_out) = resampler
            .as_ref()
            .map_or((0, 0), |r| (r.output_delay(), r.output_frames_max()));
        Self {
            in_rate,
            out_rate,
            resampler,
            pending_in: Vec::with_capacity(CHUNK_FRAMES * CHANNELS * 4),
            scratch_out: vec![0.0; max_out * CHANNELS],
            delay_left: delay,
            frames_in: 0,
            frames_out: 0,
        }
    }

    pub fn input_rate(&self) -> u32 {
        self.in_rate
    }

    /// Drops all buffered state (after a seek).
    pub fn reset(&mut self) {
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
            self.delay_left = r.output_delay();
        }
        self.pending_in.clear();
        self.frames_in = 0;
        self.frames_out = 0;
    }

    /// Feeds interleaved samples with `channels` channels; appends converted stereo to `out`.
    pub fn process(&mut self, input: &[f32], channels: usize, out: &mut Vec<f32>) {
        if self.resampler.is_none() {
            downmix_into(input, channels, out);
            return;
        }
        let before = self.pending_in.len();
        downmix_into(input, channels, &mut self.pending_in);
        self.frames_in += ((self.pending_in.len() - before) / CHANNELS) as u64;
        self.drain_full_chunks(out);
    }

    /// Flushes everything buffered at end of stream.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let Some(resampler) = self.resampler.as_mut() else {
            return;
        };
        let expected =
            (self.frames_in as f64 * self.out_rate as f64 / self.in_rate as f64).round() as u64;
        let mut tail: Vec<f32> = std::mem::take(&mut self.pending_in);
        // Process the partial chunk, then feed silence until the delay line is drained.
        loop {
            if self.frames_out >= expected {
                break;
            }
            let have = tail.len() / CHANNELS;
            let need = resampler.input_frames_next();
            let take = have.min(need);
            tail.resize(need * CHANNELS, 0.0);
            let input = InterleavedSlice::new(&tail[..], CHANNELS, need).expect("sized");
            let out_frames = resampler.output_frames_next();
            let mut output =
                InterleavedSlice::new_mut(&mut self.scratch_out[..], CHANNELS, out_frames)
                    .expect("sized");
            let indexing = Indexing::new().partial_len(take);
            let (_, written) = resampler
                .process_into_buffer(&input, &mut output, Some(&indexing))
                .expect("resampler process");
            tail.clear();
            let remaining = (expected - self.frames_out) as usize;
            emit(
                &self.scratch_out[..written * CHANNELS],
                &mut self.delay_left,
                &mut self.frames_out,
                Some(remaining),
                out,
            );
            if written == 0 {
                break;
            }
        }
    }

    fn drain_full_chunks(&mut self, out: &mut Vec<f32>) {
        let resampler = self.resampler.as_mut().expect("resampling path");
        let mut consumed = 0;
        loop {
            let need = resampler.input_frames_next();
            let avail = (self.pending_in.len() - consumed) / CHANNELS;
            if avail < need {
                break;
            }
            let input =
                InterleavedSlice::new(&self.pending_in[consumed..], CHANNELS, need).expect("sized");
            let out_frames = resampler.output_frames_next();
            let mut output =
                InterleavedSlice::new_mut(&mut self.scratch_out[..], CHANNELS, out_frames)
                    .expect("sized");
            let (read, written) = resampler
                .process_into_buffer(&input, &mut output, None)
                .expect("resampler process");
            consumed += read * CHANNELS;
            emit(
                &self.scratch_out[..written * CHANNELS],
                &mut self.delay_left,
                &mut self.frames_out,
                None,
                out,
            );
        }
        self.pending_in.drain(..consumed);
    }
}

fn emit(
    samples: &[f32],
    delay_left: &mut usize,
    frames_out: &mut u64,
    limit: Option<usize>,
    out: &mut Vec<f32>,
) {
    let frames = samples.len() / CHANNELS;
    let skip = (*delay_left).min(frames);
    *delay_left -= skip;
    let mut n = frames - skip;
    if let Some(limit) = limit {
        n = n.min(limit);
    }
    out.extend_from_slice(&samples[skip * CHANNELS..(skip + n) * CHANNELS]);
    *frames_out += n as u64;
}

/// Converts interleaved audio with any channel count to interleaved stereo.
///
/// Layouts with more than two channels are assumed to follow the usual
/// `FL FR FC LFE SL SR ...` order; center and surrounds are folded in at -3 dB.
pub fn downmix_into(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    const M3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match channels {
        0 => {}
        1 => out.extend(input.iter().flat_map(|&s| [s, s])),
        2 => out.extend_from_slice(input),
        n => {
            let has_surround = n >= 6;
            let norm = 1.0 / (1.0 + M3DB + if has_surround { M3DB } else { 0.0 });
            out.reserve(input.len() / n * 2);
            for f in input.chunks_exact(n) {
                let c = f[2] * M3DB;
                let (sl, sr) = if has_surround {
                    (f[4] * M3DB, f[5] * M3DB)
                } else {
                    (0.0, 0.0)
                };
                out.push((f[0] + c + sl) * norm);
                out.push((f[1] + c + sr) * norm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(in_rate: u32, out_rate: u32, frames: usize, packet: usize) -> Vec<f32> {
        let mut c = Conditioner::new(in_rate, out_rate);
        let input: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / in_rate as f32).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut out = Vec::new();
        for chunk in input.chunks(packet * 2) {
            c.process(chunk, 2, &mut out);
        }
        c.finish(&mut out);
        out
    }

    #[test]
    fn passthrough_is_identity() {
        let out = run(48_000, 48_000, 10_000, 1152);
        assert_eq!(out.len(), 20_000);
    }

    #[test]
    fn resampling_preserves_duration_exactly() {
        for (i, o) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (96_000, 48_000),
            (22_050, 48_000),
        ] {
            let frames = 123_457;
            let out = run(i, o, frames, 1152);
            let expected = (frames as f64 * o as f64 / i as f64).round() as usize;
            assert_eq!(out.len() / 2, expected, "{i} -> {o}");
        }
    }

    #[test]
    fn resampling_keeps_signal_aligned_and_level() {
        // A 440 Hz sine at 0.5 amplitude should stay ~0.5 after the delay is trimmed.
        let out = run(44_100, 48_000, 44_100, 1000);
        let mid = &out[20_000..60_000];
        let peak = mid.iter().fold(0f32, |m, &s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "peak {peak}");
        // First output sample should be near the sine's start (zero), not a delayed blank.
        let first_nonzero = out.iter().position(|s| s.abs() > 1e-3).unwrap();
        assert!(
            first_nonzero < 8,
            "signal delayed by {first_nonzero} samples"
        );
    }

    #[test]
    fn downmix_51_to_stereo() {
        let mut out = Vec::new();
        downmix_into(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], 6, &mut out);
        assert!(out[0] > 0.0 && out[1] == 0.0);
        let mut out = Vec::new();
        downmix_into(&[0.25], 1, &mut out);
        assert_eq!(out, vec![0.25, 0.25]);
    }
}
