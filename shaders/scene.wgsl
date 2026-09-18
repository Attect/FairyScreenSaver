// ============================================================================
//  Fairy big-eye screen saver — single-pass procedural scene shader
//
//  The reference implementation (Fairy-DSH / dsh-fairy-visual) draws the eye as
//  SVG + CSS.  Everything in it is analytic (circles, rounded rects, radial /
//  linear gradients, a 4x4 scanline pattern, 229 horizontal halo strokes and a
//  px-shader noise displacement), so the whole composition is reproduced here
//  per-pixel: no texture, no vertex buffer, no blend state.
//
//  Paint order (matching DOM/stacking order of the reference):
//      background substrate  ->  grid  ->  glow a  ->  glow b  ->  glow c
//      .dsh-fairy-halo-layer   (horizontal light filaments, opacity .92)
//      .dsh-fairy-pulse-layer  (expanding ring,            opacity .92)
//      .dsh-fairy-outer-halo   (r = 79 soft cyan halo)
//      .dsh-fairy-signal       (translateX + skewX + brightness/contrast)
//          .dsh-fairy-image    (disc / lashes / sclera / iris / highlight)
//          5 clipped clones    (blocks glitch only)
// ============================================================================

// ---------------------------------------------------------------- uniforms --

struct Globals {
    resolution: vec2<f32>,      // 0    framebuffer size, px
    time: f32,                  // 8    seconds since start
    eyeScalePx: f32,            // 12   px per eye unit (viewBox unit)
    viewOrigin: vec2<f32>,      // 16   top-left of the 160x160 eye space, px
    lashAngle: f32,             // 24   rotate(360deg/15s), radians
    lidCenterY: f32,            // 28   upper-lid crossing height, eye units (>=100 = open)
    lidCurve: f32,              // 32   quadratic bow depth, eye units
    scleraScale: f32,           // 36   breathing scales
    l3Scale: f32,               // 40
    l2Scale: f32,               // 44
    l1Scale: f32,               // 48
    flicker: f32,               // 52   full-eye flicker opacity
    gaze: vec2<f32>,            // 56   iris offset, eye units
    pulsePhase: f32,            // 64   0..1 in pulse cycle, <0 = inactive
    pulseCycle: f32,            // 68   pulse cycle length, s
    glitchMode: f32,            // 72   0 none | 1 threads | 2 blocks
    glitchAmp: f32,             // 76   thread displacement scale, eye units
    bright: f32,                // 80   signal filter
    contrast: f32,              // 84
    skew: f32,                  // 88   radians
    gx: f32,                    // 92   translateX, eye units
    sliceOffsets: vec4<f32>,    // 96   block clone x offsets 1..4
    sliceOffset5: f32,          // 112  block clone x offset 5
    glitchSeed: f32,            // 116
    // NOTE: kept as four scalars rather than a vec4 on purpose.  A vec4 would
    // force 16-byte alignment (offset 128) and silently shift every following
    // member, which is exactly the kind of mismatch that makes the eye vanish.
    sliceEdge1: f32,            // 120  block y edges 1..4 (edge0 = 12, edge5 = 148)
    sliceEdge2: f32,            // 124
    sliceEdge3: f32,            // 128
    sliceEdge4: f32,            // 132
    eyeOpacity: f32,            // 136
    _spare0: f32,               // 140
};

@group(0) @binding(0) var<uniform> G: Globals;

// ------------------------------------------------------------------ utils --

fn seg(t: f32, a: f32, b: f32) -> f32 {
    return clamp((t - a) / (b - a), 0.0, 1.0);
}

// Premultiplied source-over.
fn over(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    return src + dst * (1.0 - src.a);
}

fn premul(c: vec3<f32>, a: f32) -> vec4<f32> {
    return vec4<f32>(c * a, a);
}

fn rot2(a: f32) -> mat2x2<f32> {
    let s = sin(a);
    let c = cos(a);
    return mat2x2<f32>(c, s, -s, c);
}

// Signed distance to a circle of radius r centred on the origin.
fn sdCircle(p: vec2<f32>, r: f32) -> f32 {
    return length(p) - r;
}

// SVG rounded rect centred on `p`'s origin, half extents `b`, corner radius `r`.
fn sdRoundRect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

// Coverage from a signed distance, `px` = size of one pixel in the same units.
fn cov(d: f32, px: f32) -> f32 {
    return clamp(0.5 - d / px, 0.0, 1.0);
}

fn hashf(a: u32, b: u32, c: u32) -> f32 {
    var h: u32 = a * 0x9E3779B1u ^ b * 0x85EBCA77u ^ c * 0xC2B2AE3Du;
    h ^= h >> 15u;
    h *= 0x2545F491u;
    h ^= h >> 13u;
    h *= 0x27D4EB2Fu;
    h ^= h >> 16u;
    return f32(h >> 8u) / 16777216.0;
}

// -------------------------------------------------------------------------- //
//  Background substrate
// -------------------------------------------------------------------------- //

// #07101c
const BG_BASE: vec3<f32> = vec3<f32>(0.027451, 0.062745, 0.109804);

fn glowPm(
    p: vec2<f32>,
    ctr: vec2<f32>,
    rad: vec2<f32>,
    c0: vec3<f32>,
    a0: f32,
    c1: vec3<f32>,
    a1: f32,
    stop: f32,
) -> vec4<f32> {
    let t = length((p - ctr) / rad);
    let s1 = clamp(t / stop, 0.0, 1.0);
    let s2 = clamp((t - stop) / (1.0 - stop), 0.0, 1.0);
    return vec4<f32>(
        mix(mix(c0 * a0, c1 * a1, s1), vec3<f32>(0.0, 0.0, 0.0), s2),
        mix(mix(a0, a1, s1), 0.0, s2),
    );
}

// Antialiased 1.5px grid line coverage for a 48px cell.
fn gridCov(x: f32, px: f32) -> f32 {
    let cell = 48.0;
    let m = x - cell * floor(x / cell);
    return clamp(0.5 + min(m, 1.5 - m) / px, 0.0, 1.0);
}

fn background(p: vec2<f32>) -> vec4<f32> {
    let w = G.resolution.x;
    let h = G.resolution.y;
    // CSS  /  are *one percent* of the viewport's larger edge / height,
    // not the full edge — the whole glow geometry depends on that distinction.
    let vmax = max(w, h) * 0.01;
    let vh = h * 0.01;

    var col = vec4<f32>(BG_BASE, 1.0);

    // --- 48px cyan grid, 1.5px lines, effective alpha 0.042 * 0.7 ---
    let gcov = max(gridCov(p.x, 1.0), gridCov(p.y, 1.0));
    col = over(premul(vec3<f32>(0.392157, 0.823529, 1.0), gcov * 0.0294), col);

    // --- three oversized radial glows (dark theme values) ---
    col = over(
        glowPm(
            p,
            vec2<f32>(w - 4.26 * vmax, 12.8 * vh),
            vec2<f32>(56.0 * vmax, 29.0 * vh),
            vec3<f32>(0.376471, 0.831373, 1.0),
            0.27,
            vec3<f32>(0.211765, 0.721569, 1.0),
            0.135,
            0.52,
        ),
        col,
    );
    col = over(
        glowPm(
            p,
            vec2<f32>(w - 7.15 * vmax, 73.2 * vh),
            vec2<f32>(53.0 * vmax, 37.0 * vh),
            vec3<f32>(0.615686, 0.517647, 1.0),
            0.23,
            vec3<f32>(0.466667, 0.356863, 1.0),
            0.115,
            0.53,
        ),
        col,
    );
    col = over(
        glowPm(
            p,
            vec2<f32>(5.4 * vmax, 17.65 * vh),
            vec2<f32>(50.0 * vmax, 26.0 * vh),
            vec3<f32>(0.682353, 0.439216, 1.0),
            0.24,
            vec3<f32>(0.560784, 0.368627, 1.0),
            0.12,
            0.53,
        ),
        col,
    );
    return col;
}

// -------------------------------------------------------------------------- //
//  Radial profiles (SVG gradient stop tables, linear interpolation)
// -------------------------------------------------------------------------- //

// #dsh-fairy-outer-halo-gradient, r = 79, colour #c9f8ff
fn profileOuterHalo(t: f32) -> f32 {
    var v = 0.0;
    v = mix(v, 0.10, seg(t, 0.80, 0.84));
    v = mix(v, 0.28, seg(t, 0.84, 0.868));
    v = mix(v, 0.18, seg(t, 0.868, 0.90));
    v = mix(v, 0.06, seg(t, 0.90, 0.95));
    v = mix(v, 0.02, seg(t, 0.95, 0.99));
    v = mix(v, 0.0, seg(t, 0.99, 1.0));
    return v;
}

// #dsh-fairy-sclera-halo-gradient, r = 56, colour #ffffff
fn profileScleraHalo(t: f32) -> f32 {
    var v = 0.0;
    v = mix(v, 0.22, seg(t, 0.82, 0.864));
    v = mix(v, 0.28, seg(t, 0.864, 0.875));
    v = mix(v, 0.19, seg(t, 0.875, 0.89));
    v = mix(v, 0.11, seg(t, 0.89, 0.91));
    v = mix(v, 0.07, seg(t, 0.91, 0.94));
    v = mix(v, 0.035, seg(t, 0.94, 0.96));
    v = mix(v, 0.0, seg(t, 0.96, 1.0));
    return v;
}

// #dsh-fairy-highlight-halo-gradient, r = 18, colour #f5f8fd
fn profileHighlightHalo(t: f32) -> f32 {
    var v = 0.0;
    v = mix(v, 0.16, seg(t, 0.53, 0.56));
    v = mix(v, 0.43, seg(t, 0.56, 0.611));
    v = mix(v, 0.19, seg(t, 0.611, 0.72));
    v = mix(v, 0.12, seg(t, 0.72, 0.77));
    v = mix(v, 0.055, seg(t, 0.77, 0.83));
    v = mix(v, 0.02, seg(t, 0.83, 0.89));
    v = mix(v, 0.004, seg(t, 0.89, 0.96));
    v = mix(v, 0.0, seg(t, 0.96, 1.0));
    return v;
}

// #dsh-fairy-halo-layer-fade — 25-stop luminance mask of the filament layer.
fn haloMask(t: f32) -> f32 {
    if (t <= 0.44) {
        return 0.0;
    }
    var v = 0.0;
    v = mix(v, 0.08, seg(t, 0.44, 0.45));
    v = mix(v, 0.30, seg(t, 0.45, 0.46));
    v = mix(v, 0.65, seg(t, 0.46, 0.47));
    v = mix(v, 1.0, seg(t, 0.47, 0.48));
    v = mix(v, 0.72, seg(t, 0.48, 0.52));
    v = mix(v, 0.56, seg(t, 0.52, 0.56));
    v = mix(v, 0.44, seg(t, 0.56, 0.60));
    v = mix(v, 0.30, seg(t, 0.60, 0.65));
    v = mix(v, 0.19, seg(t, 0.65, 0.71));
    v = mix(v, 0.135, seg(t, 0.71, 0.77));
    v = mix(v, 0.09, seg(t, 0.77, 0.83));
    v = mix(v, 0.058, seg(t, 0.83, 0.89));
    v = mix(v, 0.052, seg(t, 0.89, 0.90));
    v = mix(v, 0.046, seg(t, 0.90, 0.91));
    v = mix(v, 0.039, seg(t, 0.91, 0.92));
    v = mix(v, 0.032, seg(t, 0.92, 0.93));
    v = mix(v, 0.025, seg(t, 0.93, 0.94));
    v = mix(v, 0.019, seg(t, 0.94, 0.95));
    v = mix(v, 0.013, seg(t, 0.95, 0.96));
    v = mix(v, 0.0075, seg(t, 0.96, 0.97));
    v = mix(v, 0.0035, seg(t, 0.97, 0.98));
    v = mix(v, 0.001, seg(t, 0.98, 0.99));
    v = mix(v, 0.0, seg(t, 0.99, 1.0));
    return v;
}

// -------------------------------------------------------------------------- //
//  Layer 1 — horizontal light filaments (buildHaloLines in the reference)
// -------------------------------------------------------------------------- //

fn haloFilaments(p: vec2<f32>, px: f32) -> vec4<f32> {
    // Strokes are 0.62 tall and centred on y = k + 0.25, for k in [-34, 194].
    let k = floor(p.y + 0.06);
    if ((k < -34.0) || (k > 194.0)) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let covY = cov(abs(p.y - (k + 0.25)) - 0.31, px);
    if (covY <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let t = k + 34.0;
    let lw = (sin(t * 0.082 - 1.1) + 0.55 * sin(t * 0.151 + 2.4) + 1.55) / 3.1;
    let rw = (sin(t * 0.097 + 2.2) + 0.5 * sin(t * 0.137 - 1.7) + 1.5) / 3.0;
    let left = 80.0 - (158.0 + lw * 56.0);
    let right = 80.0 + (158.0 + rw * 56.0);
    if ((p.x < left) || (p.x > right)) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let brightness = 0.50 + ((lw + rw) * 0.5) * 0.50;
    let bucket = clamp(round(brightness * 8.0) / 8.0, 0.5, 1.0);
    // Edge antialiasing across the stroke ends.
    let covX = clamp(
        0.5 + min(p.x - left, right - p.x) / px,
        0.0,
        1.0,
    );
    let m = haloMask(length(p - vec2<f32>(80.0, 80.0)) / 124.0);
    return premul(vec3<f32>(0.788235, 0.937255, 1.0), bucket * m * covY * covX * 0.99);
}

// -------------------------------------------------------------------------- //
//  Layer 2 — expanding lash pulse ring
// -------------------------------------------------------------------------- //

fn ringCov(d: f32, r: f32, w: f32, px: f32) -> f32 {
    return clamp(0.5 - (abs(d - r) - w * 0.5) / px, 0.0, 1.0);
}

fn pulseRing(p: vec2<f32>, px: f32) -> vec4<f32> {
    if (G.pulsePhase < 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let ph = G.pulsePhase;
    // transform: scale(.98) -> scale(2.78) over the first 36% of the cycle
    let sc = mix(0.98, 2.78, clamp(ph / 0.36, 0.0, 1.0));
    var op = 0.0;
    op = mix(op, 0.72, seg(ph, 0.0, 0.05));
    op = mix(op, 0.52, seg(ph, 0.05, 0.12));
    op = mix(op, 0.19, seg(ph, 0.12, 0.20));
    op = mix(op, 0.0, seg(ph, 0.20, 0.27));
    if (op <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let d = length(p - vec2<f32>(80.0, 80.0));
    let r = 52.0 * sc;
    var a = 0.0;
    a = a + ringCov(d, r, 20.0 * sc, px) * 0.05;
    a = a + ringCov(d, r, 16.0 * sc, px) * 0.06;
    a = a + ringCov(d, r, 12.0 * sc, px) * 0.08;
    a = a + ringCov(d, r, 8.0 * sc, px) * 0.10;
    a = a + ringCov(d, r, 4.5 * sc, px) * 0.09;
    return premul(vec3<f32>(0.956863, 0.992157, 1.0), min(a, 1.0) * op);
}

// -------------------------------------------------------------------------- //
//  Layer 3 — the eye image (#dsh-fairy-image)
// -------------------------------------------------------------------------- //

fn evalImage(p: vec2<f32>, lash: f32, px: f32) -> vec4<f32> {
    let c = vec2<f32>(80.0, 80.0);
    let d = length(p - c);
    var col = vec4<f32>(0.0, 0.0, 0.0, 0.0);

    // ---- outer disc r=68, linear gradient from (32,0) to (128,160) ----
    let tg = clamp(
        dot(p - vec2<f32>(32.0, 0.0), vec2<f32>(0.6, 1.0)) / 1.36,
        0.0,
        1.0,
    );
    var dc = mix(
        vec3<f32>(0.250980, 0.325490, 0.941176), // #4053f0
        vec3<f32>(0.188235, 0.270588, 0.862745), // #3045dc @ .54
        seg(tg, 0.0, 0.54),
    );
    dc = mix(dc, vec3<f32>(0.239216, 0.313725, 0.784314), seg(tg, 0.54, 1.0)); // #3d50c8
    col = over(premul(dc, cov(d - 68.0, px)), col);
    // thin white rim, stroke-width 1.2
    col = over(premul(vec3<f32>(0.949020, 0.984314, 1.0), cov(abs(d - 68.0) - 0.6, px)), col);

    // ---- rotating four-corner lashes: circle(51.75) ∪ roundedRect(86x86, r=2, +3deg) ----
    let q = rot2(-lash) * (p - c);
    let qr = rot2(-0.0523599) * q; // the rect carries its own rotate(3 80 80)
    let sd = min(sdCircle(q, 51.75), sdRoundRect(qr, vec2<f32>(43.0, 43.0), 2.0));
    col = over(
        premul(vec3<f32>(0.168627, 0.200000, 0.533333), cov(sd, px) * cov(d - 67.0, px)),
        col,
    );

    // ---- eye group: scale(.90) about (80,80) + upper-lid clip ----
    let pe = c + (p - c) / 0.90;
    var lid = 1.0;
    if (G.lidCenterY < 100.0) {
        // clip path "M20 e Q80 (e+2h) 140 e V160 H20 Z" union rect y>=108
        let s = clamp((pe.x - 20.0) / 120.0, 0.0, 1.0);
        let ly = G.lidCenterY + 4.0 * G.lidCurve * s * (1.0 - s);
        lid = max(cov(ly - pe.y, px), cov(108.0 - pe.y, px));
    }

    // ---- sclera group (own breathing scale) ----
    let ps = c + (pe - c) / G.scleraScale;
    let ds = length(ps - c);
    col = over(premul(vec3<f32>(0.933333, 0.941176, 0.960784), cov(ds - 48.0, px) * lid), col);
    col = over(
        premul(
            vec3<f32>(1.0, 1.0, 1.0),
            profileScleraHalo(ds / 56.0) * cov(ds - 56.0, px) * lid,
        ),
        col,
    );
    col = over(
        premul(vec3<f32>(1.0, 1.0, 1.0), cov(abs(ds - 48.0) - 0.4, px) * 0.16 * lid),
        col,
    );

    // ---- iris / pupil / highlight: translate by gaze, then per-layer scale ----
    let gz = pe - c - G.gaze;

    let d3 = length(gz) / G.l3Scale;
    col = over(premul(vec3<f32>(0.615686, 0.682353, 0.878431), cov(d3 - 33.0, px) * lid), col);

    let d2 = length(gz) / G.l2Scale;
    col = over(premul(vec3<f32>(0.933333, 0.941176, 0.960784), cov(d2 - 24.15, px) * lid), col);
    col = over(premul(vec3<f32>(0.192157, 0.482353, 0.811765), cov(d2 - 23.5, px) * lid), col);

    let d1 = length(gz) / G.l1Scale;
    col = over(
        premul(vec3<f32>(0.960784, 0.972549, 0.992157), cov(abs(d1 - 16.6) - 0.05, px) * lid),
        col,
    );
    col = over(premul(vec3<f32>(0.231373, 0.239216, 0.541176), cov(d1 - 16.0, px) * lid), col);

    // highlight, attached to the layer-two scaling group
    let dh = length(gz / G.l2Scale - (vec2<f32>(98.0, 100.5) - c));
    col = over(
        premul(
            vec3<f32>(0.960784, 0.972549, 0.992157),
            profileHighlightHalo(dh / 18.0) * cov(dh - 18.0, px) * lid,
        ),
        col,
    );
    col = over(
        premul(vec3<f32>(0.960784, 0.972549, 0.992157), cov(dh - 11.0, px) * lid),
        col,
    );

    // ---- full-eye flicker overlay (outside the lid clip) ----
    col = over(premul(vec3<f32>(1.0, 1.0, 1.0), cov(d - 68.6, px) * G.flicker), col);

    // ---- 4x4 scanline texture, group opacity .42 on rgba(201,248,255,.055) ----
    let my = fract(p.y / 4.0) * 4.0;
    let scan = clamp(0.5 + min(my, 1.0 - my) / px, 0.0, 1.0);
    col = over(
        premul(vec3<f32>(0.788235, 0.972549, 1.0), scan * 0.42 * 0.055 * cov(d - 67.0, px)),
        col,
    );
    return col;
}

// Horizontal low-resolution torn-thread noise (feTurbulence + feDisplacementMap,
// baseFrequency ".012 .72", filterRes "96 72").
fn threadNoise(p: vec2<f32>, seed: f32) -> f32 {
    let row = floor(p.y / 1.906);
    let fx = p.x * 0.012;
    let i = floor(fx);
    let s = (fx - i) * (fx - i) * (3.0 - 2.0 * (fx - i));
    let k = u32(i32(seed) * 40503) * 2654435761u;
    let a = hashf(u32(i32(i) + 8192), u32(i32(row) + 8192), k);
    let b = hashf(u32(i32(i) + 8193), u32(i32(row) + 8192), k);
    return mix(a, b, s);
}

fn bandMask(y: f32, y0: f32, y1: f32, px: f32) -> f32 {
    return clamp(0.5 + min(y - y0, y1 - y) / px, 0.0, 1.0);
}

fn evalSignal(p: vec2<f32>, px: f32) -> vec4<f32> {
    let c = vec2<f32>(80.0, 80.0);
    // Inverse of `translateX(gx) skewX(skew)` about the (80,80) transform origin.
    var r = p;
    r.x = p.x - G.gx - (p.y - c.y) * tan(G.skew);

    var res = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if (G.glitchMode > 1.5) {
        // "blocks": frozen lashes + five displaced horizontal copies.
        let la = G.lashAngle;
        res = evalImage(r, la, px);
        var edges = array<f32, 6>(
            12.0,
            G.sliceEdge1,
            G.sliceEdge2,
            G.sliceEdge3,
            G.sliceEdge4,
            148.0,
        );
        var offs = array<f32, 5>(
            G.sliceOffsets.x,
            G.sliceOffsets.y,
            G.sliceOffsets.z,
            G.sliceOffsets.w,
            G.sliceOffset5,
        );
        for (var i = 0; i < 5; i = i + 1) {
            let bm = bandMask(r.y, edges[i], edges[i + 1], px);
            if (bm > 0.0) {
                let ci = evalImage(r - vec2<f32>(offs[i], 0.0), la, px);
                res = over(vec4<f32>(ci.rgb * bm, ci.a * bm), res);
            }
        }
    } else {
        var pi = r;
        if (G.glitchMode > 0.5) {
            pi.x = pi.x - (threadNoise(r, G.glitchSeed) - 0.5) * G.glitchAmp;
        }
        res = evalImage(pi, G.lashAngle, px);
    }

    // Signal-level `brightness()` then `contrast()`, applied to straight alpha.
    if (res.a > 0.0001) {
        var rgb = res.rgb / res.a;
        rgb = rgb * G.bright;
        rgb = (rgb - vec3<f32>(0.5, 0.5, 0.5)) * G.contrast + vec3<f32>(0.5, 0.5, 0.5);
        res = vec4<f32>(clamp(rgb, vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 1.0)) * res.a, res.a);
    }
    return res;
}

// -------------------------------------------------------------------------- //
//  Entry points
// -------------------------------------------------------------------------- //

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle; the fragment stage derives the pixel coordinate.
    var tri = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(tri[idx], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = frag.xy;
    var col = background(p);

    // Eye bounding box (eye units relative to the 160x160 viewBox top-left):
    // filament layer spans x[-180,340] y[-70,250], the pulse ring reaches r≈172.
    let e = (p - G.viewOrigin) / G.eyeScalePx;
    if ((e.x > -204.0) && (e.x < 364.0) && (e.y > -94.0) && (e.y < 274.0)) {
        let px = 1.0 / G.eyeScalePx;
        let c = vec2<f32>(80.0, 80.0);
        var eye = vec4<f32>(0.0, 0.0, 0.0, 0.0);

        eye = over(haloFilaments(e, px) * 0.92, eye);
        eye = over(pulseRing(e, px) * 0.92, eye);

        let dh = length(e - c);
        eye = over(
            premul(
                vec3<f32>(0.788235, 0.972549, 1.0),
                profileOuterHalo(dh / 79.0) * cov(dh - 79.0, px),
            ),
            eye,
        );

        eye = over(evalSignal(e, px), eye);
        col = over(vec4<f32>(eye.rgb * G.eyeOpacity, eye.a * G.eyeOpacity), col);
    }

    // The swap chain image is opaque.
    if (col.a > 0.0001) {
        col = vec4<f32>(col.rgb / col.a, 1.0);
    }
    return vec4<f32>(clamp(col.rgb, vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 1.0)), 1.0);
}
