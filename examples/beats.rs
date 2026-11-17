//! Runs the visualizer analysis over audio files (offline, silent) and reports the beat
//! tracking: the tempo it settles on, how steady the beats are, how many fall in quiet parts.
//!
//! cargo run --release --example beats -- <files...>

use khoostic::audio::analysis::Analyzer;
use khoostic::audio::{AudioSource, ReadStatus, SymphoniaSource};

const HOP: usize = 512;

fn main() -> anyhow::Result<()> {
    for path in std::env::args().skip(1) {
        let mut src = SymphoniaSource::open_path(std::path::Path::new(&path))?;
        let spec = src.spec();
        let mut analyzer = Analyzer::new(spec.sample_rate);
        let hop_secs = HOP as f64 / spec.sample_rate as f64;
        let mut buf = Vec::new();
        let mut mono = Vec::new();
        let mut hops = 0u64;
        let mut last_count = 0;
        // (time, strength) of every beat.
        let mut beats: Vec<(f64, f32)> = Vec::new();
        let mut tempo_at = Vec::new();
        // Averages of the rhythm readings the 3D visualizer moves to.
        let (mut busy, mut energy, mut bass, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let mut last_onsets = 0;
        let mut hits = 0u64;
        loop {
            buf.clear();
            if src.read(&mut buf)? == ReadStatus::End {
                break;
            }
            let ch = spec.channels.max(1);
            mono.extend(buf.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32));
            while mono.len() >= HOP {
                let hop: Vec<f32> = mono.drain(..HOP).collect();
                let f = analyzer.process_hop(&hop);
                hops += 1;
                let t = hops as f64 * hop_secs;
                if f.rms > 0.01 {
                    busy += f.busy as f64;
                    energy += f.energy as f64;
                    bass += f.bass as f64;
                    n += 1.0;
                }
                if f.onset_count != last_onsets {
                    last_onsets = f.onset_count;
                    hits += 1;
                }
                if f.beat_count != last_count {
                    last_count = f.beat_count;
                    beats.push((t, f.beat_strength));
                }
                if hops.is_multiple_of((10.0 / hop_secs) as u64) {
                    tempo_at.push(format!("{:.0}s:{:.1}", t, f.bpm));
                }
            }
        }
        let secs = hops as f64 * hop_secs;
        let name = std::path::Path::new(&path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Steadiness of the real (not ghost) beats after the first 10 s.
        let real: Vec<f64> = beats
            .iter()
            .filter(|(t, s)| *t > 10.0 && *s > 0.3)
            .map(|(t, _)| *t)
            .collect();
        let ioi: Vec<f64> = real.windows(2).map(|w| w[1] - w[0]).collect();
        let median = {
            let mut v = ioi.clone();
            v.sort_by(f64::total_cmp);
            v.get(v.len() / 2).copied().unwrap_or(0.0)
        };
        // Share of intervals within 6% of one beat (or exactly two: a beat with no hit).
        let steady = ioi
            .iter()
            .filter(|d| {
                let r = **d / median;
                (r - 1.0).abs() < 0.06 || (r - 2.0).abs() < 0.12
            })
            .count() as f64
            / ioi.len().max(1) as f64;
        let ghosts = beats.iter().filter(|(_, s)| *s <= 0.3).count();
        let accents = beats.iter().filter(|(_, s)| *s > 1.0).count();
        println!(
            "{name}: {secs:.0} s, {} beats ({ghosts} soft, {accents} accented), interval {:.3} s = {:.1} BPM, {:.0}% steady\n  rhythm: {:.1} hits/s, busy {:.2}, energy {:.2}, bass {:.2}\n  tempo over time: {}",
            beats.len(),
            median,
            60.0 / median.max(1e-9),
            steady * 100.0,
            hits as f64 / secs,
            busy / n.max(1.0),
            energy / n.max(1.0),
            bass / n.max(1.0),
            tempo_at.join(" "),
        );
    }
    Ok(())
}
