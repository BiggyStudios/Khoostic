// Textured quads with rounded-corner SDF masks (cover art, avatars).

struct Globals {
    size: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) params: vec4<f32>,  // radius, opacity, -, -
    @location(2) clip: vec4<f32>,
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) @interpolate(flat) half: vec2<f32>,
    @location(3) @interpolate(flat) params: vec4<f32>,
    @location(4) @interpolate(flat) clip: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let p = inst.rect.xy + corner * inst.rect.zw;
    var out: VOut;
    let ndc = p / g.size * 2.0 - 1.0;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.uv = corner;
    out.half = inst.rect.zw * 0.5;
    out.local = p - (inst.rect.xy + out.half);
    out.params = inst.params;
    out.clip = inst.clip;
    return out;
}

fn sd_round_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let rr = min(r, min(half.x, half.y));
    let q = abs(p) - half + vec2<f32>(rr);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let frag = in.pos.xy;
    if (frag.x < in.clip.x || frag.y < in.clip.y || frag.x > in.clip.z || frag.y > in.clip.w) {
        discard;
    }
    let d = sd_round_box(in.local, in.half, in.params.x);
    let coverage = clamp(0.5 - d, 0.0, 1.0) * in.params.y;
    let c = textureSample(tex, samp, in.uv);
    let a = c.a * coverage;
    return vec4<f32>(c.rgb * a, a);
}
