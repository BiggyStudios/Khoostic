// The 3D visualizer: a storm of small metal cubes.
//
// `sim` moves every cube once per frame. Cubes start stacked into the 2D visualizer's bars (the
// "formation", in the same place the 2D bars are drawn), break loose from the top down and get
// carried off by a swirling flow field that the music pushes around: bass blasts them outward,
// beats kick, mids spin the storm, each cube flares with its own frequency band. Leaving 3D, they
// fly back into the bars. Two dim cube walls pulse like an equalizer on either side.
//
// `vs_cube` / `fs_cube` draw them as instanced, flat-shaded metal (environment reflection,
// fresnel, specular), fogged with distance; `vs_bg` / `fs_bg` paint the backdrop.

const BARS: u32 = 64u;
/// Cubes that fly (the rest are wall cubes).
const SWARM: u32 = 49152u;
/// The last cubes of the swarm are the big "splash" ones.
const SPARKS: u32 = 4096u;
const WALL_ROWS: u32 = 32u;
const WALL_COLS: u32 = 32u;
/// Each bar is two cubes wide and this many high.
const BAR_ROWS: u32 = 384u;
/// No cube ever gets closer to the camera than this (world units).
const NEAR: f32 = 1.9;

// Where each tuning knob sits in `tune` (the order of `config/visualizer.rs`; a test checks).
const T_CUBES: u32 = 0u;
const T_SPLASHES: u32 = 1u;
const T_CUBE_SIZE: u32 = 2u;
const T_SPLASH_SIZE: u32 = 3u;
const T_LENGTH: u32 = 4u;
const T_WIDTH: u32 = 5u;
const T_THICKNESS: u32 = 6u;
const T_SPLASH_SPREAD: u32 = 7u;
const T_REACH: u32 = 8u;
const T_TWIST: u32 = 9u;
const T_HOLD: u32 = 16u;
const T_DRIFT: u32 = 17u;
const T_BURST: u32 = 18u;
const T_SLACK: u32 = 20u;
const T_BASS_PUNCH: u32 = 22u;
const T_MID_WHIP: u32 = 23u;
const T_PUFF: u32 = 24u;
const T_FLASH: u32 = 25u;
const T_METAL: u32 = 32u;
const T_BAND_GLOW: u32 = 33u;
const T_BACKDROP_GLOW: u32 = 35u;
const T_WALL_BULGE: u32 = 37u;
const T_BAR_GAP: u32 = 38u;
const T_SPECTRUM_PUSH: u32 = 42u;

struct Scene {
    view_proj: mat4x4<f32>,
    // xyz: camera position; w: time (s)
    cam: vec4<f32>,
    // dt, seconds since the current mode started, mode (0 bars, 1 storm, 2 back to bars),
    // how far the camera has moved into 3D (0..1)
    params: vec4<f32>,
    // bass, mid, treble, beat
    audio: vec4<f32>,
    // logical width and height of the view, how visible the walls are, fog
    view: vec4<f32>,
    // where the 2D bars are drawn inside the view (logical x, y, width, height)
    bars: vec4<f32>,
    spectrum: array<vec4<f32>, 4>,
    accent: vec4<f32>,
    surface: vec4<f32>,
    fg: vec4<f32>,
    bands: array<vec4<f32>, 16>,
    // x: music time (runs faster when the music is loud), drives the wave; y: 1 on the frame a
    // beat lands; z: how recently one landed (1, fading); w: bass punch (sudden rises)
    motion: vec4<f32>,
    // x: how far the ribbon has spun (mostly the mids); y: mid punch (sudden rises); z: how
    // full the music is overall (smoothed, 0..1); w: a hit from a sudden sound anywhere in
    // the spectrum this frame (its strength; 0 if none)
    motion2: vec4<f32>,
    // the tuning knobs (config `[visualizer.tuning]`), read through `tune(T_...)`
    tune: array<vec4<f32>, 11>,
    // per band: 1 on a sharp rise for that band's own level, fading (vocals, hats, ...)
    flares: array<vec4<f32>, 16>,
    // x: 1 with no bass, down to 0.3 with a strong bass (how much the rest of the spectrum
    // leads)
    spectral: vec4<f32>,
};

struct Cube {
    // xyz, size (half the edge)
    pos: vec4<f32>,
    // xyz, seconds since it broke loose (< 0: still in a bar)
    vel: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Scene;
@group(0) @binding(1) var<storage, read_write> cubes: array<Cube>;
// The same cubes, for drawing (vertex shaders may only read storage).
@group(0) @binding(2) var<storage, read> drawn: array<Cube>;

fn band(i: u32) -> f32 {
    let b = min(i, BARS - 1u);
    return u.bands[b / 4u][b % 4u];
}

fn tune(i: u32) -> f32 {
    return u.tune[i / 4u][i % 4u];
}

fn flare(i: u32) -> f32 {
    let b = min(i, BARS - 1u);
    return u.flares[b / 4u][b % 4u];
}

fn hash(n: u32) -> f32 {
    var x = n * 747796405u + 2891336453u;
    x = ((x >> ((x >> 28u) + 4u)) ^ x) * 277803737u;
    x = (x >> 22u) ^ x;
    return f32(x) / 4294967295.0;
}

fn hash3(n: u32) -> vec3<f32> {
    return vec3<f32>(hash(n * 3u), hash(n * 3u + 1u), hash(n * 3u + 2u));
}

fn spectrum(t: f32) -> vec3<f32> {
    let p = clamp(t, 0.0, 1.0) * 3.0;
    let i = min(u32(floor(p)), 2u);
    return mix(u.spectrum[i].rgb, u.spectrum[i + 1u].rgb, p - f32(i));
}

// Where cube `i` sits in the bars, in world units (the view is 2 high, centered), and its size
// there (0 above the bar's current height).
fn formation(i: u32) -> vec4<f32> {
    let w = u.bars.z;
    let h = u.bars.w;
    let bar = i % BARS;
    let k = i / BARS;
    let col = k % 2u;
    let row = k / 2u;
    // The 2D visualizer's layout, in logical pixels.
    let gap = tune(T_BAR_GAP);
    let bar_w = max((w - gap * f32(BARS - 1u)) / f32(BARS), 1.0);
    let base = h - 24.0;
    let max_h = h - 48.0;
    let step_y = max_h / f32(BAR_ROWS);
    let x = u.bars.x + f32(bar) * (bar_w + gap) + (f32(col) + 0.5) * bar_w * 0.5;
    let y = u.bars.y + base - (f32(row) + 0.5) * step_y;
    // World units: the whole view is 2 high, centered.
    let unit = u.view.y * 0.5;
    let p = vec3<f32>((x - u.view.x * 0.5) / unit, (u.view.y * 0.5 - y) / unit, 0.0);
    let height = max(band(bar) * max_h, bar_w);
    let shown = select(0.0, 1.0, f32(row) * step_y < height);
    let size = 0.5 * min(bar_w * 0.5, step_y) * 0.9 / unit;
    return vec4<f32>(p, size * shown);
}

// The top of cube `i`'s bar, in world units.
fn bar_top(i: u32) -> f32 {
    let h = u.bars.w;
    let bar_w = max((u.bars.z - tune(T_BAR_GAP) * f32(BARS - 1u)) / f32(BARS), 1.0);
    let top = u.bars.y + h - 24.0 - max(band(i % BARS) * (h - 48.0), bar_w);
    return (u.view.y * 0.5 - top) / (u.view.y * 0.5);
}

// A swirling, divergence-free flow (the curl of a few sine waves), for the storm.
fn potential(p: vec3<f32>, t: f32) -> vec3<f32> {
    let a = vec3<f32>(
        sin(p.y * 1.7 + t * 0.31) * cos(p.z * 1.3 - t * 0.23),
        sin(p.z * 1.5 - t * 0.27) * cos(p.x * 1.9 + t * 0.19),
        sin(p.x * 1.6 + t * 0.21) * cos(p.y * 1.4 + t * 0.29),
    );
    let b = vec3<f32>(
        sin(p.z * 3.7 - t * 0.6) * cos(p.y * 3.1),
        sin(p.x * 3.3 + t * 0.5) * cos(p.z * 3.9),
        sin(p.y * 3.5 - t * 0.7) * cos(p.x * 3.2),
    );
    return a + 0.35 * b;
}

fn curl(p: vec3<f32>, t: f32) -> vec3<f32> {
    let e = 0.05;
    let dx = vec3<f32>(e, 0.0, 0.0);
    let dy = vec3<f32>(0.0, e, 0.0);
    let dz = vec3<f32>(0.0, 0.0, e);
    let px = (potential(p + dx, t) - potential(p - dx, t)) / (2.0 * e);
    let py = (potential(p + dy, t) - potential(p - dy, t)) / (2.0 * e);
    let pz = (potential(p + dz, t) - potential(p - dz, t)) / (2.0 * e);
    return vec3<f32>(py.z - pz.y, pz.x - px.z, px.y - py.x);
}

/// `v` without its part along `axis` (a unit vector).
fn flatten(v: vec3<f32>, axis: vec3<f32>) -> vec3<f32> {
    return v - axis * dot(v, axis);
}

fn safe_normalize(v: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    return select(vec3<f32>(1.0, 0.0, 0.0), v / l, l > 1e-5);
}

// Where the wave's lead point is at (music) time `tau`: a loop of swings that never quite
// repeats, kept in front of the camera.
fn path(tau: f32) -> vec3<f32> {
    return vec3<f32>(
        sin(tau * 0.6) * 0.95 + sin(tau * 1.37) * 0.25,
        sin(tau * 0.83 + 1.0) * 0.6 + cos(tau * 1.9) * 0.15,
        cos(tau * 0.47) * 0.6,
    );
}

fn wall(i: u32) -> vec4<f32> {
    let j = i - SWARM;
    let side = select(-1.0, 1.0, j >= WALL_ROWS * WALL_COLS);
    let k = j % (WALL_ROWS * WALL_COLS);
    let row = k / WALL_COLS;
    let col = k % WALL_COLS;
    // Each row is a frequency: it bulges toward the middle as that band gets loud.
    let level = band(row * 2u);
    let jitter = hash(i) * 0.05;
    let push = level * tune(T_WALL_BULGE) * u.view.z;
    let x = side * (2.55 + jitter + (1.0 - u.view.z) * 1.5 - push);
    let y = -1.55 + f32(row) * 0.1;
    let z = 2.4 - f32(col) * 0.19;
    return vec4<f32>(x, y, z, 0.043 * u.view.z);
}

@compute @workgroup_size(256)
fn sim(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= arrayLength(&cubes) {
        return;
    }
    if i >= SWARM {
        cubes[i].pos = wall(i);
        cubes[i].vel = vec4<f32>(0.0);
        return;
    }
    let dt = u.params.x;
    let phase = u.params.y;
    let mode = u.params.z;
    let t = u.cam.w;
    let f = formation(i);
    let spark = i >= SWARM - SPARKS;
    let seed = hash3(i);
    var c = cubes[i];

    // Top of the bars first, with some scatter.
    let row = f32((i / BARS) / 2u) / f32(BAR_ROWS);
    let delay = 0.08 + (1.0 - row) * 0.55 + seed.x * 0.45;

    if mode < 0.5 || (mode < 1.5 && phase < delay) {
        // In the bars.
        cubes[i].pos = f;
        cubes[i].vel = vec4<f32>(0.0, 0.0, 0.0, -1.0);
        return;
    }

    let bass = u.audio.x;
    let mid = u.audio.y;
    let treble = u.audio.z;
    let beat = u.audio.w;
    let mine = band(i % BARS);
    var v = c.vel.xyz;
    var p = c.pos.xyz;

    if mode > 1.5 {
        // Back into the bars: springs that tighten over time, scattered start.
        let pull = smoothstep(0.0, 1.2, phase - (1.0 - row) * 0.35 - seed.x * 0.3);
        let to = f.xyz - p;
        v = mix(v * (1.0 - min(dt * 1.5, 1.0)), to * 9.0, pull * min(dt * 10.0, 1.0));
        p += v * dt;
        if pull > 0.999 {
            p = f.xyz;
            v = vec3<f32>(0.0);
        }
        let size = mix(c.pos.w, f.w, min(dt * 4.0 * (0.3 + pull), 1.0));
        cubes[i].pos = vec4<f32>(p, size);
        cubes[i].vel = vec4<f32>(v, -1.0);
        return;
    }

    var age = c.vel.w;
    if age < 0.0 {
        // Just broke loose: a kick up and to the side, out of the bar (the wave then picks
        // it up).
        age = 0.0;
        if c.pos.w <= 0.0 {
            // Not part of the bar right now: start from its top instead of thin air.
            p.y = min(p.y, bar_top(i));
        }
        let out = vec3<f32>(seed.x - 0.5, 0.4 + seed.y * 0.6, 0.0);
        v = safe_normalize(out) * (0.5 + seed.y * 1.2);
    }
    age += dt;

    // The wave: a lead point swinging along a looping path; every cube follows the path some
    // time behind it, spread around it in a tube that widens toward the tail. The splashes are
    // a looser spray around the same wave. Nothing comes out of a single point.
    let tau = u.motion.x;
    let lag = seed.x * tune(T_LENGTH);
    // The wave roams wider when the music is full.
    let reach = (0.8 + 0.5 * u.motion2.z) * tune(T_REACH);
    let here = path(tau - lag) * reach;
    let tangent = safe_normalize(path(tau - lag + 0.05) * reach - here);
    let n1 = safe_normalize(cross(tangent, vec3<f32>(0.0, 1.0, 0.3)));
    let n2 = cross(tangent, n1);
    // A ribbon: wide one way, thin the other, twisting along its length.
    // Spinning around its own length, mostly with the mids.
    let twist = lag * 1.3 * tune(T_TWIST) + u.motion2.x;
    let wide = n1 * cos(twist) + n2 * sin(twist);
    let thin = cross(tangent, wide);
    let across = (seed.y * 2.0 - 1.0) * (0.12 + 0.12 * lag) * tune(T_WIDTH);
    let through = (seed.z * 2.0 - 1.0) * (0.02 + 0.03 * lag) * tune(T_THICKNESS);
    let puff = (1.0 + (bass * 0.3 + beat * 0.25) * tune(T_PUFF))
        * select(1.0, tune(T_SPLASH_SPREAD), spark);
    let target_pos = here + (wide * across + thin * through) * puff;

    let kick = u.motion.y;
    let impact = u.motion.z;
    let punch = u.motion.w;
    // Out from the wave's spine, for blasts.
    let blast = safe_normalize(flatten(p - here, tangent) + (seed - 0.5) * 0.3);
    // Pulled along to its place in the wave (the splashes loosely). Right after a hit the hold
    // goes slack, so the blast carries before the wave reels it back in.
    let slack = 1.0 - tune(T_SLACK) * impact;
    let hold = tune(T_HOLD);
    v += (target_pos - p) * select(1.0, 0.45, spark) * hold * slack * dt;
    v *= 1.0 - min(dt * select(4.5, 2.2, spark) * sqrt(hold / 10.0) * slack, 1.0);
    // A beat hits: every cube gets a burst of speed at once, out from the spine and forward
    // along the wave, as hard as the beat (an ordinary one is 0.7), the bass and this cube's
    // band.
    if kick > 0.0 {
        let hit = kick / 0.7;
        let force = (0.6 + 1.2 * seed.y) * (0.6 + bass + 0.6 * mine) * select(1.0, 1.6, spark);
        v += (blast * force + tangent * (1.2 + bass * 1.5)) * tune(T_BURST) * hit;
    }
    // Sudden bass (not just loud bass) keeps blowing the ribbon outward.
    v += blast * punch * select(6.0, 10.0, spark) * tune(T_BASS_PUNCH) * dt;
    // Sudden mids whip the cubes around the spine.
    let mid_punch = u.motion2.y;
    v += (cross(tangent, blast) * 9.0 + blast * 2.0) * mid_punch * select(1.0, 1.5, spark) * tune(T_MID_WHIP) * dt;
    // Every cube belongs to a frequency band: when that band flares (a syllable, a hi-hat, a
    // synth stab) its cubes get thrown out and spun around the spine, bass or no bass.
    let my_flare = flare(i % BARS);
    v += (blast * select(3.5, 6.0, spark) + cross(tangent, blast) * 2.5)
        * my_flare * tune(T_SPECTRUM_PUSH) * u.spectral.x * dt;
    // A sudden sound anywhere in the spectrum hits like a softer beat.
    let onset = u.motion2.w;
    if onset > 0.0 {
        let force = (0.3 + 0.6 * seed.y) * (0.5 + 1.5 * my_flare) * select(1.0, 1.4, spark);
        v += (blast * force + tangent * 0.8) * onset;
    }
    // Sand blowing about in the wave; harder when this cube's band is loud.
    let flow = curl(p * 0.9 + seed * 0.2, t * (0.6 + mid * 0.8));
    v += flow * (0.06 + 0.3 * mine + 0.15 * treble) * select(3.0, 5.0, spark) * tune(T_DRIFT) * dt;
    p += v * dt;
    // A safety net only: never closer to the camera than NEAR.
    let from_cam = p - u.cam.xyz;
    if length(from_cam) < NEAR {
        let away = safe_normalize(from_cam);
        p = u.cam.xyz + away * NEAR;
        v -= away * min(dot(v, away), 0.0);
    }
    // Fine sand, with a few big chunks among the splashes.
    let target_size = select(
        (0.0035 + seed.z * 0.008) * tune(T_CUBE_SIZE),
        (0.006 + 0.026 * pow(seed.z, 9.0)) * tune(T_SPLASH_SIZE),
        spark,
    );
    let size = mix(c.pos.w, target_size, min(dt * 2.5, 1.0));
    cubes[i].pos = vec4<f32>(p, size);
    cubes[i].vel = vec4<f32>(v, age);
}

// ---- Drawing --------------------------------------------------------------------------------

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) glow: f32,
    @location(4) metal: f32,
};

fn rotate(v: vec3<f32>, axis: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

@vertex
fn vs_cube(@builtin(vertex_index) vi: u32, @builtin(instance_index) i: u32) -> VsOut {
    // 6 faces x 2 triangles.
    let face = vi / 6u;
    let corner = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    )[vi % 6u];
    let axis = face / 2u;
    let sign = select(-1.0, 1.0, face % 2u == 0u);
    var n = vec3<f32>(0.0);
    var local = vec3<f32>(0.0);
    if axis == 0u {
        n = vec3<f32>(sign, 0.0, 0.0);
        local = vec3<f32>(sign, corner.x * sign, corner.y);
    } else if axis == 1u {
        n = vec3<f32>(0.0, sign, 0.0);
        local = vec3<f32>(corner.y, sign, corner.x * sign);
    } else {
        n = vec3<f32>(0.0, 0.0, sign);
        local = vec3<f32>(corner.x * sign, corner.y, sign);
    }

    let c = drawn[i];
    let t = u.cam.w;
    let seed = hash3(i);
    let age = c.vel.w;
    let wall_cube = i >= SWARM;
    let spark = i >= SWARM - SPARKS && !wall_cube;
    let loose = smoothstep(0.0, 0.9, age);
    // Tumble once loose; the faster, the more.
    let spin_axis = normalize(seed - 0.5 + vec3<f32>(0.0, 0.0, 1e-3));
    let spin = (t * (0.6 + seed.y * 2.0) + length(c.vel.xyz) * 0.8 + seed.z * 6.28) * loose;
    var lp = rotate(local, spin_axis, spin);
    var ln = rotate(n, spin_axis, spin);
    if wall_cube {
        // A little crooked, like rough stone.
        lp = rotate(local, spin_axis, seed.x * 0.5);
        ln = rotate(n, spin_axis, seed.x * 0.5);
    }
    // A backstop: anything that still ends up near the camera shrinks away.
    let near = smoothstep(NEAR * 0.75, NEAR + 0.2, length(c.pos.xyz - u.cam.xyz));
    // Thinned out by the Cubes / Splashes knobs (only once loose; the bars stay whole).
    let keep = select(tune(T_CUBES), tune(T_SPLASHES), spark);
    let thinned = select(1.0, 1.0 - loose, hash(i * 7u + 3u) >= keep);
    let size = select(c.pos.w * near * thinned, c.pos.w, wall_cube || age < 0.0);
    let world = c.pos.xyz + lp * size;

    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    out.normal = ln;
    let b = i % BARS;
    let mine = band(b);
    // In the bars: the 2D visualizer's colors by height. Loose: the low end of the theme's
    // spectrum for the storm, the high end for the splashes.
    let row = f32((i / BARS) / 2u) / f32(BAR_ROWS);
    let bar_color = spectrum(row);
    let storm = select(spectrum(seed.x * 0.38), spectrum(0.78 + seed.x * 0.22), spark);
    out.color = mix(bar_color, storm, loose);
    out.glow = mix(0.9, 0.02 + mine * mine * mine * select(0.45, 0.7, spark) * tune(T_BAND_GLOW)
        + flare(b) * 0.5 * tune(T_BAND_GLOW)
        + u.motion.z * 0.25 * tune(T_FLASH), loose);
    out.metal = 1.0;
    if wall_cube {
        let level = band(((i - SWARM) % (WALL_ROWS * WALL_COLS)) / WALL_COLS * 2u);
        out.color = mix(u.surface.rgb * 2.2, spectrum(0.3), level * 0.25);
        out.glow = level * level * 0.12;
        out.metal = 0.6;
    }
    return out;
}

// What metal reflects: a dim room, lit from above, with the theme's colors in the haze.
fn environment(r: vec3<f32>) -> vec3<f32> {
    // A dark studio: a big white softbox above, a strip light in the theme's high color
    // behind, a little of its low color bouncing up from the floor.
    let base = u.surface.rgb * 0.3;
    let box_light = pow(max(dot(r, normalize(vec3<f32>(0.3, 1.0, 0.5))), 0.0), 24.0) * 2.4 * u.fg.rgb;
    let strip = pow(max(dot(r, normalize(vec3<f32>(-0.8, 0.1, -0.6))), 0.0), 10.0) * 1.3 * spectrum(0.9);
    let bounce = spectrum(0.2) * 0.3 * smoothstep(0.0, -0.8, r.y);
    return base + box_light + strip + bounce;
}

@fragment
fn fs_cube(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let to_cam = u.cam.xyz - in.world;
    let dist = length(to_cam);
    let v = to_cam / dist;
    let key = normalize(vec3<f32>(0.4, 1.0, 0.6));
    let back = normalize(vec3<f32>(-0.5, 0.2, -1.0));
    let h = normalize(key + v);
    let ndv = max(dot(n, v), 0.0);
    // Metal: the color tints the reflection; edges catch more of it.
    let f0 = mix(vec3<f32>(0.04), in.color, in.metal);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - ndv, 5.0);
    let reflection = environment(reflect(-v, n)) * fresnel * tune(T_METAL);
    let diffuse = in.color * (0.03 + 0.2 * max(dot(n, key), 0.0)) * (1.0 - in.metal * 0.7);
    let spec = pow(max(dot(n, h), 0.0), 120.0) * 1.8 * mix(vec3<f32>(1.0), in.color, 0.35);
    let rim = u.accent.rgb * pow(max(dot(n, back), 0.0), 2.0) * 0.25;
    var color = diffuse + reflection + spec + rim + in.color * in.glow;
    // Fog into the backdrop with distance (only once in 3D).
    let fog = 1.0 - exp(-max(dist - 2.0, 0.0) * u.view.w);
    color = mix(color, u.surface.rgb * 0.55, clamp(fog, 0.0, 0.95));
    return vec4<f32>(color, 1.0);
}

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_bg(@builtin(vertex_index) i: u32) -> BgOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: BgOut;
    out.clip = vec4<f32>(uv * 2.0 - 1.0, 1.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

@fragment
fn fs_bg(in: BgOut) -> @location(0) vec4<f32> {
    let into_3d = u.params.w;
    let aspect = u.view.x / max(u.view.y, 1.0);
    let p = (in.uv - 0.5) * vec2<f32>(aspect, 1.0);
    let d = length(p);
    // The panel color, darkening at the edges as the room opens up, with a glow behind the
    // storm that breathes with the bass.
    let vignette = mix(1.0, (1.0 - smoothstep(0.2, 1.1, d) * 0.6) * 0.6, into_3d);
    let glow = spectrum(0.35) * exp(-d * d * 4.0) * (0.015 + 0.05 * u.audio.x + 0.12 * u.motion.z * tune(T_FLASH))
        * (0.4 + 0.6 * into_3d) * tune(T_BACKDROP_GLOW);
    return vec4<f32>(u.surface.rgb * vignette + glow, 1.0);
}
