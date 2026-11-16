//! A jam between two players in one process, over localhost, at volume 0.
//!
//! cargo run --example jam_smoke -- <music dir>
//!
//! Checks: joining, the guest following the host's song and position (and how closely),
//! guest controls reaching the host (skip, pause, seek, adding a song), and leaving.

use std::sync::Arc;
use std::time::{Duration, Instant};

use khoostic::audio::EngineConfig;
use khoostic::config::JamConfig;
use khoostic::discovery::Discovery;
use khoostic::jam::{JamStatus, Jams};
use khoostic::library::Library;
use khoostic::model::Track;
use khoostic::player::{Player, PlayerCommand as Cmd, PlayerSnapshot, Resolvers, State};

fn main() -> anyhow::Result<()> {
    let music = std::env::args().nth(1).expect("<music dir>");
    let mut files: Vec<_> = std::fs::read_dir(&music)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "flac"))
        .collect();
    files.sort();
    anyhow::ensure!(files.len() >= 4, "need at least 4 .flac files");

    let tmp = std::env::temp_dir().join("khoostic-example-lib");
    let library = Library::open(&tmp.join("lib.db"), &tmp.join("covers"))?;
    let quiet = || EngineConfig {
        volume: 0.0,
        ..Default::default()
    };
    let peer = |name: &str, port: u16| -> anyhow::Result<(Player, Arc<Jams>)> {
        let player = Player::start(quiet(), Resolvers::with_defaults())?;
        let discovery = Discovery::new(library.clone(), None, None, false);
        // Only this computer: no firewall prompt for a test.
        let config = JamConfig {
            name: name.into(),
            port,
            bind: "127.0.0.1".into(),
        };
        let jams = Jams::new(&config, player.handle(), discovery, None, None);
        Ok((player, jams))
    };
    let (host, host_jam) = peer("Hosty", 47590)?;
    let (guest, guest_jam) = peer("Guesty", 47590)?;

    let mut failures = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("[{}] {name}: {detail}", if ok { " ok " } else { "FAIL" });
        failures += usize::from(!ok);
    };
    let title = |s: &PlayerSnapshot| s.current.as_ref().map(|e| e.track.title.clone());
    let wait = |what: &dyn Fn() -> bool, secs: u64| {
        let t = Instant::now();
        while !what() && t.elapsed() < Duration::from_secs(secs) {
            std::thread::sleep(Duration::from_millis(20));
        }
        t.elapsed()
    };

    host.send(Cmd::PlayTracks {
        tracks: files.iter().map(Track::from_path).collect(),
        start: 0,
    });
    std::thread::sleep(Duration::from_millis(1500));
    host_jam.host();
    let JamStatus::Hosting { invite, .. } = host_jam.status() else {
        anyhow::bail!("not hosting: {:?}", host_jam.status());
    };
    // Same machine: dial localhost with the invite's secret.
    let secret = invite.rsplit('/').next().unwrap();
    let port = invite
        .rsplit_once('/')
        .unwrap()
        .0
        .rsplit(':')
        .next()
        .unwrap();
    println!("invite: {invite}");
    let t = Instant::now();
    guest_jam.join(&format!("127.0.0.1:{port}/{secret}"))?;
    let joined = wait(
        &|| guest.snapshot().state == State::Playing && !guest.snapshot().loading,
        10,
    );
    check(
        "guest joins and plays",
        title(&guest.snapshot()) == title(&host.snapshot()),
        format!(
            "{:?} after {joined:?}; {:?}",
            title(&guest.snapshot()),
            guest_jam.status()
        ),
    );
    let _ = t;

    // How close are they? Sample both clocks repeatedly for a few seconds.
    let drift = |secs: u64| {
        let mut worst = 0i64;
        let mut sum = 0i64;
        let mut n = 0;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(secs) {
            let (h, g) = (host.snapshot(), guest.snapshot());
            if h.current.as_ref().map(|e| e.id) == g.current.as_ref().map(|e| e.id)
                && h.state == State::Playing
                && g.state == State::Playing
            {
                let d = g.position.as_millis() as i64 - h.position.as_millis() as i64;
                worst = worst.max(d.abs());
                sum += d;
                n += 1;
            }
            std::thread::sleep(Duration::from_millis(97));
        }
        (worst, if n > 0 { sum / n } else { i64::MAX }, n)
    };
    std::thread::sleep(Duration::from_secs(3)); // let the clock settle and a nudge happen
    let (worst, mean, n) = drift(5);
    check(
        "guest stays in time",
        worst < 150 && n > 20,
        format!("mean {mean} ms, worst {worst} ms over {n} samples"),
    );

    // The guest skips: the host moves, and the guest follows the host.
    let before = title(&host.snapshot());
    guest.send(Cmd::Next);
    let took = wait(&|| title(&host.snapshot()) != before, 5);
    let took2 = wait(
        &|| {
            title(&guest.snapshot()) == title(&host.snapshot())
                && (guest.snapshot().state == State::Playing && !guest.snapshot().loading)
        },
        5,
    );
    check(
        "guest's skip reaches the host",
        title(&host.snapshot()) != before && title(&guest.snapshot()) == title(&host.snapshot()),
        format!(
            "host now {:?} after {took:?}, guest after {took2:?}",
            title(&host.snapshot())
        ),
    );

    // The host seeks: the guest jumps along.
    host.send(Cmd::Seek(Duration::from_secs(60)));
    std::thread::sleep(Duration::from_secs(4));
    let (h, g) = (host.snapshot(), guest.snapshot());
    check(
        "host's seek reaches the guest",
        (g.position.as_millis() as i64 - h.position.as_millis() as i64).abs() < 200,
        format!("host {:?}, guest {:?}", h.position, g.position),
    );

    // The guest pauses: everyone pauses; then resumes.
    guest.send(Cmd::TogglePause);
    wait(&|| host.snapshot().state == State::Paused, 3);
    std::thread::sleep(Duration::from_millis(400));
    check(
        "guest's pause pauses everyone",
        host.snapshot().state == State::Paused && guest.snapshot().state == State::Paused,
        format!(
            "host {:?}, guest {:?}",
            host.snapshot().state,
            guest.snapshot().state
        ),
    );
    guest.send(Cmd::TogglePause);
    wait(
        &|| host.snapshot().state == State::Playing && !host.snapshot().loading,
        3,
    );
    std::thread::sleep(Duration::from_secs(4));
    let (worst, mean, n) = drift(3);
    check(
        "in time again after resuming",
        worst < 150 && n > 10,
        format!("mean {mean} ms, worst {worst} ms"),
    );

    // The guest adds a song next: it lands in the host's queue.
    let add = Track::from_path(&files[0]);
    guest.send(Cmd::PlayNext(vec![add.clone()]));
    std::thread::sleep(Duration::from_millis(800));
    let host_next = host
        .queue()
        .read()
        .peek_next()
        .map(|e| e.track.title.clone());
    let guest_next = guest
        .queue()
        .read()
        .peek_next()
        .map(|e| e.track.title.clone());
    check(
        "guest's song is queued for everyone",
        host_next.as_deref() == Some(add.title.as_str()) && guest_next == host_next,
        format!("host next {host_next:?}, guest next {guest_next:?}"),
    );

    // Leaving: the guest keeps playing on its own, and its controls are its own again.
    guest_jam.leave();
    std::thread::sleep(Duration::from_millis(300));
    let host_song = title(&host.snapshot());
    guest.send(Cmd::Next);
    std::thread::sleep(Duration::from_millis(800));
    check(
        "after leaving, the guest is on its own",
        title(&host.snapshot()) == host_song && guest_jam.status() == JamStatus::Idle,
        format!(
            "host {:?} stayed; guest {:?}; guest status {:?}; host sees {:?}",
            title(&host.snapshot()),
            title(&guest.snapshot()),
            guest_jam.status(),
            host_jam.status()
        ),
    );

    // Join again, then the host ends the jam.
    guest_jam.join(&format!("127.0.0.1:{port}/{secret}"))?;
    wait(
        &|| matches!(guest_jam.status(), JamStatus::Joined { .. }),
        5,
    );
    host_jam.leave();
    wait(&|| matches!(guest_jam.status(), JamStatus::Ended(_)), 5);
    check(
        "the host ending it ends it for guests",
        matches!(guest_jam.status(), JamStatus::Ended(_)),
        format!("{:?}", guest_jam.status()),
    );
    // A wrong secret is refused.
    host_jam.host();
    let JamStatus::Hosting { invite, .. } = host_jam.status() else {
        unreachable!()
    };
    let port = invite
        .rsplit_once('/')
        .unwrap()
        .0
        .rsplit(':')
        .next()
        .unwrap()
        .to_owned();
    guest_jam.join(&format!("127.0.0.1:{port}/wrongsecret"))?;
    wait(&|| matches!(guest_jam.status(), JamStatus::Ended(_)), 5);
    check(
        "a wrong invite is refused",
        matches!(guest_jam.status(), JamStatus::Ended(ref s) if s.contains("isn't for this jam")),
        format!("{:?}", guest_jam.status()),
    );

    println!("{failures} failed");
    std::process::exit(i32::from(failures > 0));
}
