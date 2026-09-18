//! Webcam presence sensing.
//!
//! A dedicated thread owns the capture device and continuously reduces frames to
//! *is somebody there*, *which way are they*, and *roughly how far*.  The render
//! loop only ever sees a [`Reading`].
//!
//! Detection runs as a two-stage chain:
//!
//! 1. **Face** ([`crate::face`]) -- accurate bearing, and a bounding box whose
//!    height is a solid distance cue.  This is the source that decides where the
//!    eye actually aims.
//! 2. **Motion** ([`crate::motion`]) -- used only when stage 1 comes up empty.
//!    Somebody who turns to a second monitor is still *here*, and blanking out
//!    because the detector lost their face would look like a bug.
//!
//! Bearing is only ever taken from those two.  The skin-tone blob detector that
//! used to live here was removed: it answered "where is something skin coloured",
//! which on a real desk means a wooden surface, a beige wall or the viewer's own
//! torso, so the eye regularly aimed at the wrong thing entirely.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;

use crate::face;
use crate::motion;
use crate::pixels;

pub const STATUS_STARTING: u32 = 0;
pub const STATUS_STREAMING: u32 = 1;
pub const STATUS_ERROR: u32 = 2;
pub const STATUS_STOPPED: u32 = 3;

/// What produced the current bearing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// Nothing is being observed.
    #[default]
    None,
    /// The face detector found somebody.
    Face,
    /// The face detector found nobody; this is only movement.
    Motion,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::None => "none",
            Source::Face => "face",
            Source::Motion => "motion",
        }
    }
}

/// Snapshot handed to the animation loop each frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reading {
    pub present: bool,
    /// Where the bearing came from, for the diagnostics log.
    pub source: Source,
    /// Smoothed fraction of the frame the observation covers.  Doubles as the
    /// confidence value driving the "somebody just arrived" animation.
    pub confidence: f32,
    /// Smoothed centroid of the detected person, normalised 0..1.
    pub x: f32,
    pub y: f32,
    /// Apparent size of the observation relative to the frame.  Only refreshed
    /// from face detections -- see [`Smoother::push`].
    pub size: f32,
    /// Frame aspect ratio (width / height).  Needed to turn the horizontal field
    /// of view into the vertical one when reconstructing where a face is in 3D.
    pub aspect: f32,
}

struct Shared {
    present: AtomicBool,
    source: AtomicU32,
    confidence: AtomicU32,
    x: AtomicU32,
    y: AtomicU32,
    size: AtomicU32,
    aspect: AtomicU32,
    status: AtomicU32,
    frames: AtomicU64,
    device: Mutex<String>,
    /// Why the face detector is not in use, if it is not.
    face_error: Mutex<String>,
    /// Last camera bring-up failure, for the diagnostics log.
    camera_error: Mutex<String>,
}

pub struct Vision {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Vision {
    /// Lists `(index, human readable name)` pairs for the configuration dialog.
    pub fn list_devices() -> Vec<(u32, String)> {
        match nokhwa::query(ApiBackend::MediaFoundation) {
            Ok(list) => list
                .into_iter()
                .filter_map(|info| {
                    let idx = match info.index() {
                        CameraIndex::Index(i) => *i,
                        CameraIndex::String(_) => return None,
                    };
                    Some((idx, info.human_name().to_string()))
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn start(index: u32, threshold: f32) -> Vision {
        let shared = Arc::new(Shared {
            present: AtomicBool::new(false),
            source: AtomicU32::new(0),
            confidence: AtomicU32::new(0),
            x: AtomicU32::new(0.5f32.to_bits()),
            y: AtomicU32::new(0.5f32.to_bits()),
            size: AtomicU32::new(0.0f32.to_bits()),
            aspect: AtomicU32::new((16.0f32 / 9.0).to_bits()),
            status: AtomicU32::new(STATUS_STARTING),
            frames: AtomicU64::new(0),
            device: Mutex::new(String::new()),
            face_error: Mutex::new(String::new()),
            camera_error: Mutex::new(String::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("fairy-vision".into())
                .spawn(move || capture_loop(index, threshold, shared, stop))
                .ok()
        };

        Vision { shared, stop, thread }
    }

    pub fn reading(&self) -> Reading {
        let load = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        Reading {
            present: self.shared.present.load(Ordering::Relaxed),
            source: match self.shared.source.load(Ordering::Relaxed) {
                1 => Source::Face,
                2 => Source::Motion,
                _ => Source::None,
            },
            confidence: load(&self.shared.confidence),
            x: load(&self.shared.x),
            y: load(&self.shared.y),
            size: load(&self.shared.size),
            aspect: load(&self.shared.aspect),
        }
    }

    pub fn status(&self) -> u32 {
        self.shared.status.load(Ordering::Relaxed)
    }

    pub fn device_name(&self) -> String {
        self.shared.device.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Empty when the face detector is running; otherwise why it is not.
    pub fn face_error(&self) -> String {
        self.shared.face_error.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Empty when the camera is streaming; otherwise why it is not.
    pub fn camera_error(&self) -> String {
        self.shared.camera_error.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn frames(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }
}

impl Drop for Vision {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn capture_loop(index: u32, threshold: f32, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let mut smoother = Smoother::default();
    // Deliberately not built until the camera is up: creating the face detector
    // calls CoInitializeEx(MULTITHREADED) on this thread, and doing that first
    // makes Media Foundation's own apartment request fail -- which surfaces as
    // a camera that was working a moment ago now refusing to open.  The
    // diagnostics probe has always run in this order, which is why it works
    // there and did not here.
    let mut pipeline: Option<Pipeline> = None;

    while !stop.load(Ordering::Relaxed) {
        let camera = match Camera::new(
            CameraIndex::Index(index),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
        ) {
            Ok(c) => c,
            Err(e) => {
                set_note(&shared.camera_error, &format!("camera {index} could not be created: {e}"));
                shared.status.store(STATUS_ERROR, Ordering::Relaxed);
                sleep_cancellable(&stop, 3000);
                continue;
            }
        };

        let mut camera = camera;
        if let Ok(mut slot) = shared.device.lock() {
            *slot = camera.info().human_name();
        }

        if let Err(e) = camera.open_stream() {
            set_note(&shared.camera_error, &format!("camera {index} would not open: {e}"));
            shared.status.store(STATUS_ERROR, Ordering::Relaxed);
            sleep_cancellable(&stop, 3000);
            continue;
        }

        if let Ok(mut slot) = shared.camera_error.lock() {
            slot.clear();
        }
        let pipeline = pipeline.get_or_insert_with(Pipeline::new);
        if let Ok(mut slot) = shared.face_error.lock() {
            *slot = pipeline.face_error.clone().unwrap_or_default();
        }
        shared.status.store(STATUS_STREAMING, Ordering::Relaxed);

        while !stop.load(Ordering::Relaxed) {
            let frame = match camera.frame() {
                Ok(f) => f,
                Err(_) => break,
            };
            let Ok(image) = frame.decode_image::<RgbFormat>() else {
                continue;
            };

            let (w, h) = (image.width() as usize, image.height() as usize);
            let scene = pipeline.observe(image.as_raw(), w, h);
            let reading = smoother.push(scene, threshold);

            shared
                .source
                .store(reading.source as u32, Ordering::Relaxed);
            shared.confidence.store(reading.confidence.to_bits(), Ordering::Relaxed);
            shared.x.store(reading.x.to_bits(), Ordering::Relaxed);
            shared.y.store(reading.y.to_bits(), Ordering::Relaxed);
            shared.size.store(reading.size.to_bits(), Ordering::Relaxed);
            if h > 0 {
                shared
                    .aspect
                    .store((w as f32 / h as f32).to_bits(), Ordering::Relaxed);
            }
            shared.present.store(reading.present, Ordering::Relaxed);
            shared.frames.fetch_add(1, Ordering::Relaxed);

            if pipeline.face.is_none() {
                if let Ok(mut slot) = shared.face_error.lock() {
                    if slot.is_empty() {
                        *slot = pipeline
                            .face_error
                            .clone()
                            .unwrap_or_else(|| "unavailable".into());
                    }
                }
            }

            // Roughly 15 fps is plenty and keeps the saver cheap.
            sleep_cancellable(&stop, 60);
        }
    }
    shared.status.store(STATUS_STOPPED, Ordering::Relaxed);
}

/// Stores `text` unless a note of the same kind is already recorded, so the log
/// does not fill with one line per retry.
fn set_note(slot: &Mutex<String>, text: &str) {
    if let Ok(mut s) = slot.lock() {
        if s.is_empty() {
            *s = text.to_string();
        }
    }
}

fn sleep_cancellable(stop: &Arc<AtomicBool>, ms: u64) {
    let step = 25;
    let mut left = ms;
    while left > 0 && !stop.load(Ordering::Relaxed) {
        let chunk = left.min(step);
        std::thread::sleep(Duration::from_millis(chunk));
        left -= chunk;
    }
}

// ------------------------------------------------------------------ detection --

/// Below this much movement the frame is considered to contain nobody.
///
/// Tuned above the noise floor of a cheap sensor under changing light; the
/// motion detector already filters per-cell noise, so this only has to reject
/// the "a few cells twitched" case.
const MOTION_MIN_COVERAGE: f32 = 0.008;

/// How many consecutive face-detector failures mean "give up on it".  A single
/// error is usually a transient, and dropping straight to motion-only would
/// quietly downgrade the whole saver.
const MAX_FACE_FAILURES: u32 = 15;

/// One raw observation from a single frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scene {
    pub source: Source,
    /// Fraction of the frame covered by the observation.
    pub coverage: f32,
    /// Centroid of the observation, normalised 0..1.
    pub x: f32,
    pub y: f32,
    /// Bounding box, as a fraction of the frame.  `height` is the proximity
    /// signal: a face that fills a third of the frame is close.
    pub width: f32,
    pub height: f32,
    pub aspect: f32,
}

/// The full detection chain, owning both detectors.
struct Pipeline {
    face: Option<face::Detector>,
    motion: motion::Detector,
    face_failures: u32,
    face_error: Option<String>,
}

impl Pipeline {
    fn new() -> Self {
        let (face, face_error) = match face::Detector::new() {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e)),
        };
        Pipeline {
            face,
            motion: motion::Detector::new(),
            face_failures: 0,
            face_error,
        }
    }

    fn observe(&mut self, rgb: &[u8], w: usize, h: usize) -> Scene {
        let aspect = if h > 0 { w as f32 / h as f32 } else { 16.0 / 9.0 };

        // Fed every frame regardless of whether the result is used, so the
        // previous-frame buffer never goes stale.
        let moving = self.motion.update(rgb, w, h);

        if let Some(detector) = self.face.as_mut() {
            match detector.detect(rgb, w, h) {
                Ok(faces) => {
                    self.face_failures = 0;
                    // The largest face is the nearest person, which is the one
                    // worth looking at.  Area, not edge length: a box cut off by
                    // the frame edge is not actually a bigger face.
                    if let Some(best) = faces
                        .iter()
                        .max_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap_or(std::cmp::Ordering::Equal))
                    {
                        return Scene {
                            source: Source::Face,
                            coverage: best.w * best.h,
                            x: best.x,
                            y: best.y,
                            width: best.w,
                            height: best.h,
                            aspect,
                        };
                    }
                }
                Err(e) => {
                    self.face_failures += 1;
                    if self.face_failures >= MAX_FACE_FAILURES {
                        self.face_error = Some(format!("{e} (after {MAX_FACE_FAILURES} tries)"));
                        self.face = None;
                    }
                }
            }
        }

        if let Some(m) = moving {
            return Scene {
                source: Source::Motion,
                coverage: m.coverage,
                x: m.x,
                y: m.y,
                width: m.w,
                height: m.h,
                aspect,
            };
        }

        Scene {
            aspect,
            ..Scene::default()
        }
    }
}

/// Temporal filter: exponential smoothing on the centroid and size, plus a
/// debounce so a single noisy frame never registers as an arrival.
#[derive(Default)]
struct Smoother {
    initialized: bool,
    confidence: f32,
    x: f32,
    y: f32,
    size: f32,
    width: f32,
    aspect: f32,
    detected: bool,
}

impl Smoother {
    fn push(&mut self, scene: Scene, threshold: f32) -> Reading {
        if !self.initialized {
            self.x = 0.5;
            self.y = 0.5;
            self.aspect = 16.0 / 9.0;
            self.initialized = true;
        }
        if scene.aspect > 0.05 {
            self.aspect = scene.aspect;
        }

        let k = 0.25;
        self.confidence += (scene.coverage - self.confidence) * k;

        let found = match scene.source {
            Source::Face => scene.coverage >= threshold,
            Source::Motion => scene.coverage >= MOTION_MIN_COVERAGE,
            Source::None => false,
        };

        if found {
            self.x += (scene.x.clamp(0.0, 1.0) - self.x) * k;
            self.y += (scene.y.clamp(0.0, 1.0) - self.y) * k;
            // Only a face has a meaningful apparent size.  A waving arm covers
            // far more of the frame than a head does, so feeding motion into
            // the distance estimate would make the eye lunge at the screen
            // every time somebody gestured.  Keep the last face-derived value.
            if scene.source == Source::Face {
                self.size += (scene.height.clamp(0.0, 1.0) - self.size) * k;
                self.width += (scene.width.clamp(0.0, 1.0) - self.width) * k;
            }
            self.detected = true;
        } else {
            // Decay the size estimate instead of snapping it, so a momentary
            // dropout does not read as "the person teleported away".
            self.size += (0.0 - self.size) * (k * 0.5);
            self.width += (0.0 - self.width) * (k * 0.5);
            if scene.coverage <= threshold * 0.35 {
                self.detected = false;
            }
        }

        Reading {
            present: self.detected && found,
            source: if found { scene.source } else { Source::None },
            confidence: self.confidence,
            x: self.x,
            y: self.y,
            size: self.size,
            aspect: self.aspect,
        }
    }
}

// ----------------------------------------------------------------- diagnostics --

/// One-shot capture for the diagnostics dump.
///
/// Opens the camera, grabs a single frame, runs the whole detection chain over
/// it and writes an annotated BMP, then closes the camera again.  This is the
/// only way to answer "is the eye looking where the person actually is" without
/// a human in front of the lens, and it is deliberately independent of the
/// running capture thread so it works before the saver has ever started.
///
/// Returns a human-readable summary for the log; the image is the real output.
pub fn probe(index: u32, out_path: &std::path::Path) -> String {
    let camera = match Camera::new(
        CameraIndex::Index(index),
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
    ) {
        Ok(c) => c,
        Err(e) => return format!("camera {index} unavailable: {e}"),
    };
    let mut camera = camera;
    let name = camera.info().human_name();
    if let Err(e) = camera.open_stream() {
        return format!("camera {index} ({name}) would not open: {e}");
    }

    struct Shot {
        rgb: Vec<u8>,
        w: usize,
        h: usize,
        scene: Scene,
    }

    // The first frames after opening are usually dark or half-exposed, and some
    // UVC devices report "no frame yet" for a while before the first sample
    // arrives.  Neither is a reason to give up, so frames are retried rather
    // than treated as fatal.
    std::thread::sleep(Duration::from_millis(300));

    const WANTED_FRAMES: u32 = 14;
    /// Only the last few frames are kept: by then exposure has settled and the
    /// motion fallback has something to difference against.
    const KEEP_FROM: u32 = 6;
    const MAX_MISSES: u32 = 25;

    let mut pipeline = Pipeline::new();
    let mut shot: Option<Shot> = None;
    let mut taken = 0u32;
    let mut misses = 0u32;
    while taken < WANTED_FRAMES {
        let frame = match camera.frame() {
            Ok(f) => {
                misses = 0;
                f
            }
            Err(_) => {
                misses += 1;
                if misses > MAX_MISSES {
                    return format!(
                        "camera {index} ({name}) stopped delivering frames after {taken}"
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let Ok(image) = frame.decode_image::<RgbFormat>() else {
            continue;
        };
        let (w, h) = (image.width() as usize, image.height() as usize);
        let scene = pipeline.observe(image.as_raw(), w, h);
        taken += 1;
        if taken >= KEEP_FROM {
            shot = Some(Shot {
                rgb: image.as_raw().clone(),
                w,
                h,
                scene,
            });
        }
    }
    let Some(Shot { rgb, w, h, scene }) = shot else {
        return format!("camera {index} ({name}) produced no usable frames");
    };

    let written = annotate_and_save(rgb, w, h, scene, out_path);
    let face_note = pipeline.face_error.clone().unwrap_or_else(|| "ok".into());
    format!(
        "camera {index} ({name}) {w}x{h}: {} face_detector={face_note}; {written}",
        describe(scene),
    )
}

/// Runs the detection chain over an image file rather than a camera.
///
/// The verification counterpart to [`probe`], and the reason it exists: "is the
/// eye aiming at the right place" can be answered from a photograph, so the
/// chain can be checked, compared against a previous build, and regression
/// tested without anybody sitting in front of a lens.
///
/// A still image carries no motion, so the answer comes from the face detector
/// alone -- which is exactly right, since a photograph is not evidence that
/// somebody is moving.
pub fn probe_image(src: &std::path::Path, out: &std::path::Path) -> (String, Option<Scene>) {
    let (rgb, w, h) = match pixels::read_bmp(src) {
        Ok(v) => v,
        Err(e) => return (e, None),
    };
    let mut pipeline = Pipeline::new();
    let scene = pipeline.observe(&rgb, w, h);
    let written = annotate_and_save(rgb, w, h, scene, out);
    let face_note = pipeline.face_error.clone().unwrap_or_else(|| "ok".into());
    let summary = format!(
        "{} {w}x{h}: {} face_detector={face_note}; {written}",
        src.display(),
        describe(scene),
    );
    // The Scene goes back to the caller, which owns the desktop context and can
    // turn a bearing into the place the eye will actually look at.
    (summary, if scene.source == Source::None { None } else { Some(scene) })
}

/// One-line description of an observation, shared by both probes.
fn describe(scene: Scene) -> String {
    format!(
        "source={} centre=({:.3},{:.3}) box={}x{} coverage={:.3}",
        scene.source.label(),
        scene.x,
        scene.y,
        (scene.width * 1000.0).round(),
        (scene.height * 1000.0).round(),
        scene.coverage,
    )
}

/// Draws the result over the frame and saves it.  Green means a real face,
/// amber means the motion fallback, red means nothing was found.
fn annotate_and_save(
    rgb: Vec<u8>,
    w: usize,
    h: usize,
    scene: Scene,
    out: &std::path::Path,
) -> String {
    let mut annotated = rgb;
    let (x0, y0, x1, y1) = box_px(scene, w, h);
    let colour = match scene.source {
        Source::Face => [0, 220, 0],
        Source::Motion => [0, 200, 255],
        Source::None => [200, 0, 0],
    };
    pixels::draw_rect(&mut annotated, w, h, x0, y0, x1, y1, colour);
    // Crosshair at frame centre, so the bearing can be read off relative to it.
    let (cx, cy) = (w as i32 / 2, h as i32 / 2);
    pixels::draw_rect(&mut annotated, w, h, cx - 12, cy, cx + 12, cy, [255, 255, 255]);
    pixels::draw_rect(&mut annotated, w, h, cx, cy - 12, cx, cy + 12, [255, 255, 255]);

    match pixels::write_bmp(out, &annotated, w, h) {
        Ok(()) => format!("written to {}", out.display()),
        Err(e) => format!("could not write {}: {e}", out.display()),
    }
}

/// Turns a normalised bounding box into pixels, clamped to the frame.
fn box_px(scene: Scene, w: usize, h: usize) -> (i32, i32, i32, i32) {
    let x0 = ((scene.x - scene.width * 0.5) * w as f32).round() as i32;
    let y0 = ((scene.y - scene.height * 0.5) * h as f32).round() as i32;
    let x1 = ((scene.x + scene.width * 0.5) * w as f32).round() as i32;
    let y1 = ((scene.y + scene.height * 0.5) * h as f32).round() as i32;
    (
        x0.clamp(0, w as i32 - 1),
        y0.clamp(0, h as i32 - 1),
        x1.clamp(0, w as i32 - 1),
        y1.clamp(0, h as i32 - 1),
    )
}
