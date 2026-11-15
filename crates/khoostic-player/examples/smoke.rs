//! End-to-end smoke test against the real audio device, at volume 0.
//!
//! cargo run -p khoostic-player --example smoke -- <short1> <short2> <long track>
//!
//! Checks: track transitions (gapless), position accuracy, seeking, pause, prev-restarts,
//! and that the analysis feed is alive.

use std::time::{Duration, Instant};

use khoostic_audio::{EngineConfig, PlaybackState};
use khoostic_core::Track;
use khoostic_player::{Player, PlayerCommand as Cmd, PlayerEvent, Resolvers};

fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(paths.len() == 3, "need <short1> <short2> <long>");
    let config = EngineConfig {
        volume: 0.0,
        ..Default::default()
    };
    let player = Player::start(config, Resolvers::with_defaults())?;
    let t0 = Instant::now();
    let mut failures = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("[{}] {name}: {detail}", if ok { " ok " } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    player.send(Cmd::Enqueue(paths.iter().map(Track::from_path).collect()));
    player.send(Cmd::Next);

    // Wait until the long (third) track starts, logging transitions.
    let mut starts = Vec::new();
    while starts.len() < 3 && t0.elapsed() < Duration::from_secs(60) {
        if let Ok(PlayerEvent::TrackChanged(Some(e))) =
            player.events().recv_timeout(Duration::from_secs(10))
        {
            println!(
                "  {:>7.3}s  started {}",
                t0.elapsed().as_secs_f64(),
                e.track.title
            );
            starts.push((t0.elapsed(), e.track.title.clone()));
        }
    }
    check(
        "three tracks started in order",
        starts.len() == 3,
        format!("{starts:?}"),
    );

    std::thread::sleep(Duration::from_secs(2));
    let s = player.snapshot();
    check(
        "position advances",
        (1.7..2.4).contains(&s.position.as_secs_f64()),
        format!(
            "{:.3}s after 2s, duration {:?}, {} Hz",
            s.position.as_secs_f64(),
            s.duration,
            s.sample_rate
        ),
    );

    let a = player.analysis();
    let lit = a.bands.iter().filter(|&&b| b > 0.05).count();
    std::thread::sleep(Duration::from_millis(200));
    let seq2 = player.analysis().seq;
    check(
        "analysis feed alive",
        seq2 > a.seq && lit > 0,
        format!("seq {}→{seq2}, {lit} bands lit, rms {:.3}", a.seq, a.rms),
    );

    player.send(Cmd::Seek(Duration::from_secs(60)));
    std::thread::sleep(Duration::from_millis(300));
    let s = player.snapshot();
    check(
        "seek to 60s",
        (60.1..60.6).contains(&s.position.as_secs_f64()),
        format!("{:.3}s, 300ms after seeking", s.position.as_secs_f64()),
    );

    player.send(Cmd::Pause);
    std::thread::sleep(Duration::from_millis(100));
    let p1 = player.snapshot().position;
    std::thread::sleep(Duration::from_millis(500));
    let s = player.snapshot();
    check(
        "pause holds position",
        s.state == PlaybackState::Paused && s.position == p1,
        format!(
            "{:?} {:.3}s → {:.3}s",
            s.state,
            p1.as_secs_f64(),
            s.position.as_secs_f64()
        ),
    );
    player.send(Cmd::Play);
    std::thread::sleep(Duration::from_millis(500));
    let s = player.snapshot();
    check(
        "resume",
        s.state == PlaybackState::Playing && s.position > p1,
        format!("{:.3}s", s.position.as_secs_f64()),
    );

    player.send(Cmd::SeekBy(-10_000));
    std::thread::sleep(Duration::from_millis(200));
    let s = player.snapshot();
    check(
        "relative seek -10s",
        (50.4..51.2).contains(&s.position.as_secs_f64()),
        format!("{:.3}s", s.position.as_secs_f64()),
    );

    player.send(Cmd::Prev);
    std::thread::sleep(Duration::from_millis(200));
    let s = player.snapshot();
    check(
        "prev restarts track",
        s.position < Duration::from_millis(500),
        format!("{:.3}s", s.position.as_secs_f64()),
    );

    player.send(Cmd::Prev);
    std::thread::sleep(Duration::from_millis(200));
    let title = player.snapshot().current.map(|e| e.track.title.clone());
    check(
        "prev again goes back a track",
        title.as_deref() == Some(starts[1].1.as_str()),
        format!("{title:?}"),
    );

    player.send(Cmd::Stop);
    std::thread::sleep(Duration::from_millis(200));
    let s = player.snapshot();
    check(
        "stop",
        s.state == PlaybackState::Stopped,
        format!("{:?}", s.state),
    );

    while let Ok(ev) = player.events().try_recv() {
        if let PlayerEvent::Error(e) = ev {
            check("no errors", false, e);
        }
    }
    drop(player);
    println!(
        "{}",
        if failures == 0 {
            "ALL PASSED".to_string()
        } else {
            format!("{failures} FAILED")
        }
    );
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
