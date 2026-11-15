// Instanced SDF shapes: rounded boxes, rounded triangles, soft shadows/glows.
// Coordinates are physical pixels, origin top-left. Output is premultiplied alpha.

struct Globals {
    size: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;

struct Instance {
    @location(0) rect: vec4<f32>,          // x, y, w, h
    @location(1) color_top: vec4<f32>,     // linear rgba
    @location(2) color_bottom: vec4<f32>,  // linear rgba (vertical gradient)
    @location(3) border_color: vec4<f32>,
    @location(4) params: vec4<f32>,        // radius, border width, kind, blur
    @location(5) clip: vec4<f32>,          // x0, y0, x1, y1
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,  // position relative to the shape center
    @location(1) half: vec2<f32>,
    @location(2) t: f32,            // 0 at top, 1 at bottom (gradient)
    @location(3) @interpolate(flat) color_top: vec4<f32>,
    @location(4) @interpolate(flat) color_bottom: vec4<f32>,
    @location(5) @interpolate(flat) border_color: vec4<f32>,
    @location(6) @interpolate(flat) params: vec4<f32>,
    @location(7) @interpolate(flat) clip: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    // Grow the quad so anti-aliasing and blur aren't cut off.
    let pad = inst.params.w * 2.0 + 1.0;
    let p = inst.rect.xy - vec2<f32>(pad) + corner * (inst.rect.zw + vec2<f32>(2.0 * pad));
    var out: VOut;
    let ndc = p / g.size * 2.0 - 1.0;
    out.pos = vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
    out.half = inst.rect.zw * 0.5;
    out.local = p - (inst.rect.xy + out.half);
    out.t = clamp((p.y - inst.rect.y) / max(inst.rect.w, 1.0), 0.0, 1.0);
    out.color_top = inst.color_top;
    out.color_bottom = inst.color_bottom;
    out.border_color = inst.border_color;
    out.params = inst.params;
    out.clip = inst.clip;
    return out;
}

fn sd_round_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let rr = min(r, min(half.x, half.y));
    let q = abs(p) - half + vec2<f32>(rr);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

// Distance to a circular arc of radius r from angle a0 to a1 (radians, y down = clockwise).
fn sd_arc(p: vec2<f32>, r: f32, a0: f32, a1: f32) -> f32 {
    let tau = 6.28318530718;
    let span = a1 - a0;
    if (span >= tau) {
        return abs(length(p) - r);
    }
    var t = atan2(p.y, p.x) - a0;
    t = t - floor(t / tau) * tau;
    if (t <= span) {
        return abs(length(p) - r);
    }
    let pa = r * vec2<f32>(cos(a0), sin(a0));
    let pb = r * vec2<f32>(cos(a1), sin(a1));
    return min(length(p - pa), length(p - pb));
}

// Inigo Quilez's exact triangle SDF.
fn sd_triangle(p: vec2<f32>, p0: vec2<f32>, p1: vec2<f32>, p2: vec2<f32>) -> f32 {
    let e0 = p1 - p0;
    let e1 = p2 - p1;
    let e2 = p0 - p2;
    let v0 = p - p0;
    let v1 = p - p1;
    let v2 = p - p2;
    let pq0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    let pq1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    let pq2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    let s = sign(e0.x * e2.y - e0.y * e2.x);
    let d = min(min(
        vec2<f32>(dot(pq0, pq0), s * (v0.x * e0.y - v0.y * e0.x)),
        vec2<f32>(dot(pq1, pq1), s * (v1.x * e1.y - v1.y * e1.x))),
        vec2<f32>(dot(pq2, pq2), s * (v2.x * e2.y - v2.y * e2.x)));
    return -sqrt(d.x) * sign(d.y);
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let frag = in.pos.xy;
    if (frag.x < in.clip.x || frag.y < in.clip.y || frag.x > in.clip.z || frag.y > in.clip.w) {
        discard;
    }
    let radius = in.params.x;
    let border = in.params.y;
    let kind = u32(in.params.z);
    let blur = in.params.w;

    var d: f32;
    switch kind {
        // 1: triangle pointing right, 2: pointing left. Rounded by `radius`.
        case 1u: {
            let h = in.half - vec2<f32>(radius);
            d = sd_triangle(in.local, vec2<f32>(-h.x, -h.y), vec2<f32>(h.x, 0.0), vec2<f32>(-h.x, h.y)) - radius;
        }
        case 2u: {
            let h = in.half - vec2<f32>(radius);
            d = sd_triangle(in.local, vec2<f32>(h.x, -h.y), vec2<f32>(h.x, h.y), vec2<f32>(-h.x, 0.0)) - radius;
        }
        // 3: round-capped line; endpoints (relative to the center) ride in border_color.
        case 3u: {
            d = sd_segment(in.local, in.border_color.xy, in.border_color.zw) - radius;
        }
        // 4: arc stroke; border_color = (start angle, end angle, arc radius, _).
        case 4u: {
            d = sd_arc(in.local, in.border_color.z, in.border_color.x, in.border_color.y) - radius;
        }
        default: {
            d = sd_round_box(in.local, in.half, radius);
        }
    }

    let fill = mix(in.color_top, in.color_bottom, in.t);
    var color: vec4<f32>;
    if (blur > 0.0) {
        // Soft shape: a smooth falloff of `blur` pixels around the edge (glows, shadows).
        let a = 1.0 - smoothstep(-blur, blur, d);
        color = vec4<f32>(fill.rgb, fill.a * a);
    } else {
        let coverage = clamp(0.5 - d, 0.0, 1.0);
        var c = fill;
        if (border > 0.0) {
            let inner = clamp(0.5 - (d + border), 0.0, 1.0);
            c = mix(in.border_color, fill, inner);
        }
        color = vec4<f32>(c.rgb, c.a * coverage);
    }
    return vec4<f32>(color.rgb * color.a, color.a);
}
