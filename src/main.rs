//! Fairy screen saver — entry point.
//!
//! Windows invokes a screen saver (a plain executable renamed to `.scr`) with
//! one of four switches:
//!
//! * `/s`          run the saver full screen
//! * `/c` `/c:HWND` show the configuration UI, optionally owned by `HWND`
//! * `/p HWND`      render a live preview inside the settings page thumbnail
//! * `/a HWND`      change the password (obsolete, ignored)
//!
//! Running with no switch at all also opens the configuration UI, which is what
//! double-clicking the `.scr` file does.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod diag;
mod dialog;
mod face;
mod gfx;
mod monitor;
mod motion;
mod pixels;
mod sim;
mod vision;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    // Without this the saver would be bitmap-stretched on scaled displays and
    // would only ever see the primary monitor's logical resolution.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    std::panic::set_hook(Box::new(|info| {
        // A crash is worth one line even with logging switched off: it is the
        // only record of why the saver disappeared, and it happens once.
        diag::set_enabled(true);
        diag::log(&format!("PANIC: {info}"));
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();
    diag::log(&format!("startup: argv={args:?}"));

    let cfg = config::Config::load();
    let (switch, param) = parse_switch(&args);

    // The diagnostic switches exist to produce a log, so they always write one.
    // Everything else follows the config, which is silent by default.
    diag::set_enabled(cfg.log_enabled || matches!(switch, Some('d') | Some('f')));

    let code = match switch {
        Some('s') => app::run_screensaver(cfg),
        Some('c') => {
            let mut cfg = cfg;
            dialog::run(&mut cfg, param as HWND);
            0
        }
        Some('p') => app::run_preview(cfg, param as HWND),
        Some('a') => 0,
        Some('d') => app::dump_diagnostics(),
        Some('f') => probe_image(&args),
        _ => {
            let mut cfg = cfg;
            dialog::run(&mut cfg, std::ptr::null_mut());
            0
        }
    };

    std::process::exit(code);
}

/// `/f <image.bmp> [out.bmp]` -- runs the detection chain over a still image
/// and writes the result back out with the answer drawn on it.
///
/// Answers "is the eye aiming at the right place" from a photograph, so camera
/// placement can be checked, and detector changes compared, without needing
/// anybody to sit in front of a lens.
fn probe_image(args: &[String]) -> i32 {
    let files: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('/') && !a.starts_with('-'))
        .collect();

    let Some(src) = files.first() else {
        diag::log("usage: FairyScreenSaver /f <image.bmp> [out.bmp]");
        return 1;
    };
    let src = std::path::PathBuf::from(src);
    let out = files
        .get(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| src.with_extension("annotated.bmp"));

    let (summary, scene) = vision::probe_image(&src, &out);
    diag::log(&format!("image probe: {summary}"));
    println!("{summary}");
    if let Some(scene) = scene {
        let aim = aim_report(&scene);
        diag::log(&format!("  aim: {aim}"));
        println!("  aim: {aim}");
    }
    0
}

/// Where the eye would end up looking, given what the detector found.
///
/// This is the end-to-end answer to "is the bearing right", and the detector's
/// own output cannot give it on its own: the same face position means a
/// different place on the desktop depending on where the camera sits, how wide
/// the lens is, and how the mount is trimmed.  Printing the resolved point --
/// and which screen it lands on -- is what makes a wrong bearing attributable
/// to a specific setting instead of to "the detection is bad".
fn aim_report(scene: &vision::Scene) -> String {
    let cfg = config::Config::load();
    let monitors = monitor::enumerate();
    if monitors.is_empty() {
        return "no monitors".into();
    }

    let reading = vision::Reading {
        present: true,
        source: scene.source,
        confidence: scene.coverage,
        x: scene.x,
        y: scene.y,
        size: scene.height,
        aspect: scene.aspect,
    };
    let distance = sim::estimate_distance(&cfg, reading);
    let cam_mon = sim::camera_monitor_index(&cfg, &monitors);
    let (vx, vy, _) = sim::project_viewer(&cfg, &monitors, cam_mon, reading, distance);

    // Back into desktop pixels, then find the screen that contains the point --
    // or, failing that, the nearest one, since a viewer can stand in a seam.
    let ppm = cfg.px_per_mm.max(0.01);
    let (dx, dy) = (vx * ppm, vy * ppm);
    let host = monitors
        .iter()
        .position(|m| {
            dx >= m.left as f32 && dx < m.right as f32 && dy >= m.top as f32 && dy < m.bottom as f32
        })
        .unwrap_or_else(|| {
            let nearest = monitors
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = centre_distance(a, dx, dy);
                    let db = centre_distance(b, dx, dy);
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            nearest
        });

    format!(
        "distance≈{distance:.0}mm desktop=({dx:.0},{dy:.0})px screen=#{}",
        monitors[host].index
    )
}

/// Squared distance from a desktop point to the middle of a monitor.
fn centre_distance(m: &monitor::Monitor, x: f32, y: f32) -> f32 {
    let cx = (m.left + m.right) as f32 * 0.5;
    let cy = (m.top + m.bottom) as f32 * 0.5;
    (x - cx).powi(2) + (y - cy).powi(2)
}

/// Accepts `/s`, `-s`, `/c:1234`, `/c 1234`, `/p1234` and friends.
fn parse_switch(args: &[String]) -> (Option<char>, isize) {
    for (i, arg) in args.iter().enumerate() {
        let trimmed = arg.trim_start_matches(['/', '-']).trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut chars = trimmed.chars();
        let Some(letter) = chars.next() else { continue };
        let letter = letter.to_ascii_lowercase();
        let rest: String = chars.collect();
        let rest = rest.trim_start_matches([':', '=']).trim();

        if matches!(letter, 'c' | 'p' | 'a') {
            if let Ok(n) = rest.parse::<isize>() {
                return (Some(letter), n);
            }
            if let Some(next) = args.get(i + 1) {
                if let Ok(n) = next.trim().parse::<isize>() {
                    return (Some(letter), n);
                }
            }
        }
        return (Some(letter), 0);
    }
    (None, 0)
}
