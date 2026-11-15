//! Renders a test card offscreen to check the Vulkan renderer: `cargo run -p khoostic-gui --example shapes -- out.png`

use khoostic_gui::canvas::{Canvas, Color, Rect, Weight};
use khoostic_gui::gpu::Gpu;
use khoostic_gui::renderer::Renderer;

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "shapes.png".into());
    let gpu = pollster::block_on(Gpu::headless())?;
    println!("adapter: {:?}", gpu.adapter.get_info());
    let mut r = Renderer::new(gpu, wgpu::TextureFormat::Rgba8UnormSrgb, None);
    let size = (640, 520);
    let mut c = Canvas::new(size, 1.0, Color::hex("#0e0e12").unwrap());
    let accent = Color::hex("#89dceb").unwrap();
    c.rect(
        Rect::new(20.0, 20.0, 200.0, 120.0),
        Color::hex("#17171d").unwrap(),
        12.0,
    );
    c.gradient(
        Rect::new(240.0, 20.0, 120.0, 120.0),
        accent,
        Color::hex("#cba6f7").unwrap(),
        16.0,
    );
    c.outline(Rect::new(380.0, 20.0, 120.0, 120.0), accent, 12.0, 2.0);
    c.glow(
        Rect::new(540.0, 50.0, 60.0, 60.0),
        accent.alpha(0.6),
        30.0,
        16.0,
    );
    c.circle(570.0, 80.0, 20.0, accent);
    c.triangle(Rect::new(40.0, 180.0, 40.0, 44.0), accent, true, 3.0);
    c.triangle(
        Rect::new(100.0, 180.0, 40.0, 44.0),
        Color::WHITE,
        false,
        3.0,
    );
    c.push_clip(Rect::new(160.0, 180.0, 60.0, 60.0));
    c.circle(220.0, 240.0, 50.0, Color::hex("#f38ba8").unwrap()); // clipped quarter
    c.pop_clip();
    c.text_at("khoostic", 40.0, 260.0, 32.0, Weight::Bold, accent);
    c.text_at(
        "Minimal, Vulkan-rendered — 日本語 ✓",
        40.0,
        305.0,
        16.0,
        Weight::Regular,
        Color::WHITE,
    );
    let fitted = r.text.fit(
        "A very long title that will not fit in the box",
        14.0,
        Weight::Medium,
        180.0,
    );
    c.text_at(fitted, 380.0, 200.0, 14.0, Weight::Medium, Color::WHITE);
    c.set_layer(1);
    c.rect(
        Rect::new(300.0, 250.0, 300.0, 80.0),
        Color::BLACK.alpha(0.7),
        10.0,
    );
    c.text_at(
        "overlay layer",
        320.0,
        275.0,
        18.0,
        Weight::Medium,
        Color::WHITE,
    );
    // The icon set, at two sizes.
    c.set_layer(0);
    use khoostic_gui::icons::{Icon, draw};
    let icons = [
        Icon::Play,
        Icon::Pause,
        Icon::Prev,
        Icon::Next,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::RepeatOne,
        Icon::Volume(1.0),
        Icon::Volume(0.3),
        Icon::Volume(0.0),
        Icon::Search,
        Icon::Album,
        Icon::Artist,
        Icon::Song,
        Icon::Playlist,
        Icon::Queue,
        Icon::Folder,
        Icon::Visualizer,
        Icon::Settings,
        Icon::Heart,
        Icon::Plus,
        Icon::Close,
        Icon::Back,
        Icon::Up,
        Icon::Spotify,
    ];
    for (i, icon) in icons.iter().enumerate() {
        let (x, y) = (
            20.0 + (i % 13) as f32 * 47.0,
            350.0 + (i / 13) as f32 * 40.0,
        );
        draw(&mut c, *icon, Rect::new(x, y, 24.0, 24.0), Color::WHITE);
        draw(&mut c, *icon, Rect::new(x, y + 90.0, 16.0, 16.0), accent);
    }
    c.arc(600.0, 470.0, 14.0, 0.0, 4.0, 3.0, accent);
    let px = r.capture(&c, size)?;
    let file = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut enc = png::Encoder::new(file, size.0, size.1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&px)?;
    println!("wrote {out}");
    Ok(())
}
