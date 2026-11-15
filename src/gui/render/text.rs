//! Text via glyphon/cosmic-text, with a shaped-buffer cache so steady frames don't reshape.

use std::collections::HashMap;

use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport,
};

use crate::gui::render::{LAYERS, Layer, Weight};

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    size_bits: u32,
    weight: Weight,
}

struct Entry {
    buffer: Buffer,
    width: f32,
    /// Distance from the top of the buffer to the first baseline.
    baseline: f32,
    last_used: u64,
}

pub struct TextSystem {
    fonts: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderers: Vec<TextRenderer>,
    family: Option<String>,
    cache: HashMap<Key, Entry>,
    fit_cache: HashMap<(Key, u32), String>,
    frame: u64,
}

fn weight(w: Weight) -> glyphon::Weight {
    match w {
        Weight::Regular => glyphon::Weight::NORMAL,
        Weight::Medium => glyphon::Weight::MEDIUM,
        Weight::Bold => glyphon::Weight::BOLD,
    }
}

impl TextSystem {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        family: Option<String>,
    ) -> Self {
        let fonts = FontSystem::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderers = (0..LAYERS)
            .map(|_| TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None))
            .collect();
        Self {
            fonts,
            swash: SwashCache::new(),
            atlas,
            viewport,
            renderers,
            family: family.filter(|f| !f.trim().is_empty()),
            cache: HashMap::new(),
            fit_cache: HashMap::new(),
            frame: 0,
        }
    }

    fn entry(&mut self, text: &str, size: f32, w: Weight) -> &mut Entry {
        let key = Key {
            text: text.to_owned(),
            size_bits: size.to_bits(),
            weight: w,
        };
        let frame = self.frame;
        if !self.cache.contains_key(&key) {
            let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(size, (size * 1.3).ceil()));
            buffer.set_size(None, None);
            let family = match &self.family {
                Some(f) => Family::Name(f),
                None => Family::SansSerif,
            };
            let attrs = Attrs::new().family(family).weight(weight(w));
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.fonts, false);
            let width = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
            let baseline = buffer.layout_runs().next().map_or(size, |r| r.line_y);
            self.cache.insert(
                key.clone(),
                Entry {
                    buffer,
                    width,
                    baseline,
                    last_used: frame,
                },
            );
        }
        let e = self.cache.get_mut(&key).expect("just inserted");
        e.last_used = frame;
        e
    }

    /// Width and line height in physical pixels.
    pub fn measure(&mut self, text: &str, size: f32, w: Weight) -> (f32, f32) {
        let width = self.entry(text, size, w).width;
        (width, (size * 1.3).ceil())
    }

    /// Offset from the text's top to its baseline, in physical pixels. Differs per font
    /// (weights are often separate files with different metrics).
    pub fn baseline(&mut self, text: &str, size: f32, w: Weight) -> f32 {
        self.entry(text, size, w).baseline
    }

    /// `text`, shortened with `…` to fit `max_width` physical pixels.
    pub fn fit(&mut self, text: &str, size: f32, w: Weight, max_width: f32) -> String {
        if self.measure(text, size, w).0 <= max_width {
            return text.to_owned();
        }
        let key = (
            Key {
                text: text.to_owned(),
                size_bits: size.to_bits(),
                weight: w,
            },
            max_width as u32,
        );
        if let Some(s) = self.fit_cache.get(&key) {
            return s.clone();
        }
        // Binary search the longest prefix that fits with an ellipsis.
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let candidate: String = chars[..mid]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_owned()
                + "…";
            if self.measure(&candidate, size, w).0 <= max_width {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let out = if lo == 0 {
            String::new()
        } else {
            chars[..lo].iter().collect::<String>().trim_end().to_owned() + "…"
        };
        if self.fit_cache.len() > 4096 {
            self.fit_cache.clear();
        }
        self.fit_cache.insert(key, out.clone());
        out
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: (u32, u32),
        layers: &[Layer],
    ) -> anyhow::Result<()> {
        self.viewport.update(
            queue,
            Resolution {
                width: size.0,
                height: size.1,
            },
        );
        // Make sure every buffer exists before borrowing them all immutably.
        for layer in layers {
            for t in &layer.texts {
                self.entry(&t.text, t.size, t.weight);
            }
        }
        for (i, layer) in layers.iter().enumerate() {
            let areas = layer.texts.iter().map(|t| {
                let key = Key {
                    text: t.text.clone(),
                    size_bits: t.size.to_bits(),
                    weight: t.weight,
                };
                let [r, g, b, a] = t.color.to_rgba8();
                TextArea {
                    buffer: &self.cache[&key].buffer,
                    left: t.x.round(),
                    top: t.y.round(),
                    scale: 1.0,
                    bounds: TextBounds {
                        left: t.clip.x.floor() as i32,
                        top: t.clip.y.floor() as i32,
                        right: (t.clip.x + t.clip.w).ceil() as i32,
                        bottom: (t.clip.y + t.clip.h).ceil() as i32,
                    },
                    default_color: glyphon::Color::rgba(r, g, b, a),
                    custom_glyphs: &[],
                }
            });
            self.renderers[i].prepare(
                device,
                queue,
                &mut self.fonts,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            )?;
        }
        Ok(())
    }

    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>, layer: usize) -> anyhow::Result<()> {
        self.renderers[layer].render(&self.atlas, &self.viewport, pass)?;
        Ok(())
    }

    /// Drops buffers not used for a few frames and compacts the glyph atlas.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.cache.retain(|_, e| frame - e.last_used < 120);
        self.atlas.trim();
        self.frame += 1;
    }
}
