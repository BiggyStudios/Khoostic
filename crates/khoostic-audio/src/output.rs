//! Device output.
//!
//! The cpal callback is the only real-time code in Khoostic. It never locks or allocates in
//! the steady state: it pops stereo frames from an SPSC ring, applies a click-free volume ramp,
//! expands to the device's channel layout and taps the same frames into the analysis ring.
//!
//! The stream runs continuously (silence while paused) so control handshakes such as the
//! flush used for seeking always complete promptly.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use crossbeam_channel::Sender;

use crate::condition::CHANNELS;
use crate::engine::EngineEvent;

/// State shared between the engine thread and the audio callback.
#[derive(Debug)]
pub struct OutputShared {
    pub paused: AtomicBool,
    /// Target linear gain, stored as `f32` bits.
    gain: AtomicU32,
    /// Frames taken out of the ring: played *or* discarded by a flush. Comparable with the
    /// engine's count of frames pushed, which is what makes position tracking exact.
    pub consumed: AtomicU64,
    flush_requested: AtomicU64,
    flush_done: AtomicU64,
}

impl OutputShared {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
            consumed: AtomicU64::new(0),
            flush_requested: AtomicU64::new(0),
            flush_done: AtomicU64::new(0),
        }
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
}

pub struct Output {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub device_name: String,
    pub shared: Arc<OutputShared>,
}

impl Output {
    /// Asks the callback to discard everything currently queued and waits until it has.
    /// The producer must not push while this runs.
    pub fn flush(&self, timeout: Duration) -> bool {
        let req = self.shared.flush_requested.fetch_add(1, Ordering::AcqRel) + 1;
        let deadline = Instant::now() + timeout;
        while self.shared.flush_done.load(Ordering::Acquire) < req {
            if Instant::now() >= deadline {
                tracing::warn!("audio flush timed out; is the output stream stalled?");
                return false;
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        true
    }
}

pub struct OpenedOutput {
    pub output: Output,
    pub producer: rtrb::Producer<f32>,
    pub analysis: rtrb::Consumer<f32>,
}

/// Names of the available output devices, default device first.
pub fn device_names() -> anyhow::Result<Vec<String>> {
    let host = cpal::default_host();
    let name = |d: &cpal::Device| d.description().map(|d| d.name().to_owned()).ok();
    let default = host.default_output_device().as_ref().and_then(name);
    let mut names: Vec<String> = host.output_devices()?.filter_map(|d| name(&d)).collect();
    if let Some(def) = default {
        names.retain(|n| *n != def);
        names.insert(0, format!("{def} (default)"));
    }
    Ok(names)
}

/// Opens an output device (by name substring, or the default) and starts its stream.
pub fn open(
    device_hint: Option<&str>,
    buffer: Duration,
    events: Sender<EngineEvent>,
) -> anyhow::Result<OpenedOutput> {
    let host = cpal::default_host();
    let device = match device_hint {
        Some(hint) => {
            let hint = hint.to_lowercase();
            host.output_devices()?
                .find(|d| {
                    d.description()
                        .is_ok_and(|desc| desc.name().to_lowercase().contains(&hint))
                })
                .ok_or_else(|| anyhow::anyhow!("no output device matching {hint:?}"))?
        }
        None => host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no default output device"))?,
    };
    let device_name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "unknown device".into());
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let sample_rate = config.sample_rate;

    let ring_len = ((sample_rate as f64 * buffer.as_secs_f64()) as usize).max(4096) * CHANNELS;
    let (producer, consumer) = rtrb::RingBuffer::new(ring_len);
    let (analysis_tx, analysis) = rtrb::RingBuffer::new(sample_rate as usize / 2 * CHANNELS);
    let shared = Arc::new(OutputShared::new());

    let cb = Callback {
        ring: consumer,
        analysis: analysis_tx,
        shared: shared.clone(),
        device_channels: config.channels as usize,
        scratch: vec![0.0; 8192 * CHANNELS],
        current_gain: 1.0,
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, cb, events)?,
        SampleFormat::I16 => build::<i16>(&device, config, cb, events)?,
        SampleFormat::I32 => build::<i32>(&device, config, cb, events)?,
        SampleFormat::U16 => build::<u16>(&device, config, cb, events)?,
        SampleFormat::F64 => build::<f64>(&device, config, cb, events)?,
        SampleFormat::U8 => build::<u8>(&device, config, cb, events)?,
        SampleFormat::I8 => build::<i8>(&device, config, cb, events)?,
        other => anyhow::bail!("unsupported device sample format {other}"),
    };
    stream.play()?;
    tracing::info!(%device_name, sample_rate, ?format, "audio output opened");

    Ok(OpenedOutput {
        output: Output {
            _stream: stream,
            sample_rate,
            device_name,
            shared,
        },
        producer,
        analysis,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut cb: Callback,
    events: Sender<EngineEvent>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| cb.fill(data),
        move |err| {
            let _ = events.try_send(EngineEvent::OutputError(err.to_string()));
        },
        None,
    )?;
    Ok(stream)
}

struct Callback {
    ring: rtrb::Consumer<f32>,
    analysis: rtrb::Producer<f32>,
    shared: Arc<OutputShared>,
    device_channels: usize,
    scratch: Vec<f32>,
    current_gain: f32,
}

impl Callback {
    fn fill<T: SizedSample + FromSample<f32>>(&mut self, data: &mut [T]) {
        let shared = &*self.shared;

        let requested = shared.flush_requested.load(Ordering::Acquire);
        if shared.flush_done.load(Ordering::Relaxed) != requested {
            let n = self.ring.slots();
            if let Ok(chunk) = self.ring.read_chunk(n) {
                chunk.commit_all();
            }
            shared
                .consumed
                .fetch_add((n / CHANNELS) as u64, Ordering::AcqRel);
            shared.flush_done.store(requested, Ordering::Release);
        }

        let frames = data.len() / self.device_channels;
        let silence = T::from_sample(0.0f32);
        if shared.paused.load(Ordering::Relaxed) {
            data.fill(silence);
            return;
        }

        if self.scratch.len() < frames * CHANNELS {
            // Only happens if the device suddenly asks for a huge buffer.
            self.scratch.resize(frames * CHANNELS, 0.0);
        }
        let want = (frames * CHANNELS).min(self.ring.slots()) & !1;
        let (got, _) = self.ring.pop_partial_slice(&mut self.scratch[..want]);
        let got_frames = got.len() / CHANNELS;

        // Visualizers see the signal pre-volume so they don't die when you turn it down.
        let _ = self.analysis.push_partial_slice(got);

        let target = shared.gain();
        let start = self.current_gain;
        let step = if got_frames > 0 {
            (target - start) / got_frames as f32
        } else {
            0.0
        };
        let dc = self.device_channels;
        for (i, (out, frame)) in data
            .chunks_exact_mut(dc)
            .zip(got.as_chunks::<2>().0)
            .enumerate()
        {
            let g = start + step * (i + 1) as f32;
            let (l, r) = (frame[0] * g, frame[1] * g);
            if dc == 1 {
                out[0] = T::from_sample((l + r) * 0.5);
            } else {
                out[0] = T::from_sample(l);
                out[1] = T::from_sample(r);
                out[2..].fill(silence);
            }
        }
        if got_frames > 0 {
            self.current_gain = target;
        }
        data[got_frames * dc..].fill(silence);
        shared
            .consumed
            .fetch_add(got_frames as u64, Ordering::AcqRel);
    }
}
