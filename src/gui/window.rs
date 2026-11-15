//! The window: winit events in, frames out, paced to the configured frame rate (lower in the
//! background, nothing while minimized).

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::config::Config;
use crate::gui::app;
use crate::gui::app::Gui;
use crate::gui::render::gpu::{Gpu, WindowSurface};
use crate::gui::render::{Canvas, Renderer};
use crate::gui::ui::{Input, Key, KeyPress};
use crate::player::{Player, PlayerEvent};
use crate::services::{Launch, Services};

/// Redraw interval when nothing moves (clock text, toasts expiring).
const IDLE_REDRAW: Duration = Duration::from_millis(500);

/// Wake-up interval while minimized (no drawing).
const HIDDEN_TICK: Duration = Duration::from_secs(1);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Pixels scrolled per wheel notch.
const WHEEL_STEP: f32 = 84.0;

/// Opens the window and runs until it closes.
pub fn run(app: Services, config: &Config, launch: Launch) -> anyhow::Result<()> {
    let Services {
        player,
        library,
        spotify,
        discovery,
    } = app;
    launch.apply(&player);
    let (gui, problems) = Gui::new(config, library, spotify, discovery);
    let mut app = WinitApp {
        player,
        config: config.clone(),
        gui,
        window: None,
        input: Input::default(),
        modifiers: ModifiersState::empty(),
        last_click: None,
        animating: true,
        error: None,
        last_title: String::new(),
        focused: true,
        occluded: false,
        dirty: true,
        last_frame: Instant::now(),
        frame_due: Instant::now(),
        pointer: false,
        frames: 0,
        stats_since: Instant::now(),
    };
    if !problems.is_empty() {
        app.gui.toast(problems.join("; "));
    }
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

struct WindowState {
    window: Arc<Window>,
    surface: WindowSurface,
    renderer: Renderer,
}

struct WinitApp {
    player: Player,
    config: Config,
    gui: Gui,
    window: Option<WindowState>,
    input: Input,
    modifiers: ModifiersState,
    last_click: Option<(Instant, (f32, f32))>,
    animating: bool,
    error: Option<anyhow::Error>,
    last_title: String,
    focused: bool,
    /// Covered or minimized: nothing is drawn.
    occluded: bool,
    /// Input arrived since the last frame.
    dirty: bool,
    last_frame: Instant,
    /// When the next animation frame is due. Advanced by fixed steps so wake-up latency
    /// doesn't accumulate into a lower frame rate.
    frame_due: Instant,
    /// The hand cursor is showing.
    pointer: bool,
    /// Frames drawn since `stats_since` (debug log of the real frame rate).
    frames: u32,
    stats_since: Instant,
}

impl WinitApp {
    fn scale(&self) -> f32 {
        let os = self
            .window
            .as_ref()
            .map_or(1.0, |w| w.window.scale_factor() as f32);
        os * self.config.gui.scale.clamp(0.5, 4.0)
    }

    /// Nothing on screen to draw: minimized, fully covered, or zero-sized.
    fn hidden(&self) -> bool {
        let Some(ws) = &self.window else { return true };
        self.occluded
            || ws.window.is_minimized() == Some(true)
            || ws.surface.config.width == 0
            || ws.surface.config.height == 0
    }

    /// Time between frames while something animates.
    fn frame_interval(&self) -> Duration {
        let fps = if self.focused {
            self.gui.fps
        } else {
            self.gui.background_fps
        };
        Duration::from_secs_f64(1.0 / fps.max(1) as f64)
    }

    /// Work that must continue while hidden: player errors and the window title.
    fn background_tick(&mut self) {
        while let Ok(ev) = self.player.events().try_recv() {
            if let PlayerEvent::Error(e) = ev {
                self.gui.toast(e);
            }
        }
        self.update_title();
    }

    fn update_title(&mut self) {
        let Some(ws) = self.window.as_ref() else {
            return;
        };
        let title = match self.player.snapshot().current {
            Some(e) => format!("{} — Khoostic", e.track.title),
            None => "Khoostic".to_string(),
        };
        if title != self.last_title {
            ws.window.set_title(&title);
            self.last_title = title;
        }
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let interval = self.frame_interval();
        self.frame_due += interval;
        if self.frame_due + interval < now || self.frame_due > now + interval {
            // Idle for a while (or the rate changed): restart the cadence from now.
            self.frame_due = now + interval;
        }
        self.last_frame = now;
        self.dirty = false;
        self.frames += 1;
        let span = self.stats_since.elapsed();
        if span >= Duration::from_secs(2) {
            tracing::debug!(
                fps = self.frames as f32 / span.as_secs_f32(),
                focused = self.focused,
                animating = self.animating,
                "frame rate"
            );
            self.frames = 0;
            self.stats_since = Instant::now();
        }
        self.background_tick();
        let scale = self.scale();
        let Some(ws) = self.window.as_mut() else {
            return;
        };
        let size = (ws.surface.config.width, ws.surface.config.height);
        let mut canvas = Canvas::new(size, scale, self.gui.theme.background);
        let result = self.gui.frame(
            &mut canvas,
            app::Gfx {
                text: &mut ws.renderer.text,
                images: &mut ws.renderer.images,
            },
            &self.input,
            &self.player,
        );
        self.animating = result.animating;
        self.input.end_frame();
        if result.pointer != self.pointer {
            self.pointer = result.pointer;
            ws.window.set_cursor(if result.pointer {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            });
        }

        let frame = match ws.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(_) | wgpu::CurrentSurfaceTexture::Outdated => {
                let (w, h) = (ws.surface.config.width, ws.surface.config.height);
                ws.surface.resize(&ws.renderer.gpu.device, w, h);
                ws.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                match ws.renderer.gpu.instance.create_surface(ws.window.clone()) {
                    Ok(s) => {
                        ws.surface.surface = s;
                        ws.surface
                            .surface
                            .configure(&ws.renderer.gpu.device, &ws.surface.config);
                    }
                    Err(e) => tracing::error!("recreating surface: {e}"),
                }
                ws.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Validation => {
                tracing::error!("surface validation error");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if let Err(e) = ws.renderer.render(&canvas, &view, size) {
            tracing::error!("render failed: {e:#}");
        }
        ws.window.pre_present_notify();
        ws.renderer.gpu.queue.present(frame);
    }

    fn key(&mut self, key: &WKey) {
        let k = match key {
            WKey::Named(n) => match n {
                NamedKey::Space => Key::Space,
                NamedKey::Enter => Key::Enter,
                NamedKey::Escape => Key::Escape,
                NamedKey::Backspace => Key::Backspace,
                NamedKey::Delete => Key::Delete,
                NamedKey::Tab => Key::Tab,
                NamedKey::ArrowUp => Key::Up,
                NamedKey::ArrowDown => Key::Down,
                NamedKey::ArrowLeft => Key::Left,
                NamedKey::ArrowRight => Key::Right,
                NamedKey::Home => Key::Home,
                NamedKey::End => Key::End,
                NamedKey::PageUp => Key::PageUp,
                NamedKey::PageDown => Key::PageDown,
                _ => return,
            },
            WKey::Character(s) => match s.chars().next() {
                Some(' ') => Key::Space,
                Some(c) => Key::Char(c),
                None => return,
            },
            _ => return,
        };
        self.input.keys.push(KeyPress {
            key: k,
            shift: self.modifiers.shift_key(),
            ctrl: self.modifiers.control_key(),
        });
    }
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let g = &self.config.gui;
        let attrs = Window::default_attributes()
            .with_title("Khoostic")
            .with_inner_size(LogicalSize::new(g.width.max(640), g.height.max(420)))
            .with_min_inner_size(LogicalSize::new(720, 480))
            .with_theme(Some(winit::window::Theme::Dark))
            .with_visible(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.error = Some(e.into());
                return event_loop.exit();
            }
        };
        let display = Box::new(event_loop.owned_display_handle());
        match pollster::block_on(Gpu::for_window(window.clone(), display, g.vsync)) {
            Ok((gpu, surface)) => {
                let font = Some(g.font.clone());
                let renderer = Renderer::new(gpu, surface.config.format, font);
                self.window = Some(WindowState {
                    window: window.clone(),
                    surface,
                    renderer,
                });
                // Shown only once the first frame can render, so there's no white flash.
                self.redraw();
                window.set_visible(true);
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let scale = self.scale();
        // Input only marks the frame dirty; `about_to_wait` decides when to draw, so a
        // 1000 Hz mouse can't push rendering past the frame-rate cap.
        let mut dirty = true;
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(ws) = self.window.as_mut() {
                    ws.surface
                        .resize(&ws.renderer.gpu.device, size.width, size.height);
                    // Draw right away so live resizing doesn't smear.
                    if size.width > 0 && size.height > 0 {
                        ws.window.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if !self.hidden() {
                    self.redraw();
                }
                dirty = false;
            }
            WindowEvent::Focused(f) => self.focused = f,
            WindowEvent::Occluded(o) => self.occluded = o,
            WindowEvent::CursorMoved { position, .. } => {
                self.input.mouse = Some((position.x as f32 / scale, position.y as f32 / scale));
            }
            WindowEvent::CursorLeft { .. } => self.input.mouse = None,
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    let pos = self.input.mouse.unwrap_or_default();
                    let now = Instant::now();
                    self.input.double_click = self.last_click.is_some_and(|(t, p)| {
                        now - t < DOUBLE_CLICK
                            && (p.0 - pos.0).abs() < 5.0
                            && (p.1 - pos.1).abs() < 5.0
                    });
                    self.last_click = if self.input.double_click {
                        None
                    } else {
                        Some((now, pos))
                    };
                    self.input.pressed = true;
                    self.input.down = true;
                }
                (MouseButton::Left, ElementState::Released) => {
                    self.input.released = true;
                    self.input.down = false;
                }
                (MouseButton::Right, ElementState::Pressed) => self.input.right_pressed = true,
                (MouseButton::Back, ElementState::Pressed) => self.input.back_pressed = true,
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                self.input.scroll -= match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * WHEEL_STEP,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / scale,
                };
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.key(&event.logical_key);
            }
            WindowEvent::DroppedFile(path) => self.input.dropped.push(path),
            _ => dirty = false,
        }
        self.dirty |= dirty;
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        let now = Instant::now();
        if self.hidden() {
            // Minimized/covered: no frames at all, just keep the title and errors current.
            self.background_tick();
            event_loop.set_control_flow(ControlFlow::WaitUntil(now + HIDDEN_TICK));
            return;
        }
        let next = if self.dirty || self.animating {
            self.frame_due
        } else {
            self.last_frame + IDLE_REDRAW
        };
        if now >= next {
            if let Some(ws) = &self.window {
                ws.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Wait);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        }
    }
}
