# Khoostic — notes for coding agents

Khoostic is a music player written in Rust: one crate, one binary (`khoostic`). The Vulkan-rendered
GUI is the main interface; `--tui` gives a vim-style terminal UI over the same player core. Local
files, Spotify (librespot) and Last.fm today; Navidrome/Subsonic, MPD, Discord presence and
listen-along jams are planned.

## Layout

One crate (`src/lib.rs` + a thin `src/main.rs`), split into modules. Modules are layered: each
only uses the ones listed above it here (keep it that way; no cycles back up).

| Module | Owns |
|---|---|
| `model/` | `Track`, `TrackLocator` (+ URI form), `CoverRef`, `Queue`, `Lyrics` (+ LRC parsing), song matching and `normalize`, natural sort. **No I/O.** |
| `pending.rs` | `Pending<T>`: a background result the UIs poll once per frame |
| `config.rs` | `config.toml` model; every field defaults, unknown keys are errors; `save_value` edits one key in place |
| `audio/` | `AudioSource` trait, symphonia decoding, `Conditioner` (stereo + rubato), engine thread, cpal output, FFT analysis feed |
| `player/` | `Player` controller (queue ↔ engine, gapless preload), `SourceResolver`s, file browser model |
| `library/` | lofty tags, cover cache (content-hashed thumbnails), SQLite + FTS5 index, incremental scan, playlists, liked local songs |
| `spotify/` | Browser sign-in (OAuth PKCE, cancellable local listener), library, search and lyrics over Spotify's client API (spclient, `api.rs`), artist pages (`artist.rs`), liked songs and followed artists (`collection.rs`), librespot streaming as an `AudioSource`, `SpotifyResolver` |
| `genres.rs` | Artist genres from MusicBrainz, falling back to Deezer's public API |
| `lastfm/` | Signed API client, browser sign-in, scrobble rules (`tracker.rs`, pure), background scrobbler with a persisted offline queue, similar tracks |
| `discovery.rs` | Song radio (Spotify radio + Last.fm similar, matched to local files first, library-only fallback) and the autoplay watcher |
| `lyrics.rs` | Lyrics providers in configured order: local `.lrc` / tags, Spotify (also for matched local songs), LRCLIB |
| `services.rs` | `Services::start`: library, Spotify, Last.fm, player and discovery wired together; `Launch` (paths from the command line) |
| `gui/` | `window.rs` event loop + frame pacing + the borderless window (resize edges, drag), `headless.rs` offscreen; `app.rs` state + frame layout (title bar with window buttons, Your Library panel, page, queue panel, player bar: `top_bar.rs`, `sidebar.rs`, `queue_panel.rs`, `player_bar.rs`), `menu.rs` (song and library menus), `spotify.rs` (online data: Spotify, lyrics, genres), `state.rs` (panel state saved to `gui-state.json`); `views/` one file per page (+ `lists.rs` track rows, `grid.rs` cover tiles, `page.rs` whole-page scrolling, cards, two-line song rows); `ui/` widgets, `anim.rs`, `icons.rs`, `text_field.rs`; `render/` canvas, SDF painter (boxes, triangles, lines, arcs), images, glyphon text, wgpu (Vulkan only) |
| `tui/` | ratatui frontend: keymap engine (vim notation, counts), modes, views, half-block covers |
| `main.rs` | CLI: `khoostic [paths] [--tui] [--play] [--shuffle]`, `devices`, `config`, `spotify`/`lastfm` `login/logout/status` |

Data flow: `AudioSource → Conditioner → SPSC ring → cpal callback → speakers`, with the callback
tapping the same samples into the analysis thread (`ArcSwap<AnalysisFrame>`). The engine tracks
track boundaries as frame segments, which is what makes gapless playback and exact positions work.
New backends (Spotify, Subsonic, MPD) plug in as a `SourceResolver` that returns an `AudioSource`.

## Commands

```sh
cargo build                          # dev deps are built with opt-level 3 (audio DSP needs it)
cargo test
cargo clippy --all-targets           # keep this warning-free
cargo fmt
cargo run --release                  # the GUI; or: -- --tui, -- <paths> --play, -- devices, -- config --init
KHOOSTIC_LOG=debug cargo run         # logs go to %TEMP%/khoostic.log, never stderr (TUI)
```

Real-hardware checks (examples):

```sh
cargo run --example audio_smoke  -- <short.wav> <short2.wav> <long track>
cargo run --example tui_headless -- <music dir>   # scripted keys, prints screens
cargo run --example gui_screens  -- <music dir> <out dir>   # PNG screenshots
cargo run --example gui_shapes   -- <out.png>     # renderer test card
cargo run --example library_scan -- <music dir> <scratch dir>
```

## Testing rules

- **Never make sound on the user's device.** Examples and smoke tests run the engine at volume 0
  (the visualizer tap is pre-volume, so analysis still gets exercised).
- Examples use a throwaway library at `%TEMP%/khoostic-example-lib`, never the real one.
- UI changes: verify by rendering. `gui::Headless` renders the real GUI offscreen with
  scripted `Input` and waits for cover art; read the PNGs. The TUI renders to `TestBackend`
  (note: its cell grid can show stale cells under wide CJK glyphs that real terminals clear).
- Unit tests must not need audio hardware, a GPU or the user's files; generate fixtures (the
  library integration test writes tagged WAVs with lofty).

## Conventions

- Rust 2024, clippy-clean, rustfmt defaults. `anyhow` at the edges, `thiserror` in libraries.
- The cpal callback is real-time: no locks, no allocation in the steady state, no blocking.
- Keep `model` free of I/O and heavy deps.
- GUI is immediate mode: views rebuild from state each frame; anything slow (DB queries, image
  decoding) is cached or moved off the render thread. Redraw only while something animates.
- GUI motion: ease values through `ui.anim` (by id, frame-rate independent) instead of hand-rolled
  timers; anything still moving keeps frames coming. The event loop paces frames to `gui.fps`
  (`background_fps` unfocused) and draws nothing while minimized. Fills that should look subtle
  use `Theme::wash` (blending is in linear light, so raw low alphas look far too bright).
  Within a layer, images draw above shapes: put shapes that must cover art on layer 1.
- Several dependencies are newer than most training data: symphonia 0.6, cpal 0.18, rubato 5,
  wgpu 30, glyphon 0.12 / cosmic-text 0.19, ratatui 0.30, rusqlite 0.43, lofty 0.25, winit 0.30.
  Read their source/examples in `~/.cargo/registry/src/` instead of guessing APIs.
- Spotify: never use the public Web API (api.spotify.com). Sign-in uses librespot's shared app id,
  which is permanently rate-limited there (every call is a 429). Go through the librespot
  session's spclient (context-resolve, extended-metadata, playlist v2), as `api.rs` does.
  Track radio (`get_radio_for_track`) works; Last.fm/ListenBrainz can add more recommendations.
  Local tracks can never be added to Spotify playlists. Endpoints known to work: search is the
  `spotify:search:<q>` context (the old `searchview` endpoints are gone); artist pages are
  `/artistview/v1/artist/<id>?platform=android&locale=en&country=` (monthly listeners, play
  counts, related); likes and follows are `/collection/v2/{paging,write}` with content type
  `application/vnd.collection-v2.spotify.proto`; lyrics are `/color-lyrics/v2/track/<id>` with an
  `app-platform: WebPlayer` header. Check changes that write to the account with the user first.

## Repo hygiene

- Commit and push straight to `master` as work lands (no branches/PRs), using the repo's
  configured git identity.
- `README.md` stays exactly `# Khoostic`. Don't add roadmaps, plans or design docs to the repo.

## Environment

Primary dev machine: Windows 11, Git Bash + PowerShell, NVIDIA GPU with Vulkan. No Python.
Config: `%APPDATA%\khoostic\config\config.toml`. Library DB: `%LOCALAPPDATA%\khoostic\data`.
Cover cache: `%LOCALAPPDATA%\khoostic\cache\covers`.
