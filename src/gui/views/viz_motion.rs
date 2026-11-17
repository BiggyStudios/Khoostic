//! How the 3D visualizer moves to the music: every musical signal mapped to motion in one
//! place, frame-rate independent and testable without a GPU.
//!
//! The music is read for what *kind* of thing is happening, against the song's own levels:
//! * **beats** (the tracker's, on the tempo) are the big hits: a burst, a lunge, a camera punch;
//! * **percussive hits that aren't beats** (hats, snares, a drum loop's stabs) whip the cubes
//!   around the wave and kick its spin, so a busy loop with little bass spins and swirls;
//! * **how busy the rhythm is** keeps the swarm turning and swirling between hits;
//! * **bass** puffs the ribbon out when it rises; when a song has little bass, its rhythm and
//!   loudness drive what the bass would (so it doesn't go limp);
//! * **loudness against the song's own loud parts** sets how wide and fast it all goes.
//!
//! The spin has momentum (hits kick it, it eases back), and every 16 beats it turns around.

use crate::audio::AnalysisFrame;
use crate::config::VisualizerTuning;
use crate::gui::views::visualizer::SpectrumTracker;

/// Beats before the spin turns the other way.
const BEATS_PER_TURN: u32 = 16;

#[derive(Default)]
pub(crate) struct Motion {
    /// Seconds the storm has run.
    pub time: f32,
    /// Music time: how far along its path the wave has swung.
    pub tau: f32,
    /// A beat this frame: its strength (0 when none).
    pub kick: f32,
    /// The hit envelope (beats, settling within a beat).
    pub impact: f32,
    /// Rising bass (not just loud bass).
    pub punch: f32,
    /// Whips around the spine: rising mids, and every percussive hit.
    pub whip: f32,
    /// The ribbon's twist and the whole wave's turn (radians).
    pub spin: f32,
    pub yaw: f32,
    /// Cubes orbiting the spine with the rhythm (continuous).
    pub swirl: f32,
    /// How full the music is (0..1, against the song's own range).
    pub fullness: f32,
    /// What hits and puffs scale with: the bass, or for songs without much, rhythm and
    /// loudness.
    pub drive: f32,
    /// How little the bass carries the song right now (0..1).
    pub bassless: f32,
    /// Off-beat hits (only with the "Off-beat hits" knob up).
    pub onset: f32,

    beats: u64,
    onsets: u64,
    /// Percussive hits between beats, fading fast.
    groove: f32,
    slow_bass: f32,
    slow_mid: f32,
    /// Spin speed (with momentum), its direction (eased toward `turn`) and beats since it
    /// last turned around.
    spin_vel: f32,
    dir: f32,
    turn: f32,
    beats_this_way: u32,
    spectrum: SpectrumTracker,
}

fn ease(dt: f32, rate: f32) -> f32 {
    1.0 - (-dt * rate).exp()
}

impl Motion {
    /// Per band, 1 on a sharp rise, fading.
    pub(crate) fn flares(&self) -> [f32; crate::audio::BANDS] {
        self.spectrum.flare
    }

    pub(crate) fn update(&mut self, a: &AnalysisFrame, dt: f32, k: &VisualizerTuning) {
        let dt = dt.clamp(0.0, 0.1);
        if self.turn == 0.0 {
            self.turn = 1.0;
            self.dir = 1.0;
        }
        self.time += dt;

        // Beats: the tracker's, as hard as the beat (accents harder, ghosts soft).
        let kick = if a.beat_count != self.beats {
            a.beat_strength.clamp(0.0, 1.25)
        } else {
            0.0
        };
        self.beats = a.beat_count;
        self.kick = kick;
        let tempo = if a.bpm > 0.0 {
            (a.bpm / 120.0).clamp(0.6, 1.7)
        } else {
            1.0
        };
        // A hit settles before the next beat, at any tempo.
        self.impact = (self.impact * (-dt * k.recover * tempo).exp()).max(kick);

        // Percussive hits that aren't beats: the hats and snares of a loop.
        let hit = if a.onset_count != self.onsets && kick == 0.0 {
            a.onset_strength
        } else {
            0.0
        };
        self.onsets = a.onset_count;
        self.groove = (self.groove * (-dt * 10.0).exp()).max(hit);

        // Rising bass and mids (not loud bass and mids).
        self.slow_bass += (a.bass - self.slow_bass) * ease(dt, 2.5);
        self.punch = ((a.bass - self.slow_bass) * 3.0).clamp(0.0, 1.0);
        self.slow_mid += (a.mid - self.slow_mid) * ease(dt, 3.0);
        let mid_rise = ((a.mid - self.slow_mid) * 4.0).clamp(0.0, 1.0);
        self.whip = mid_rise.max(0.9 * self.groove);

        // How much of the song is bass right now, and what drives the hits when it isn't.
        let share = self.slow_bass / (self.slow_bass + self.slow_mid + 0.05);
        self.bassless = (1.0 - 1.6 * share).clamp(0.0, 1.0);
        let rhythm = 0.5 * a.energy + 0.5 * self.groove.max(a.busy * a.energy);
        self.drive = a.bass.max(self.bassless * rhythm);
        let level = ((a.bass + a.mid + a.treble) / 3.0).max(0.6 * a.energy);
        self.fullness += (level - self.fullness) * ease(dt, 1.5);

        // Spin: a speed with momentum, kicked by hits, easing toward what the mids and the
        // rhythm's busyness ask for; turning around every few bars.
        let busy = a.busy * a.energy;
        let target = k.spin + k.mid_spin * (0.5 * a.mid + 0.5 * busy) + k.activity_speed * busy;
        self.spin_vel += (0.5 * kick + 1.2 * hit * (0.4 + self.bassless)) * k.mid_spin;
        self.spin_vel += (target - self.spin_vel) * ease(dt, 1.6);
        if kick > 0.3 {
            self.beats_this_way += 1;
            if self.beats_this_way >= BEATS_PER_TURN {
                self.beats_this_way = 0;
                self.turn = -self.turn;
            }
        }
        self.dir += (self.turn - self.dir) * ease(dt, 1.2);
        self.spin += self.spin_vel * self.dir * dt;
        // The whole wave turns too, more slowly, so a spinning song visibly goes round.
        self.yaw += 0.3 * self.spin_vel * self.dir * dt;
        self.swirl = 1.4 * busy + self.groove;

        // Every band against its own range; off-beat hits only if asked for.
        let (activity, onset) = self.spectrum.update(a, dt);
        self.onset = onset.map_or(0.0, |s| s * k.onset_kick * self.bassless);
        if self.onset > 0.0 {
            self.impact = self.impact.max((0.4 * self.onset).min(1.0));
        }

        // Flow: in time with the song, faster with the levels or the rhythm, lunging on hits
        // (and, without bass, a little on the loop's hits too).
        let levels = k.bass_speed * a.bass + k.mid_speed * a.mid + k.treble_speed * a.treble;
        let moving = k.activity_speed * 2.0 * activity.max(busy);
        self.tau += dt
            * (k.speed * tempo
                + levels.max(moving)
                + k.lunge * self.impact * self.impact
                + 0.5 * k.lunge * self.groove * self.groove * self.bassless);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten seconds of a song at 60 fps: beats every 0.5 s, other hits `per_beat` times as
    /// often in between, with the given levels.
    fn play(bass: f32, mid: f32, treble: f32, hits_per_beat: u32, busy: f32) -> Stats {
        let k = VisualizerTuning::default();
        let mut m = Motion::default();
        let mut a = AnalysisFrame {
            bass,
            mid,
            treble,
            bpm: 120.0,
            busy,
            energy: 0.85,
            ..Default::default()
        };
        a.bands = [(bass + mid + treble) / 3.0; crate::audio::BANDS];
        let dt = 1.0 / 60.0;
        let mut s = Stats::default();
        for f in 0..600 {
            let t = f as f32 * dt;
            let step = 0.5 / hits_per_beat as f32;
            let prev = ((t - dt) / step).floor();
            if (t / step).floor() != prev {
                a.seq += 1;
                a.onset_count += 1;
                a.onset_strength = 0.6;
                if ((t / 0.5).floor()) != ((t - dt) / 0.5).floor() {
                    a.beat_count += 1;
                    a.beat_strength = 0.7;
                }
            }
            m.update(&a, dt, &k);
            s.spin += (m.spin_vel * dt).abs();
            s.drive += m.drive / 600.0;
            s.whip += m.whip / 600.0;
            s.swirl += m.swirl / 600.0;
            s.flow = m.tau;
        }
        s
    }

    #[derive(Default, Debug)]
    struct Stats {
        spin: f32,
        drive: f32,
        whip: f32,
        swirl: f32,
        flow: f32,
    }

    #[test]
    fn a_drum_loop_without_bass_spins_and_swirls_more_than_a_plain_kick() {
        let kick = play(0.75, 0.35, 0.25, 1, 0.25);
        let loop_ = play(0.08, 0.5, 0.55, 4, 1.0);
        assert!(loop_.spin > kick.spin * 1.4, "spin {loop_:?} vs {kick:?}");
        assert!(loop_.whip > kick.whip * 2.0, "whip {loop_:?} vs {kick:?}");
        assert!(
            loop_.swirl > kick.swirl * 2.0,
            "swirl {loop_:?} vs {kick:?}"
        );
        // Without bass it still hits about as hard and keeps flowing.
        assert!(
            loop_.drive > kick.drive * 0.6,
            "drive {loop_:?} vs {kick:?}"
        );
        assert!(loop_.flow > kick.flow * 0.8, "flow {loop_:?} vs {kick:?}");
    }

    #[test]
    fn the_spin_turns_around_every_few_bars() {
        let k = VisualizerTuning::default();
        let mut m = Motion::default();
        let mut a = AnalysisFrame {
            mid: 0.5,
            energy: 0.8,
            busy: 0.8,
            ..Default::default()
        };
        let mut signs = vec![];
        for f in 0..(60 * 20) {
            if f % 30 == 0 {
                a.beat_count += 1;
                a.beat_strength = 0.7;
            }
            m.update(&a, 1.0 / 60.0, &k);
            if f % 60 == 0 {
                signs.push(m.dir.signum());
            }
        }
        assert!(signs.contains(&1.0) && signs.contains(&-1.0), "{signs:?}");
    }

    #[test]
    fn silence_settles() {
        let k = VisualizerTuning::default();
        let mut m = Motion::default();
        let a = AnalysisFrame::default();
        for _ in 0..600 {
            m.update(&a, 1.0 / 60.0, &k);
        }
        assert!(m.impact < 1e-3 && m.whip < 1e-3 && m.swirl < 1e-3 && m.drive < 1e-3);
    }
}
