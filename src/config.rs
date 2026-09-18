//! Persistent configuration: a small hand-rolled INI file so the screen saver
//! has no runtime dependency beyond the standard library.
//!
//! Location: `%APPDATA%\FairyScreenSaver\config.ini`

use std::fmt;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MonitorMode {
    /// Only the primary monitor (the default).
    Primary,
    /// Every monitor attached to the desktop, as one continuous eye.
    All,
}

impl MonitorMode {
    fn as_str(self) -> &'static str {
        match self {
            MonitorMode::Primary => "primary",
            MonitorMode::All => "all",
        }
    }
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "every" | "所有" => MonitorMode::All,
            _ => MonitorMode::Primary,
        }
    }
}

/// How far outside its reference monitor a camera offset may stray.  One whole
/// monitor in either direction is enough for any seam or bezel gap, and it keeps
/// a corrupted config from throwing the camera off into nowhere.
pub const OFFSET_RANGE: (f32, f32) = (-1.5, 2.5);

#[derive(Clone, Debug)]
pub struct Config {
    // ---- display ---------------------------------------------------------
    pub monitor_mode: MonitorMode,
    /// Eye diameter as a percentage of the monitor height.  This is a screen
    /// saver, so the default is deliberately large -- it should fill the room,
    /// not sit politely in the middle of it.
    pub eye_scale_percent: f32,
    /// Global animation speed multiplier.
    pub animation_rate: f32,
    /// Allow the periodic fault / tearing glitches.
    pub glitch_enabled: bool,

    // ---- logging ---------------------------------------------------------
    /// Write `log.txt` alongside the config.
    ///
    /// Off by default.  This runs unattended for hours at a time, and having it
    /// touch the disk the whole while is not worth it unless somebody is
    /// actually chasing a problem.  The diagnostic switches (`/d`, `/f`) turn
    /// it on regardless -- for them, the log *is* the output.  A panic is also
    /// always recorded: it is the only evidence of why the saver vanished.
    pub log_enabled: bool,

    // ---- camera ----------------------------------------------------------
    pub camera_enabled: bool,
    /// Index of the capture device (as reported by the device query).
    pub camera_index: u32,
    /// Reference monitor for `camera_off_x/y`, or -1 for the primary monitor.
    ///
    /// The offsets are expressed in that monitor's own units, and they are *not*
    /// clamped to 0..1 on purpose: a webcam clipped to the seam between two
    /// stacked screens legitimately sits at, say, y = 1.04 -- just below the
    /// bottom edge of the reference monitor.
    pub camera_monitor: i32,
    /// Where the camera is, normalised to the reference monitor (0,0 = its
    /// top-left corner, 1,1 = its bottom-right).
    pub camera_off_x: f32,
    pub camera_off_y: f32,
    /// Horizontal field of view of the camera, degrees.
    pub camera_fov_deg: f32,
    /// Correction for a camera that is not mounted perpendicular to the screen.
    /// Positive yaw means the lens actually points to the right, positive pitch
    /// that it points upwards.  These exist because a webcam clipped to the top
    /// bezel of one screen in a stack is rarely aimed anywhere useful.
    pub camera_yaw_trim_deg: f32,
    pub camera_pitch_trim_deg: f32,
    /// Fallback distance from the camera to the viewer, millimetres.  Only used
    /// while the detector has no usable blob to measure.
    pub camera_distance_mm: f32,
    /// Come closer than this and the eye notices you.
    pub engage_distance_mm: f32,
    /// Move further away than this and the eye stops following.
    pub release_distance_mm: f32,
    /// How many desktop pixels one physical millimetre spans.
    pub px_per_mm: f32,
    /// Fraction of the frame that must be skin-toned before a person counts.
    pub presence_threshold: f32,
    /// Seconds without a detection before the eye gives up and goes home.
    pub lost_timeout_sec: f32,

    // ---- gaze ------------------------------------------------------------
    /// Let the eye move between monitors to keep facing the viewer.
    pub gaze_enabled: bool,
    /// Positional smoothing factor, 1.0 = default, higher = snappier.
    pub follow_speed: f32,

    // ---- preview / misc --------------------------------------------------
    pub preview_fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            monitor_mode: MonitorMode::Primary,
            eye_scale_percent: 68.0,
            animation_rate: 1.0,
            glitch_enabled: true,
            log_enabled: false,

            camera_enabled: false,
            camera_index: 0,
            camera_monitor: -1,
            camera_off_x: 0.5,
            camera_off_y: 0.0,
            camera_fov_deg: 60.0,
            camera_yaw_trim_deg: 0.0,
            camera_pitch_trim_deg: 0.0,
            camera_distance_mm: 700.0,
            engage_distance_mm: 900.0,
            release_distance_mm: 1400.0,
            px_per_mm: 3.78,
            presence_threshold: 0.012,
            lost_timeout_sec: 8.0,

            gaze_enabled: true,
            follow_speed: 1.0,

            preview_fps: 20,
        }
    }
}

impl fmt::Display for MonitorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MonitorMode::Primary => "primary",
            MonitorMode::All => "all",
        })
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| "C:\\".to_string());
        PathBuf::from(base).join("FairyScreenSaver")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.ini")
    }

    pub fn load() -> Self {
        let mut cfg = Config::default();
        let Ok(text) = fs::read_to_string(Self::path()) else {
            return cfg;
        };
        cfg.apply_ini(&text);
        cfg
    }

    pub fn save(&self) -> std::io::Result<()> {
        fs::create_dir_all(Self::dir())?;
        fs::write(Self::path(), self.to_ini())
    }

    fn apply_ini(&mut self, text: &str) {
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match k.as_str() {
                "monitor_mode" => self.monitor_mode = MonitorMode::parse(v),
                "eye_scale_percent" => set_f32(&mut self.eye_scale_percent, v, 8.0, 300.0),
                "animation_rate" => set_f32(&mut self.animation_rate, v, 0.1, 4.0),
                "glitch_enabled" => self.glitch_enabled = parse_bool(v, self.glitch_enabled),
                "log_enabled" => self.log_enabled = parse_bool(v, self.log_enabled),
                "camera_enabled" => self.camera_enabled = parse_bool(v, self.camera_enabled),
                "camera_index" => set_u32(&mut self.camera_index, v),
                "camera_monitor" => set_i32(&mut self.camera_monitor, v),
                "camera_off_x" => set_f32(&mut self.camera_off_x, v, OFFSET_RANGE.0, OFFSET_RANGE.1),
                "camera_off_y" => set_f32(&mut self.camera_off_y, v, OFFSET_RANGE.0, OFFSET_RANGE.1),
                "camera_fov_deg" => set_f32(&mut self.camera_fov_deg, v, 10.0, 170.0),
                "camera_yaw_trim_deg" => set_f32(&mut self.camera_yaw_trim_deg, v, -80.0, 80.0),
                "camera_pitch_trim_deg" => set_f32(&mut self.camera_pitch_trim_deg, v, -80.0, 80.0),
                "camera_distance_mm" => set_f32(&mut self.camera_distance_mm, v, 150.0, 5000.0),
                "engage_distance_mm" => set_f32(&mut self.engage_distance_mm, v, 150.0, 5000.0),
                "release_distance_mm" => set_f32(&mut self.release_distance_mm, v, 150.0, 5000.0),
                "px_per_mm" => set_f32(&mut self.px_per_mm, v, 0.2, 40.0),
                "presence_threshold" => set_f32(&mut self.presence_threshold, v, 0.0005, 0.5),
                "lost_timeout_sec" => set_f32(&mut self.lost_timeout_sec, v, 0.5, 600.0),
                "gaze_enabled" => self.gaze_enabled = parse_bool(v, self.gaze_enabled),
                "follow_speed" => set_f32(&mut self.follow_speed, v, 0.1, 8.0),
                "preview_fps" => set_u32(&mut self.preview_fps, v),
                _ => {}
            }
        }
        self.normalize();
    }

    /// Cross-field sanity that no single setter can enforce on its own.
    pub fn normalize(&mut self) {
        // Strictly greater: an equal pair would leave the state machine with no
        // hysteresis at all, which makes the eye flicker on every noisy frame.
        if self.release_distance_mm <= self.engage_distance_mm {
            self.release_distance_mm = self.engage_distance_mm * 1.4;
        }
    }

    fn to_ini(&self) -> String {
        format!(
            "\
; FairyScreenSaver configuration
; Eye diameter as a percentage of the monitor height.

[display]
monitor_mode={mode}
eye_scale_percent={eye:.2}
animation_rate={rate:.2}
glitch_enabled={glitch}

[camera]
camera_enabled={cam_on}
camera_index={cam_idx}
camera_monitor={cam_mon}
camera_off_x={offx:.3}
camera_off_y={offy:.3}
camera_fov_deg={fov:.1}
camera_yaw_trim_deg={yaw:.1}
camera_pitch_trim_deg={pitch:.1}
camera_distance_mm={dist:.0}
engage_distance_mm={engage:.0}
release_distance_mm={release:.0}
px_per_mm={ppmm:.3}
presence_threshold={thr:.4}
lost_timeout_sec={lost:.1}

[gaze]
gaze_enabled={gaze}
follow_speed={follow:.2}

[logging]
log_enabled={log}

[preview]
preview_fps={fps}
",
            mode = self.monitor_mode.as_str(),
            eye = self.eye_scale_percent,
            rate = self.animation_rate,
            glitch = self.glitch_enabled as u8,
            log = self.log_enabled as u8,
            cam_on = self.camera_enabled as u8,
            cam_idx = self.camera_index,
            cam_mon = self.camera_monitor,
            offx = self.camera_off_x,
            offy = self.camera_off_y,
            fov = self.camera_fov_deg,
            yaw = self.camera_yaw_trim_deg,
            pitch = self.camera_pitch_trim_deg,
            dist = self.camera_distance_mm,
            engage = self.engage_distance_mm,
            release = self.release_distance_mm,
            ppmm = self.px_per_mm,
            thr = self.presence_threshold,
            lost = self.lost_timeout_sec,
            gaze = self.gaze_enabled as u8,
            follow = self.follow_speed,
            fps = self.preview_fps,
        )
    }

    /// Eye diameter in pixels for a monitor of the given height.
    pub fn eye_size_px(&self, monitor_height: i32) -> f32 {
        (monitor_height as f32 * self.eye_scale_percent / 100.0).clamp(64.0, 100_000.0)
    }
}

fn parse_bool(v: &str, fallback: bool) -> bool {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => fallback,
    }
}

fn set_f32(slot: &mut f32, v: &str, lo: f32, hi: f32) {
    if let Ok(x) = v.trim().parse::<f32>() {
        if x.is_finite() {
            *slot = x.clamp(lo, hi);
        }
    }
}

fn set_u32(slot: &mut u32, v: &str) {
    if let Ok(x) = v.trim().parse::<u32>() {
        *slot = x;
    }
}

fn set_i32(slot: &mut i32, v: &str) {
    if let Ok(x) = v.trim().parse::<i32>() {
        *slot = x;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_offsets_outside_the_monitor_survive_a_round_trip() {
        let mut c = Config::default();
        c.camera_monitor = 1;
        c.camera_off_x = 0.5;
        c.camera_off_y = 1.05; // mounted in the seam below the reference screen

        let mut back = Config::default();
        back.apply_ini(&c.to_ini());
        assert_eq!(back.camera_monitor, 1);
        assert!((back.camera_off_x - 0.5).abs() < 0.001);
        assert!(
            (back.camera_off_y - 1.05).abs() < 0.001,
            "seam offsets must not be clamped back onto the screen: {}",
            back.camera_off_y
        );
    }

    #[test]
    fn release_distance_is_kept_above_engage_distance() {
        let mut c = Config::default();
        c.engage_distance_mm = 800.0;
        c.release_distance_mm = 800.0; // nonsensical: no hysteresis at all
        let text = c.to_ini();
        let mut back = Config::default();
        back.apply_ini(&text);
        assert!(
            back.release_distance_mm > back.engage_distance_mm,
            "parser must fix the pair itself: {} vs {}",
            back.release_distance_mm,
            back.engage_distance_mm
        );
    }
}
