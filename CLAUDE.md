# Khoostic — notes for coding agents

Khoostic is a music player written in Rust: one binary (`khoostic`) with a vim-style TUI and a
Vulkan-rendered GUI over a shared player core. Local files and Spotify (librespot) today; Last.fm, Navidrome/Subsonic, MPD, Discord
presence and listen-along jams are planned.

## Workspace

| Crate | Owns |
|---|---|
| `khoostic-core` | `Track`, `TrackLocator` (+ URI form), `CoverRef`, `Queue`, natural sort. **No I/O.** |
| `khoostic-audio` | `AudioSource` trait, symphonia decoding, `Conditioner` (stereo + rubato), engine thread, cpal output, FFT analysis feed |
| `khoostic-player` | `Player` controller (queue ↔ engine, gapless preload), `SourceResolver`s, file browser model |
| `khoostic-library` | lofty tags, cover cache (content-hashed thumbnails), SQLite + FTS5 index, incremental scan, playlists |
| `khoostic-spotify` | Browser sign-in (OAuth PKCE, cancellable local listener), Web API client, librespot streaming as an `AudioSource`, `SpotifyResolver` |
| `khoostic-config` | `config.toml` model; every field defaults, unknown keys are errors |
| `khoostic-tui` | ratatui frontend: keymap engine (vim notation, counts), modes, views, half-block covers |
| `khoostic-gui` | winit + wgpu (Vulkan only): SDF quad painter, image pipeline, glyphon text, immediate-mode UI (`app.rs` chrome, `views.rs` pages) |
| `src/main.rs` | CLI: `khoostic [tui]`, `gui`, `play`, `devices`, `config`, `spotify login|logout|status` |

Data flow: `AudioSource → Conditioner → SPSC ring → cpal callback → speakers`, with the callback
tapping the same samples into the analysis thread (`ArcSwap<AnalysisFrame>`). The engine tracks
track boundaries as frame segments, which is what makes gapless playback and exact positions work.
New backends (Spotify, Subsonic, MPD) plug in as a `SourceResolver` that returns an `AudioSource`.

## Commands

```sh
cargo build                          # dev deps are built with opt-level 3 (audio DSP needs it)
cargo test --workspace
cargo clippy --workspace --all-targets   # keep this warning-free
cargo fmt --all
cargo run --release -- gui           # or: tui, play <paths>, devices, config --init
KHOOSTIC_LOG=debug cargo run -- gui  # logs go to %TEMP%/khoostic.log, never stderr (TUI)
```

Real-hardware checks (examples):

```sh
cargo run -p khoostic-player  --example smoke    -- <short.wav> <short2.wav> <long track>
cargo run -p khoostic-tui     --example headless -- <music dir>   # scripted keys, prints screens
cargo run -p khoostic-gui     --example screens  -- <music dir> <out dir>   # PNG screenshots
cargo run -p khoostic-gui     --example shapes   -- <out.png>     # renderer test card
cargo run -p khoostic-library --example scan     -- <music dir> <scratch dir>
```

## Testing rules

- **Never make sound on the user's device.** Examples and smoke tests run the engine at volume 0
  (the visualizer tap is pre-volume, so analysis still gets exercised).
- Examples use a throwaway library at `%TEMP%/khoostic-example-lib`, never the real one.
- UI changes: verify by rendering. `khoostic_gui::Headless` renders the real GUI offscreen with
  scripted `Input` and waits for cover art; read the PNGs. The TUI renders to `TestBackend`
  (note: its cell grid can show stale cells under wide CJK glyphs that real terminals clear).
- Unit tests must not need audio hardware, a GPU or the user's files; generate fixtures (the
  library integration test writes tagged WAVs with lofty).

## Conventions

- Rust 2024, clippy-clean, rustfmt defaults. `anyhow` at the edges, `thiserror` in libraries.
- The cpal callback is real-time: no locks, no allocation in the steady state, no blocking.
- Keep `khoostic-core` free of I/O and heavy deps.
- GUI is immediate mode: views rebuild from state each frame; anything slow (DB queries, image
  decoding) is cached or moved off the render thread. Redraw only while something animates.
- Several dependencies are newer than most training data: symphonia 0.6, cpal 0.18, rubato 5,
  wgpu 30, glyphon 0.12 / cosmic-text 0.19, ratatui 0.30, rusqlite 0.43, lofty 0.25, winit 0.30.
  Read their source/examples in `~/.cargo/registry/src/` instead of guessing APIs.
- Spotify: `/recommendations` and audio-features are unavailable to new apps; recommendations
  must come from Last.fm/ListenBrainz. Local tracks can never be added to Spotify playlists.

## Repo hygiene

- Commit and push straight to `master` as work lands (no branches/PRs), using the repo's
  configured git identity.
- `README.md` stays exactly `# Khoostic`. Don't add roadmaps, plans or design docs to the repo.

## Environment

Primary dev machine: Windows 11, Git Bash + PowerShell, NVIDIA GPU with Vulkan. No Python.
Config: `%APPDATA%\khoostic\config\config.toml`. Library DB: `%LOCALAPPDATA%\khoostic\data`.
Cover cache: `%LOCALAPPDATA%\khoostic\cache\covers`.
