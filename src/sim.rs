//! The animation world.
//!
//! Every timing constant here is transcribed from the reference implementation
//! (`mascot-runtime.js`, `mascot-motion-clock.js`, `mascot-style.js`) so that the
//! Rust screen saver breathes, rotates its lashes, flickers and tears on exactly
//! the same cadence as the web original.

use std::f32::consts::TAU;

/// Largest iris excursion, in the eye's own 160-unit space.
///
/// The iris group is drawn at radius 33 and the sclera at 48, so there are
/// exactly 15 units of room before the iris spills past its own outline.  The
/// saturating curve never quite reaches the ceiling, so 14.5 spends what there
/// is without risking the silhouette.  (This is the knob for "the pupil should
/// move more" -- check the 15 against the shader radii before raising it.)
const MAX_LOOK_UNITS: f32 = 14.5;
/// Tangent value at which the gaze response has travelled half of
/// `MAX_LOOK_UNITS`.
///
/// A saturating curve keeps small head movements legible while stopping a viewer
/// at the edge of the room from pinning the iris to the corner.  Lowering it
/// makes an ordinary amount of leaning about read as a larger glance.
const LOOK_SOFTNESS: f32 = 0.22;

/// Length of the blink that covers a cross-screen move, in seconds.
const BLINK_SECS: f32 = 0.44;

/// Height of the thing the distance estimate measures, in millimetres.
///
/// That thing is now a *face* -- the height of the detector's bounding box --
/// rather than the height of a skin-toned blob.  This matters: a person's face
/// is roughly the same size as anybody else's, while a blob's height depends on
/// whether they are wearing short sleeves, so the same apparent size maps to
/// the same distance for everyone.  A grown adult's face is about 200 mm from
/// hairline to chin.
const SUBJECT_MM: f32 = 200.0;
/// How long somebody has to stay inside the engage radius before the eye reacts.
const ENGAGE_HOLD_SECS: f32 = 0.30;
/// Length of the "I can see you" acknowledgement, matching the lid curve.
///
/// Most of this is spent half-lidded, and the hold is the part that reads as an
/// acknowledgement.  It is deliberately long -- this is the eye saying it has
/// noticed you, and it is in no hurry to stop.
const ALERT_SECS: f32 = 5.90;

use crate::config::Config;
use crate::monitor::Monitor;
use crate::vision::Reading;

// ---------------------------------------------------------------------- rng --

pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { state: seed | 1 }
    }

    pub fn next_u32(&mut self) -> u32 {
        // xorshift64*
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / 16_777_216.0
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

// ------------------------------------------------------------ motion clock --

/// Cubic-bezier(.72, 0, .28, 1) evaluated as a one-dimensional easing, exactly
/// the way the reference resolves its shared eye phase.
fn ease_eye_phase(value: f32) -> f32 {
    let x = value.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let t = (lo + hi) * 0.5;
        let xt = 3.0 * (1.0 - t) * (1.0 - t) * t * 0.72 + 3.0 * (1.0 - t) * t * t * 0.28 + t * t * t;
        if xt < x {
            lo = t;
        } else {
            hi = t;
        }
    }
    let t = (lo + hi) * 0.5;
    3.0 * (1.0 - t) * t * t + t * t * t
}

/// Triangle phase folded through the easing above.
fn eye_progress(phase: f32) -> f32 {
    let p = phase.rem_euclid(1.0);
    ease_eye_phase(if p <= 0.5 { p * 2.0 } else { 2.0 - p * 2.0 })
}

/// Squared distance from a point to a monitor rectangle (0 when inside).
fn rect_distance(m: &Monitor, x: f32, y: f32) -> f32 {
    let dx = (m.left as f32 - x).max(x - (m.right as f32 - 1.0)).max(0.0);
    let dy = (m.top as f32 - y).max(y - (m.bottom as f32 - 1.0)).max(0.0);
    dx * dx + dy * dy
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ------------------------------------------------------------------ glitch --

#[derive(Clone, Copy, PartialEq, Eq)]
enum GPhase {
    Idle,
    Burst,
    Fade,
}

const MODE_NONE: u8 = 0;
const MODE_THREADS: u8 = 1;
const MODE_BLOCKS: u8 = 2;

struct Glitch {
    enabled: bool,
    phase: GPhase,
    timer: f32,
    mode: u8,
    next_mode: u8,
    pulses_left: u32,
    transition: bool,

    active: bool,
    gx: f32,
    skew: f32,
    bright: f32,
    contrast: f32,
    amp: f32,
    seed: f32,
    offsets: [f32; 5],
    edges: [f32; 4],
    frozen_lash: f32,
    threads_live: bool,

    rng: Rng,
}

impl Glitch {
    fn new(enabled: bool, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let first = if rng.unit() < 0.5 { MODE_THREADS } else { MODE_BLOCKS };
        Glitch {
            enabled,
            phase: GPhase::Idle,
            timer: rng.range(2.3, 4.6),
            mode: MODE_NONE,
            next_mode: first,
            pulses_left: 0,
            transition: false,
            active: false,
            gx: 0.0,
            skew: 0.0,
            bright: 1.0,
            contrast: 1.0,
            amp: 0.0,
            seed: 1.0,
            offsets: [0.0; 5],
            edges: [46.0, 80.0, 114.0, 148.0],
            frozen_lash: 0.0,
            threads_live: false,
            rng,
        }
    }

    fn set_enabled(&mut self, on: bool) {
        if on == self.enabled {
            return;
        }
        self.enabled = on;
        if !on {
            self.active = false;
            self.phase = GPhase::Idle;
            self.transition = false;
        }
    }

    /// The two-burst fault the reference plays whenever the expression changes.
    fn trigger_transition(&mut self, lash_angle: f32) {
        if !self.enabled {
            return;
        }
        // 2 pulses of "blocks" then 6 pulses of "threads", at a 40ms cadence.
        self.mode = MODE_BLOCKS;
        self.next_mode = MODE_BLOCKS;
        self.transition = true;
        self.phase = GPhase::Burst;
        self.pulses_left = 7;
        self.frozen_lash = lash_angle;
        self.new_block_layout();
        self.emit_pulse(true);
        self.timer = 0.040;
    }

    fn gap_seconds(&mut self) -> f32 {
        if self.transition {
            return 0.040;
        }
        match self.mode {
            MODE_THREADS => self.rng.range(0.030, 0.042),
            _ => self.rng.range(0.056, 0.104),
        }
    }

    fn fade_seconds(&mut self) -> f32 {
        match self.mode {
            MODE_THREADS => self.rng.range(0.034, 0.076),
            _ => self.rng.range(0.048, 0.096),
        }
    }

    fn idle_seconds(&mut self, rate: f32) -> f32 {
        self.rng.range(2.3, 4.6) / rate.max(0.05)
    }

    fn begin_event(&mut self, lash_angle: f32) {
        let mode = if self.next_mode == MODE_THREADS { MODE_BLOCKS } else { MODE_THREADS };
        self.next_mode = mode;
        self.mode = mode;
        self.pulses_left = match mode {
            MODE_THREADS => 4,
            _ => 2 * (1 + (self.rng.unit() * 2.0) as u32),
        };
        if mode == MODE_BLOCKS {
            self.new_block_layout();
            self.frozen_lash = lash_angle;
        }
        self.emit_pulse(false);
        self.pulses_left = self.pulses_left.saturating_sub(1);
        self.phase = GPhase::Burst;
        self.timer = self.gap_seconds();
    }

    fn new_block_layout(&mut self) {
        let mut weights = [0.0f32; 5];
        let mut total = 0.0;
        for w in weights.iter_mut() {
            *w = self.rng.range(0.9, 1.1);
            total += *w;
        }
        let mut y = 12.0f32;
        for i in 0..4 {
            y += 136.0 * weights[i] / total;
            self.edges[i] = y;
        }
    }

    fn emit_pulse(&mut self, is_transition: bool) {
        let (lo_x, hi_x) = if is_transition { (-1.6, 1.6) } else { (-0.9, 0.9) };
        let (lo_k, hi_k) = if is_transition { (-0.32, 0.32) } else { (-0.24, 0.24) };
        let (lo_b, hi_b) = if is_transition { (1.06, 1.18) } else { (1.02, 1.14) };
        let (lo_c, hi_c) = if is_transition { (1.08, 1.22) } else { (1.02, 1.16) };

        self.gx = self.rng.range(lo_x, hi_x);
        self.skew = self.rng.range(lo_k, hi_k).to_radians();
        self.bright = self.rng.range(lo_b, hi_b);
        self.contrast = self.rng.range(lo_c, hi_c);
        self.active = true;

        if self.mode == MODE_THREADS {
            // A new turbulence field only at the start of each thread burst.
            if !self.threads_live {
                self.seed = self.rng.range(1.0, 999.0);
                self.amp = if is_transition {
                    self.rng.range(30.0, 46.0)
                } else {
                    self.rng.range(17.0, 29.0)
                };
            }
        } else {
            let polarity = if self.rng.unit() < 0.5 { -1.0 } else { 1.0 };
            let strength = if is_transition {
                self.rng.range(4.2, 6.8)
            } else {
                self.rng.range(1.4, 3.0)
            };
            for i in 0..5 {
                let dir = if i % 2 == 0 { -polarity } else { polarity };
                self.offsets[i] = dir * (strength + self.rng.range(-0.35, 0.35));
            }
        }
        self.threads_live = self.mode == MODE_THREADS;
    }

    fn update(&mut self, dt: f32, lash_angle: f32, rate: f32) {
        if !self.enabled {
            self.active = false;
            self.transition = false;
            return;
        }
        self.timer -= dt;
        if self.timer > 0.0 {
            return;
        }
        match self.phase {
            GPhase::Idle => self.begin_event(lash_angle),
            GPhase::Burst => {
                if self.pulses_left > 0 {
                    self.emit_pulse(self.transition);
                    self.pulses_left -= 1;
                    self.timer = self.gap_seconds();
                } else {
                    self.phase = GPhase::Fade;
                    self.timer = self.fade_seconds();
                }
            }
            GPhase::Fade => {
                self.active = false;
                self.transition = false;
                self.mode = MODE_NONE;
                self.phase = GPhase::Idle;
                self.timer = self.idle_seconds(rate);
            }
        }
    }
}

// ------------------------------------------------------------- eye flicker --

struct Flicker {
    wait: f32,
    remaining: f32,
    duration: f32,
    o1: f32,
    omid: f32,
    o2: f32,
    rng: Rng,
}

impl Flicker {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let wait = rng.range(1.75, 3.6);
        Flicker { wait, remaining: 0.0, duration: 0.2, o1: 0.0, omid: 0.0, o2: 0.0, rng }
    }

    fn update(&mut self, dt: f32, rate: f32) -> f32 {
        if self.remaining > 0.0 {
            self.remaining -= dt;
            if self.remaining <= 0.0 {
                self.wait = self.rng.range(1.75, 3.6) / rate.max(0.05);
                self.remaining = 0.0;
                return 0.0;
            }
            let p = 1.0 - (self.remaining / self.duration);
            // keyframes: 0% -> 0, 22% -> o1, 48% -> mid, 72% -> o2, 100% -> 0
            return if p < 0.22 {
                self.o1 * (p / 0.22)
            } else if p < 0.48 {
                self.o1 + (self.omid - self.o1) * ((p - 0.22) / 0.26)
            } else if p < 0.72 {
                self.omid + (self.o2 - self.omid) * ((p - 0.48) / 0.24)
            } else {
                self.o2 * (1.0 - (p - 0.72) / 0.28)
            };
        }
        self.wait -= dt;
        if self.wait <= 0.0 {
            self.duration = self.rng.range(0.155, 0.255);
            self.remaining = self.duration;
            self.o1 = self.rng.range(0.026, 0.048);
            self.omid = self.rng.range(0.006, 0.016);
            self.o2 = self.rng.range(0.018, 0.040);
        }
        0.0
    }
}

// -------------------------------------------------------------------- lids --

/// Upper-lid closure.  `0` is wide open, `1` is shut.
struct Lid {
    current: f32,
    target: f32,
}

impl Lid {
    fn new() -> Self {
        Lid { current: 0.0, target: 0.0 }
    }

    /// The "I have noticed you" acknowledgement curve: a quick half-close, a
    /// held stare, then a relaxed attentive narrowing.
    fn alert_curve(t: f32) -> f32 {
        // Times are in seconds and must stay inside `ALERT_SECS`.  The shape is
        // deliberate: drop into the half-lid quickly, hold it for most of the
        // duration, then snap back open in a quarter of a second.  Sliding back
        // open slowly reads as drifting off rather than as having noticed you.
        //
        // The hold sits at 0.55 because that puts the lid line on the eye's own
        // centre (`lidCenterY = 20 + 110 * lid` = 80.5), which is what "half
        // closed" should look like.  Higher values pull the lid visibly below
        // the middle and read as sleepy rather than attentive.
        const KEYS: [(f32, f32); 6] = [
            (0.00, 0.00),
            (0.25, 0.58),
            (0.70, 0.56),
            (5.35, 0.55),
            (5.60, 0.11),
            (5.90, 0.11),
        ];
        if t <= KEYS[0].0 {
            return KEYS[0].1;
        }
        for w in KEYS.windows(2) {
            let (t0, v0) = w[0];
            let (t1, v1) = w[1];
            if t <= t1 {
                return v0 + (v1 - v0) * smoothstep(t0, t1, t);
            }
        }
        KEYS[KEYS.len() - 1].1
    }

    fn update(&mut self, dt: f32, target: f32) {
        self.target = target;
        let tau = 0.085f32;
        let k = 1.0 - (-dt / tau).exp();
        self.current += (self.target - self.current) * k.clamp(0.0, 1.0);
    }
}

// ------------------------------------------------------------------- state --

#[derive(Clone, Copy, Debug)]
enum Presence {
    Absent,
    Alert { t: f32 },
    Tracking,
}

/// Everything the renderer needs for one frame.  The eye centre is *not* here —
/// that is per window, since each window carries a different monitor origin.
#[derive(Clone, Copy, Debug)]
pub struct FrameParams {
    pub eye_scale_px: f32,
    pub eye_size_px: f32,
    pub lash_angle: f32,
    pub lid_center_y: f32,
    pub lid_curve: f32,
    pub sclera: f32,
    pub l3: f32,
    pub l2: f32,
    pub l1: f32,
    pub flicker: f32,
    pub gaze: (f32, f32),
    pub pulse_phase: f32,
    pub pulse_cycle: f32,
    pub glitch_mode: f32,
    pub glitch_amp: f32,
    pub bright: f32,
    pub contrast: f32,
    pub skew: f32,
    pub gx: f32,
    pub slice_offsets: [f32; 5],
    pub slice_edges: [f32; 4],
    pub glitch_seed: f32,
    pub opacity: f32,
}

impl Default for FrameParams {
    fn default() -> Self {
        FrameParams {
            eye_scale_px: 3.0,
            eye_size_px: 480.0,
            lash_angle: 0.0,
            lid_center_y: 100.0,
            lid_curve: 8.25,
            sclera: 0.985,
            l3: 1.0,
            l2: 1.0,
            l1: 1.0,
            flicker: 0.0,
            gaze: (0.0, 0.0),
            pulse_phase: -1.0,
            pulse_cycle: 4.0,
            glitch_mode: 0.0,
            glitch_amp: 0.0,
            bright: 1.0,
            contrast: 1.0,
            skew: 0.0,
            gx: 0.0,
            slice_offsets: [0.0; 5],
            slice_edges: [46.0, 80.0, 114.0, 148.0],
            glitch_seed: 1.0,
            opacity: 1.0,
        }
    }
}

pub struct World {
    pub cfg: Config,
    pub monitors: Vec<Monitor>,
    pub t: f32,
    pub rate: f32,
    pub lash_phase: f32,
    motion_phase: f32,
    pub eye_pos: (f32, f32),
    eye_target: (f32, f32),
    opacity: f32,
    opacity_target: f32,
    gaze: (f32, f32),
    gaze_target: (f32, f32),
    presence: Presence,
    /// Viewer position in millimetres: the desktop's X/Y axes plus Z pointing
    /// out of the screen.  The camera lives on the screen plane (Z = 0).
    viewer_mm: (f32, f32, f32),
    person_valid: bool,
    /// Index of the monitor the eyeball is currently sitting on.
    host_index: usize,
    /// Countdown of the relocation blink, in seconds.  Zero means eyes open.
    blink_left: f32,
    /// True while somebody is close enough to be worth watching.
    engaged: bool,
    /// Seconds spent continuously inside the engage radius.
    hold: f32,
    /// Seconds since the viewer last satisfied the radius test.
    away_for: f32,
    /// Best estimate of how far away the viewer is, millimetres.
    distance_mm: f32,
    eye_size_px: f32,
    glitch: Glitch,
    flicker: Flicker,
    flicker_last: f32,
    lid: Lid,
}

/// Where the viewer is, in millimetres, from what the camera saw.
///
/// The camera sits somewhere on the desktop plane -- including in the seam
/// between two stacked screens -- so its millimetre position is fixed.  A face
/// at `(u, v)` in the frame maps to a direction on the camera's optical axis;
/// the axis is then corrected by the mount trim angles, and the viewer is placed
/// `distance_mm` along the resulting ray.
///
/// Shared by the renderer and the settings preview so the red dot in the dialog
/// is the same point the eye will actually look at.
pub fn project_viewer(
    cfg: &Config,
    monitors: &[Monitor],
    camera_monitor: usize,
    r: Reading,
    distance_mm: f32,
) -> (f32, f32, f32) {
    let cam = &monitors[camera_monitor.min(monitors.len().saturating_sub(1))];
    let ppm = cfg.px_per_mm.max(0.01);
    let cam_x = (cam.left as f32 + cfg.camera_off_x * cam.width() as f32) / ppm;
    let cam_y = (cam.top as f32 + cfg.camera_off_y * cam.height() as f32) / ppm;

    // u runs left..right across the *desktop*, v runs bottom..top, because the
    // desktop's Y axis grows downwards while the frame's grows upwards from its
    // bottom edge.
    //
    // The horizontal axis needs one extra flip that the vertical one does not:
    // the camera faces the viewer, so the viewer's own right hand lands on the
    // *left* of the frame -- the same reason a video call shows you mirrored
    // until the client flips it.  Reading a bearing straight off the frame
    // therefore puts the eye on the wrong side for anybody sitting off-centre,
    // which reads as "the bearing is just wrong" rather than as anything
    // obviously broken.
    let u = ((0.5 - r.x) * 2.0).clamp(-1.0, 1.0);
    let v = ((0.5 - r.y) * 2.0).clamp(-1.0, 1.0);
    let tan_h = (cfg.camera_fov_deg.to_radians() * 0.5).tan().max(0.05);
    let tan_v = tan_h / r.aspect.max(0.2);

    // Camera-local direction, then the mount correction: yaw turns the lens
    // left/right (positive = the lens points right), pitch up/down.
    let (mut dx, mut dy, mut dz) = (u * tan_h, -v * tan_v, 1.0);
    if cfg.camera_yaw_trim_deg != 0.0 {
        let a = cfg.camera_yaw_trim_deg.to_radians();
        let (s_a, c_a) = (a.sin(), a.cos());
        let (nx, nz) = (dx * c_a + dz * s_a, -dx * s_a + dz * c_a);
        dx = nx;
        dz = nz;
    }
    if cfg.camera_pitch_trim_deg != 0.0 {
        let b = cfg.camera_pitch_trim_deg.to_radians();
        let (s_b, c_b) = (b.sin(), b.cos());
        let (ny, nz) = (dy * c_b - dz * s_b, dy * s_b + dz * c_b);
        dy = ny;
        dz = nz;
    }

    let len = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-4);
    (
        cam_x + dx / len * distance_mm,
        cam_y + dy / len * distance_mm,
        dz / len * distance_mm,
    )
}

/// How far away the viewer is, inferred from how much of the camera frame their
/// blob covers.  This is what "approaching" and "leaving" are measured against,
/// and it also feeds the gaze geometry, so leaning in genuinely deepens the
/// eye's attention instead of just being decorative.
pub fn estimate_distance(cfg: &Config, r: Reading) -> f32 {
    if r.size < 0.04 {
        return cfg.camera_distance_mm;
    }
    let tan_v = ((cfg.camera_fov_deg.to_radians() * 0.5).tan() / r.aspect.max(0.2)).max(0.05);
    let d = (SUBJECT_MM * 0.5) / (r.size * tan_v);
    d.clamp(150.0, 4000.0)
}

/// Resolves `camera_monitor` (which may be -1 for "the primary") to an index.
pub fn camera_monitor_index(cfg: &Config, monitors: &[Monitor]) -> usize {
    if cfg.camera_monitor < 0 {
        monitors.iter().position(|m| m.primary).unwrap_or(0)
    } else {
        (cfg.camera_monitor as usize).min(monitors.len().saturating_sub(1))
    }
}

impl World {
    pub fn new(cfg: Config, monitors: Vec<Monitor>) -> Self {
        let home_monitor = monitors
            .iter()
            .find(|m| m.primary)
            .or_else(|| monitors.first())
            .cloned()
            .unwrap_or_else(crate::monitor::fallback);

        let home = home_monitor.center();
        let eye_size_px = cfg.eye_size_px(home_monitor.height());
        let rate = cfg.animation_rate;

        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);

        // Place the (unknown) viewer straight out from the eye until the camera
        // says otherwise.
        let ppm = cfg.px_per_mm.max(0.01);
        let fallback_distance_mm = cfg.camera_distance_mm;
        let viewer_mm = (home.0 / ppm, home.1 / ppm, fallback_distance_mm);

        World {
            glitch: Glitch::new(cfg.glitch_enabled, seed),
            flicker: Flicker::new(seed ^ 0xA5A5_5A5A),
            flicker_last: 0.0,
            lid: Lid::new(),
            rate,
            cfg,
            monitors,
            t: 0.0,
            lash_phase: 0.0,
            motion_phase: 0.0,
            eye_pos: home,
            eye_target: home,
            opacity: 1.0,
            opacity_target: 1.0,
            gaze: (0.0, 0.0),
            gaze_target: (0.0, 0.0),
            presence: Presence::Absent,
            viewer_mm,
            person_valid: false,
            host_index: home_monitor.index as usize,
            blink_left: 0.0,
            engaged: false,
            hold: 0.0,
            away_for: 0.0,
            distance_mm: fallback_distance_mm,
            eye_size_px,
        }
    }

    pub fn home_monitor_index(&self) -> usize {
        self.monitors
            .iter()
            .position(|m| m.primary)
            .unwrap_or(0)
    }

    pub fn camera_monitor_index(&self) -> usize {
        camera_monitor_index(&self.cfg, &self.monitors)
    }

    /// Overrides how large the eye is drawn.  The preview thumbnail is a small
    /// window on a big monitor, so it has to be sized against its own client
    /// area rather than the monitor it happens to sit on.
    pub fn set_eye_size_px(&mut self, px: f32) {
        self.eye_size_px = px.clamp(48.0, 100_000.0);
    }

    pub fn set_glitch_enabled(&mut self, on: bool) {
        self.glitch.set_enabled(on);
    }


    fn locate_viewer(&self, r: Reading, distance_mm: f32) -> (f32, f32, f32) {
        project_viewer(
            &self.cfg,
            &self.monitors,
            self.camera_monitor_index(),
            r,
            distance_mm,
        )
    }

    fn estimate_distance_mm(&self, r: Reading) -> f32 {
        estimate_distance(&self.cfg, r)
    }

    /// Which screen should be carrying the eye right now.
    ///
    /// The viewer's reconstructed position is a point in the desktop plane, so
    /// "the nearer screen" is simply the monitor that point sits in -- or the
    /// closest one when it lands in the gap between two screens.  With nobody
    /// around the eye goes back to the primary monitor.
    fn host_monitor(&self, engaged: bool) -> usize {
        if !engaged || !self.cfg.gaze_enabled {
            return self.home_monitor_index();
        }
        let ppm = self.cfg.px_per_mm.max(0.01);
        let x = self.viewer_mm.0 * ppm;
        let y = self.viewer_mm.1 * ppm;
        self.monitors
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                rect_distance(a, x, y)
                    .partial_cmp(&rect_distance(b, x, y))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }


    /// Iris offset, in eye units, that makes the drawn eye point at the viewer.
    ///
    /// A flat drawing cannot rotate, so "looking at" is expressed the only way it
    /// can be: the iris slides along the target's direction *within the screen
    /// plane*, scaled by the real tangent of the gaze angle -- `lateral / depth`
    /// -- and passed through a saturating curve so the eye still reads as
    /// interested when the viewer is far off to one side.
    fn gaze_for(&self, present: bool) -> (f32, f32) {
        if !present {
            return (0.0, 0.0);
        }
        let ppm = self.cfg.px_per_mm.max(0.01);
        let eye_x = self.eye_pos.0 / ppm;
        let eye_y = self.eye_pos.1 / ppm;
        let (px, py, pz) = self.viewer_mm;
        let depth = pz.max(120.0);

        let tan_x = (px - eye_x) / depth;
        let tan_y = (py - eye_y) / depth;
        let mag = (tan_x * tan_x + tan_y * tan_y).sqrt();
        if mag < 1e-5 {
            return (0.0, 0.0);
        }
        let units = MAX_LOOK_UNITS * mag / (mag + LOOK_SOFTNESS);
        (tan_x / mag * units, tan_y / mag * units)
    }

    /// One-line description of where the eye is aiming, for the diagnostics log.
    pub fn gaze_debug(&self) -> String {
        let ppm = self.cfg.px_per_mm.max(0.01);
        let (px, py, pz) = self.viewer_mm;
        let depth = pz.max(120.0);
        let hx = ((px - self.eye_pos.0 / ppm) / depth).atan().to_degrees();
        let hy = ((py - self.eye_pos.1 / ppm) / depth).atan().to_degrees();
        format!(
            "dist={:.0}mm yaw={hx:+.1}deg pitch={hy:+.1}deg iris=({:+.2},{:+.2}) host=#{} engaged={}",
            self.distance_mm,
            self.gaze.0,
            self.gaze.1,
            self.host_index,
            self.engaged
        )
    }

    pub fn update(&mut self, dt: f32, reading: Option<Reading>) {
        self.t += dt;

        let lash_period = 15.0 / self.rate.max(0.05);
        self.lash_phase = (self.lash_phase + dt / lash_period).rem_euclid(1.0);
        let lash_angle = self.lash_phase * TAU;

        // 1440ms master clock, shared by every breathing layer.
        self.motion_phase = (self.motion_phase + dt * self.rate / 1.440).rem_euclid(1.0);

        // ---- presence ----------------------------------------------------
        // ---- presence: approach and leave, with hysteresis -----------------
        let seen = reading.map(|r| r.present).unwrap_or(false);
        if let Some(r) = reading.filter(|_| seen) {
            self.distance_mm = self.estimate_distance_mm(r);
            self.viewer_mm = self.locate_viewer(r, self.distance_mm);
        }
        self.person_valid = seen;

        // Crossing `engage_distance_mm` gets you noticed; you then have to
        // retreat past `release_distance_mm` before the eye loses interest, so
        // somebody shifting in their chair never flickers between states.
        let threshold = if self.engaged {
            self.cfg.release_distance_mm
        } else {
            self.cfg.engage_distance_mm
        };
        let in_range = seen && self.distance_mm <= threshold;
        if in_range {
            self.hold += dt;
            self.away_for = 0.0;
        } else {
            self.hold = if self.engaged { self.hold } else { 0.0 };
            self.away_for += dt;
        }

        if !self.engaged {
            if self.hold >= ENGAGE_HOLD_SECS {
                self.engaged = true;
                self.away_for = 0.0;
                self.presence = Presence::Alert { t: 0.0 };
                self.glitch.trigger_transition(lash_angle);
            }
        } else if self.away_for >= self.cfg.lost_timeout_sec {
            self.engaged = false;
            self.hold = 0.0;
            self.presence = Presence::Absent;
        }

        if let Presence::Alert { t } = self.presence {
            let nt = t + dt;
            self.presence = if nt >= ALERT_SECS {
                Presence::Tracking
            } else {
                Presence::Alert { t: nt }
            };
        }

        // The pupil watches anything it can see; the *screens* and the
        // acknowledgement animation only engage once somebody is close.
        let watching = seen;
        let engaged = self.engaged;

        // ---- lid ---------------------------------------------------------
        let mut lid_target = match self.presence {
            Presence::Alert { t } => Lid::alert_curve(t),
            Presence::Tracking => 0.11,
            Presence::Absent => 0.0,
        };

        // The relocation blink rides on top of whatever the expression wants:
        // shut fast, hold for a beat, open a little slower.
        if self.blink_left > 0.0 {
            self.blink_left = (self.blink_left - dt).max(0.0);
            let p = 1.0 - self.blink_left / BLINK_SECS;
            let shut = smoothstep(0.0, 0.30, p);
            let open = smoothstep(0.60, 1.0, p);
            lid_target = lid_target.max((shut - open).clamp(0.0, 1.0));
        }
        self.lid.update(dt, lid_target);

        // ---- which screen hosts the eye ------------------------------------
        // Inside a screen the eye is always dead centre: it is the *screen* that
        // changes, not the eye's position within it.  Only the iris tracks.
        let host = self.host_monitor(engaged);
        if host != self.host_index {
            self.host_index = host;
            // A cross-screen move is masked by two things at once: the same
            // two-burst fault the reference plays for expression changes, and a
            // full blink so the eye is never seen mid-teleport.
            self.glitch.trigger_transition(lash_angle);
            self.blink_left = BLINK_SECS;
        }
        self.eye_target = self.monitors[self.host_index].center();
        let travel = (1.0 - (-dt * 7.0).exp()).clamp(0.0, 1.0);
        self.eye_pos.0 += (self.eye_target.0 - self.eye_pos.0) * travel;
        self.eye_pos.1 += (self.eye_target.1 - self.eye_pos.1) * travel;

        // ---- gaze ----------------------------------------------------------
        self.gaze_target = if self.cfg.gaze_enabled {
            self.gaze_for(watching)
        } else {
            (0.0, 0.0)
        };
        // A slightly slow, slightly under-damped follow keeps the iris from
        // snapping when the detector's centroid jumps a frame.
        let gk = (1.0 - (-dt * 3.2 * self.cfg.follow_speed.max(0.05)).exp()).clamp(0.0, 1.0);
        self.gaze.0 += (self.gaze_target.0 - self.gaze.0) * gk;
        self.gaze.1 += (self.gaze_target.1 - self.gaze.1) * gk;

        // ---- opacity -------------------------------------------------------
        self.opacity += (self.opacity_target - self.opacity) * (1.0 - (-dt * 5.0).exp());

        // ---- procedural events ---------------------------------------------
        self.glitch.update(dt, lash_angle, self.rate);
        self.flicker_last = self.flicker.update(dt, self.rate);
    }

    pub fn params(&self) -> FrameParams {
        let sclera = 0.985 + (0.91 - 0.985) * eye_progress(self.motion_phase);
        let l3 = 1.0 + (0.90 - 1.0) * eye_progress(self.motion_phase + 45.0 / 1440.0);
        let l2 = 1.0 + (0.87 - 1.0) * eye_progress(self.motion_phase + 90.0 / 1440.0);
        let l1 = 1.0 + (0.85 - 1.0) * eye_progress(self.motion_phase + 180.0 / 1440.0);

        let pulse_cycle = 1.44 + 2.56 / self.rate.max(0.05);
        let pulse_phase = (self.t / pulse_cycle).rem_euclid(1.0);

        // Lid travel has to span the whole eye: 20 sits just above the sclera
        // (radius 48 around y=80) and 130 is far enough past its bottom edge to
        // shut it completely, so a blink really blinks.
        let lid = self.lid.current.clamp(0.0, 1.0);
        let lid_center_y = if lid <= 0.0005 { 100.0 } else { 20.0 + (130.0 - 20.0) * lid };

        let (gm, amp, gx, skew, bright, contrast) = if self.glitch.active {
            (
                self.glitch.mode as f32,
                self.glitch.amp,
                self.glitch.gx,
                self.glitch.skew,
                self.glitch.bright,
                self.glitch.contrast,
            )
        } else {
            (0.0, 0.0, 0.0, 0.0, 1.0, 1.0)
        };

        FrameParams {
            eye_scale_px: self.eye_size_px / 160.0,
            eye_size_px: self.eye_size_px,
            lash_angle: self.lash_phase * TAU,
            lid_center_y,
            lid_curve: 8.25,
            sclera,
            l3,
            l2,
            l1,
            flicker: self.flicker_last,
            gaze: self.gaze,
            pulse_phase,
            pulse_cycle,
            glitch_mode: gm,
            glitch_amp: amp,
            bright,
            contrast,
            skew,
            gx,
            slice_offsets: self.glitch.offsets,
            slice_edges: self.glitch.edges,
            glitch_seed: self.glitch.seed,
            opacity: self.opacity,
        }
    }

    /// Human-readable status for the configuration dialog preview line.
    pub fn status_line(&self) -> String {
        if !self.engaged {
            return match self.person_valid {
                true => format!("有人但较远 ({:.1}m)", self.distance_mm / 1000.0),
                false => "等待有人靠近".to_string(),
            };
        }
        match self.presence {
            Presence::Absent => "等待有人靠近".to_string(),
            Presence::Alert { .. } => format!("发现人物，正在注视 ({:.1}m)", self.distance_mm / 1000.0),
            Presence::Tracking => format!("正在跟随 ({:.1}m)", self.distance_mm / 1000.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::monitor::Monitor;

    fn mon(index: u32, left: i32, top: i32, w: i32, h: i32, primary: bool) -> Monitor {
        Monitor {
            index,
            handle: std::ptr::null_mut(),
            left,
            top,
            right: left + w,
            bottom: top + h,
            primary,
            device: format!("DISPLAY{index}"),
        }
    }

    /// Two 1080p screens stacked one above the other -- the arrangement the
    /// seam-mounted-webcam case comes from.
    fn stacked() -> Vec<Monitor> {
        vec![
            mon(0, 0, 0, 1920, 1080, true),
            mon(1, 0, 1080, 1920, 1080, false),
        ]
    }

    fn reading_at(x: f32, y: f32, size: f32) -> Reading {
        Reading {
            present: true,
            source: crate::vision::Source::Face,
            confidence: 0.1,
            x,
            y,
            size,
            aspect: 16.0 / 9.0,
        }
    }

    fn world(cfg: Config) -> World {
        World::new(cfg, stacked())
    }

    #[test]
    fn host_switches_between_stacked_screens() {
        let mut cfg = Config::default();
        cfg.gaze_enabled = true;
        cfg.px_per_mm = 1.0;
        let mut w = world(cfg);

        // The eye rests on the primary screen.
        assert_eq!(w.host_monitor(false), 0);

        // Somebody below the seam belongs to the lower screen...
        w.viewer_mm = (960.0, 1600.0, 700.0);
        assert_eq!(w.host_monitor(true), 1);

        // ...and somebody above it to the upper one.
        w.viewer_mm = (960.0, 300.0, 700.0);
        assert_eq!(w.host_monitor(true), 0);
    }

    #[test]
    fn camera_in_the_vertical_seam_projects_where_it_sits() {
        let mut cfg = Config::default();
        cfg.camera_monitor = 0;
        cfg.camera_off_x = 0.5;
        cfg.camera_off_y = 1.05; // 5% of a screen *below* the reference monitor
        cfg.px_per_mm = 1.0; // one pixel per millimetre keeps this readable
        cfg.camera_distance_mm = 700.0;
        let w = world(cfg);

        let (x, y, z) = w.locate_viewer(reading_at(0.5, 0.5, 0.25), 700.0);
        assert!((x - 960.0).abs() < 1.0, "x = {x}");
        assert!((y - 1134.0).abs() < 1.0, "y = {y} (expected 1080 + 0.05*1080)");
        assert!((z - 700.0).abs() < 1.0, "z = {z}");
    }

    #[test]
    fn a_face_in_the_lower_half_is_placed_below_the_camera() {
        let mut cfg = Config::default();
        cfg.px_per_mm = 1.0;
        cfg.camera_fov_deg = 60.0;
        let w = world(cfg);
        // Camera default sits at the top centre of the primary: (960, 0).
        // y = 0.8 in the frame means well below the optical axis.
        let off_axis = w.locate_viewer(reading_at(0.5, 0.8, 0.2), 800.0);
        let centred = w.locate_viewer(reading_at(0.5, 0.5, 0.2), 800.0);
        assert!(off_axis.1 > centred.1, "{off_axis:?} should be lower than {centred:?}");
        assert!(off_axis.2 < centred.2, "an off-axis face is also nearer in depth");
    }

    #[test]
    fn a_face_on_the_right_of_the_frame_means_the_viewer_moved_left() {
        let mut cfg = Config::default();
        cfg.px_per_mm = 1.0;
        cfg.camera_fov_deg = 60.0;
        let w = world(cfg);

        // The camera faces the viewer, so the viewer's own right hand shows up
        // on the *left* of the frame.  Reconstructing the desktop position has
        // to undo that flip: somebody whose face appears on the right of the
        // frame is standing to the left of the camera, not the right.
        let frame_right = w.locate_viewer(reading_at(0.9, 0.5, 0.2), 800.0);
        let frame_left = w.locate_viewer(reading_at(0.1, 0.5, 0.2), 800.0);

        assert!(
            frame_right.0 < frame_left.0,
            "frame-right should map to desktop-left: {frame_right:?} vs {frame_left:?}"
        );
        assert!(
            (frame_right.1 - frame_left.1).abs() < 1.0,
            "a horizontal-only move must not shift the vertical position"
        );
    }

    #[test]
    fn gaze_turns_towards_the_viewer_and_saturates() {        let mut cfg = Config::default();
        cfg.px_per_mm = 1.0;
        let mut w = world(cfg);

        // 300 mm to the right of the eye, 700 mm deep.
        w.viewer_mm = (960.0 + 300.0, 540.0, 700.0);
        let (gx, gy) = w.gaze_for(true);
        // 14.5 * 0.4286 / (0.4286 + 0.22) = 9.58
        assert!((gx - 9.58).abs() < 0.2, "gx = {gx}");
        assert!(gy.abs() < 0.5, "gy = {gy}");

        // A viewer far off to the side still cannot push the iris off the sclera.
        w.viewer_mm = (960.0 + 4000.0, 540.0, 700.0);
        let (gx, _) = w.gaze_for(true);
        assert!(gx <= MAX_LOOK_UNITS + 0.01, "gx = {gx}");
        assert!(gx > 9.0, "still clearly looking sideways: {gx}");
        // Room before the iris (r=33) leaves the sclera (r=48): 15 units.
        assert!(gx < 15.0, "the iris would spill past the sclera: {gx}");

        // Nobody there: dead centre.
        assert_eq!(w.gaze_for(false), (0.0, 0.0));
    }

    #[test]
    fn the_eye_never_leaves_the_middle_of_its_screen() {
        let mut cfg = Config::default();
        cfg.px_per_mm = 1.0;
        let mut w = world(cfg);
        // Feed an extreme detection and run enough frames to settle.
        for _ in 0..600 {
            w.update(1.0 / 60.0, Some(reading_at(0.02, 0.5, 0.3)));
        }
        let home = w.monitors[w.home_monitor_index()].center();
        assert!(
            (w.eye_pos.0 - home.0).abs() < 1.0 && (w.eye_pos.1 - home.1).abs() < 1.0,
            "eye drifted to {:?}, home is {home:?}",
            w.eye_pos
        );
    }

    #[test]
    fn approach_and_leave_have_hysteresis() {
        let mut cfg = Config::default();
        cfg.px_per_mm = 1.0;
        cfg.camera_fov_deg = 60.0;
        cfg.engage_distance_mm = 900.0;
        cfg.release_distance_mm = 1400.0;
        cfg.lost_timeout_sec = 2.0;
        let mut w = world(cfg);

        // A big blob reads as close, so the eye engages.
        let close = reading_at(0.5, 0.5, 0.6);
        for _ in 0..120 {
            w.update(1.0 / 60.0, Some(close));
        }
        assert!(w.engaged, "a near viewer should engage, distance = {}", w.distance_mm);

        // Drifting to the far side of the release radius does not drop out
        // instantly -- that is the whole point of the hysteresis.
        let far = reading_at(0.5, 0.5, 0.12);
        w.update(1.0 / 60.0, Some(far));
        assert!(w.engaged, "must not flicker off in a single frame");

        // ... but sustained distance eventually does.
        for _ in 0..240 {
            w.update(1.0 / 60.0, Some(far));
        }
        assert!(!w.engaged, "should have released, distance = {}", w.distance_mm);
    }

    #[test]
    fn distance_estimate_tracks_apparent_size() {
        let cfg = Config::default();
        let w = world(cfg);
        let near = w.estimate_distance_mm(reading_at(0.5, 0.5, 0.5));
        let far = w.estimate_distance_mm(reading_at(0.5, 0.5, 0.1));
        assert!(near < far, "bigger blob should read as closer: {near} vs {far}");
        assert!(near > 150.0 && far < 4000.0);
    }
}
