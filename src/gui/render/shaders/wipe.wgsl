// The theme swipe: the last frame drawn with the old theme, wiped away by a slanted, wavy edge
// that glows in the new accent and pulls the old picture toward itself like liquid.

struct Wipe {
    size: vec2<f32>,
    progress: f32,
    scale: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> w: Wipe;
@group(0) @binding(1) var snap: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the screen.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let s = w.scale;
    let px = pos.xy;
    // The edge comes in at the top-right corner and sweeps down to the bottom-left.
    let dir = normalize(vec2<f32>(-0.8, 1.0));
    let first = dot(vec2<f32>(w.size.x, 0.0), dir);
    let last = dot(vec2<f32>(0.0, w.size.y), dir);
    // Ripples run along the edge.
    let along = dot(px, vec2<f32>(dir.y, -dir.x));
    let wave = sin(along / (90.0 * s) + w.progress * 7.0) * 16.0 * s
        + sin(along / (31.0 * s) - w.progress * 11.0) * 4.0 * s;
    let margin = 160.0 * s;
    let front = mix(first - margin, last + margin, w.progress);
    // > 0: still the old theme; < 0: already the new one.
    let x = dot(px, dir) + wave - front;

    // Old pixels near the edge are dragged along with it.
    let near = exp(-max(x, 0.0) / (34.0 * s));
    let pull = near * 10.0 * s;
    let uv = clamp((px - dir * pull) / w.size, vec2<f32>(0.0), vec2<f32>(1.0));
    let old = textureSampleLevel(snap, samp, uv, 0.0);
    let a = smoothstep(-1.5 * s, 1.5 * s, x);

    // A bright core line with a wide, soft bloom, trailing a little more on the new side.
    let core = exp(-abs(x) / (3.0 * s));
    let bloom = exp(-abs(x) / (select(26.0, 70.0, x < 0.0) * s));
    let glow = (0.9 * core + 0.35 * bloom) * w.color.a;
    // The old page darkens a little toward the edge, as if lifting off.
    let shade = 1.0 - 0.4 * near;
    let rgb = old.rgb * shade * a + w.color.rgb * glow;
    return vec4<f32>(rgb, a);
}
