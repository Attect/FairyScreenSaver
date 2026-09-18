//! Window management and the main render loops.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, MonitorFromWindow, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Config, MonitorMode};
use crate::diag;
use crate::gfx::{Globals, Gfx, Surface};
use crate::monitor::{self, Monitor};
use crate::sim::{FrameParams, World};
use crate::vision::Vision;

pub const CLASS_SAVER: &str = "FairyScreenSaverClass";
pub const CLASS_PREVIEW: &str = "FairyScreenSaverPreviewClass";

/// How long after a saver window appears before its input means anything.
///
/// Windows delivers the tail of whatever launched us -- the button release,
/// the keys that were already held -- to the window as it takes the
/// foreground.  None of that is somebody asking to come back.  Expressed as a
/// deadline rather than a flag because a flag needs something to clear it, and
/// the something that used to clear it no longer exists.
const INPUT_SETTLE: Duration = Duration::from_millis(400);

// --------------------------------------------------------------- utilities --

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn class_ptr(name: &str) -> Vec<u16> {
    wide(name)
}

struct WinState {
    screensaver: bool,
    ignore_input: bool,
    quit: Arc<AtomicBool>,
    /// When this window appeared.  Input arriving before `INPUT_SETTLE` has
    /// elapsed is the tail of whatever launched us, not the user.
    created: Instant,
}

static mut CLASS_REGISTERED: [bool; 2] = [false, false];

unsafe fn register_class(name: &str, index: usize) {
    if CLASS_REGISTERED[index] {
        return;
    }
    let class = class_ptr(name);
    let hinstance = GetModuleHandleW(std::ptr::null());
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        // The icon compiled in from assets/app.ico (resource id 1), so the
        // window and task bar match the screen saver picker.
        hIcon: LoadIconW(hinstance, 1 as *const u16),
        hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class.as_ptr(),
        hIconSm: LoadIconW(hinstance, 1 as *const u16),
    };
    RegisterClassExW(&wc);
    CLASS_REGISTERED[index] = true;
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;

    match msg {
        WM_NCCREATE => {
            let cs = lparam as *const CREATESTRUCTW;
            if !cs.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        WM_NCDESTROY => {
            if !state_ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state_ptr));
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        _ => {}
    }

    let state = if state_ptr.is_null() { None } else { Some(&mut *state_ptr) };

    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &mut ps);
            return 0;
        }
        WM_SETCURSOR => {
            if let Some(s) = state {
                if s.screensaver {
                    SetCursor(std::ptr::null_mut());
                    return 1;
                }
            }
        }
        WM_CLOSE => {
            if let Some(s) = state {
                s.quit.store(true, Ordering::Relaxed);
                if s.screensaver {
                    DestroyWindow(hwnd);
                    return 0;
                }
            }
            DestroyWindow(hwnd);
            return 0;
        }
        WM_DESTROY => return 0,
        // A click or a keypress dismisses the saver.  Pointer *movement* does
        // not, and that is deliberate.
        //
        // The usual screen saver convention is "any movement quits", but it is
        // unusable for something that exists to be watched: Windows delivers a
        // burst of `WM_MOUSEMOVE` messages of its own while raising a
        // fullscreen topmost window, and an optical mouse on a shared desk
        // picks up tremors all day.  The visible result is a saver that quits
        // itself a few seconds in with nobody having touched anything.  A click
        // or a key is unambiguous intent, and is what anyone reaches for.
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP | WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN
        | WM_SYSCHAR => {
            if let Some(s) = state {
                if s.screensaver && !s.ignore_input {
                    // `WM_SYSCOMMAND` is deliberately not in this list: that is
                    // the system issuing commands (SC_SCREENSAVE, monitor
                    // power), not the user asking to come back.
                    if s.created.elapsed() < INPUT_SETTLE {
                        return 0;
                    }
                    diag::log(&format!("exit: input (msg {msg:#06x})"));
                    s.quit.store(true, Ordering::Relaxed);
                    PostQuitMessage(0);
                    return 0;
                }
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Raises `hwnd` and gives it the keyboard focus.
///
/// `SetForegroundWindow` only succeeds for a process that already has the right
/// to it, and a saver started by hand from Explorer does not have it.  The
/// window still goes fullscreen and topmost, so everything *looks* right while
/// `WM_KEYDOWN` never arrives -- a silent failure that reads as "the keyboard
/// does nothing".  Attaching to the current foreground thread for the duration
/// of the call is the standard way around the restriction and needs no faked
/// input.
unsafe fn take_foreground(hwnd: HWND) {
    let foreground = GetForegroundWindow();
    let target = if foreground.is_null() {
        0
    } else {
        GetWindowThreadProcessId(foreground, std::ptr::null_mut())
    };
    let own = GetCurrentThreadId();
    let attached = target != 0 && target != own && AttachThreadInput(own, target, 1) != 0;

    let raised = SetForegroundWindow(hwnd);
    SetFocus(hwnd);
    BringWindowToTop(hwnd);

    if attached {
        AttachThreadInput(own, target, 0);
    }

    // Worth recording: a window that never took the foreground still looks
    // right on screen, and the only symptom is that keys do not arrive.
    // Input is still caught by the polled check either way.
    diag::log(&format!("  foreground: attached={attached} raised={raised}"));
}

/// Creates the fullscreen, topmost, input-eating window for one monitor.
unsafe fn create_saver_window(monitor: &Monitor, quit: Arc<AtomicBool>, ignore_input: bool) -> Option<HWND> {
    register_class(CLASS_SAVER, 0);
    let class = class_ptr(CLASS_SAVER);
    let title = wide("FairyScreenSaver");
    let hinstance = GetModuleHandleW(std::ptr::null());
    let state = Box::into_raw(Box::new(WinState {
        screensaver: true,
        ignore_input: ignore_input,
        quit,
        created: Instant::now(),
    }));

    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        class.as_ptr(),
        title.as_ptr(),
        WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN,
        monitor.left,
        monitor.top,
        monitor.width(),
        monitor.height(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinstance,
        state as *const std::ffi::c_void,
    );
    if hwnd.is_null() {
        drop(Box::from_raw(state));
        return None;
    }
    SetWindowPos(hwnd, HWND_TOPMOST, monitor.left, monitor.top, monitor.width(), monitor.height(), SWP_SHOWWINDOW);
    ShowWindow(hwnd, SW_SHOW);
    take_foreground(hwnd);
    diag::log(&format!(
        "  window {} rect=({},{},{},{}) visible={}",
        monitor.index,
        monitor.left, monitor.top, monitor.width(), monitor.height(),
        IsWindowVisible(hwnd)
    ));
    Some(hwnd)
}

/// Creates the child window the Windows screen saver settings page embeds.
unsafe fn create_preview_window(parent: HWND, quit: Arc<AtomicBool>) -> Option<HWND> {
    register_class(CLASS_PREVIEW, 1);
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetClientRect(parent, &mut rect);
    let w = (rect.right - rect.left).max(80);
    let h = (rect.bottom - rect.top).max(60);

    let class = class_ptr(CLASS_PREVIEW);
    let title = wide("");
    let hinstance = GetModuleHandleW(std::ptr::null());
    let state = Box::into_raw(Box::new(WinState {
        screensaver: false,
        ignore_input: false,
        quit,
        created: Instant::now(),
    }));

    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        title.as_ptr(),
        WS_CHILD | WS_VISIBLE,
        0,
        0,
        w,
        h,
        parent,
        std::ptr::null_mut(),
        hinstance,
        state as *const std::ffi::c_void,
    );
    if hwnd.is_null() {
        drop(Box::from_raw(state));
        return None;
    }
    Some(hwnd)
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetClientRect(hwnd, &mut rect) };
    (
        (rect.right - rect.left).max(1) as u32,
        (rect.bottom - rect.top).max(1) as u32,
    )
}

fn pump_messages() -> bool {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_QUIT {
                return false;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

// -------------------------------------------------------------- parameters --

fn build_globals(params: &FrameParams, world: &World, monitor: &Monitor, size: (u32, u32)) -> Globals {
    let mut g = Globals::default();
    g.resolution = [size.0 as f32, size.1 as f32];
    g.time = world.t;
    g.eye_scale_px = params.eye_scale_px;
    // eye_pos is the centre of the eyeball; the shader works in a 160x160
    // space whose origin sits half a diameter up and to the left of it.
    let half = params.eye_scale_px * 80.0;
    g.view_origin = [
        world.eye_pos.0 - monitor.left as f32 - half,
        world.eye_pos.1 - monitor.top as f32 - half,
    ];
    g.lash_angle = params.lash_angle;
    g.lid_center_y = params.lid_center_y;
    g.lid_curve = params.lid_curve;
    g.sclera_scale = params.sclera;
    g.l3_scale = params.l3;
    g.l2_scale = params.l2;
    g.l1_scale = params.l1;
    g.flicker = params.flicker;
    g.gaze = [params.gaze.0, params.gaze.1];
    g.pulse_phase = params.pulse_phase;
    g.pulse_cycle = params.pulse_cycle;
    g.glitch_mode = params.glitch_mode;
    g.glitch_amp = params.glitch_amp;
    g.bright = params.bright;
    g.contrast = params.contrast;
    g.skew = params.skew;
    g.gx = params.gx;
    g.slice_offsets = [
        params.slice_offsets[0],
        params.slice_offsets[1],
        params.slice_offsets[2],
        params.slice_offsets[3],
    ];
    g.slice_offset5 = params.slice_offsets[4];
    g.glitch_seed = params.glitch_seed;
    g.slice_edges = params.slice_edges;
    g.eye_opacity = params.opacity;
    g
}

// ------------------------------------------------------------------- setup --

struct WindowEntry {
    hwnd: HWND,
    monitor: Monitor,
    surface: Surface,
    size: (u32, u32),
}

fn targets_for(cfg: &Config, monitors: &[Monitor]) -> Vec<Monitor> {
    match cfg.monitor_mode {
        MonitorMode::Primary => monitors
            .iter()
            .filter(|m| m.primary)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .chain(std::iter::empty())
            .collect(),
        MonitorMode::All => monitors.to_vec(),
    }
}

/// Spawns the camera when configured, tolerating a missing device.
fn spawn_vision(cfg: &Config) -> Option<Vision> {
    if !cfg.camera_enabled {
        return None;
    }
    let v = Vision::start(cfg.camera_index, cfg.presence_threshold);
    diag::log(&format!("camera: starting device #{}", cfg.camera_index));
    Some(v)
}

// ------------------------------------------------------------- main loops --

/// `/s` — the actual screen saver.
pub fn run_screensaver(cfg: Config) -> i32 {
    let monitors = monitor::enumerate();
    diag::log(&format!(
        "monitors: {}",
        monitors
            .iter()
            .map(|m| format!(
                "#{} {}x{}@{},{} primary={}",
                m.index, m.width(), m.height(), m.left, m.top, m.primary
            ))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    if monitors.is_empty() {
        diag::log("no monitors found");
        return 1;
    }
    let targets = {
        let t = targets_for(&cfg, &monitors);
        if t.is_empty() {
            monitors[..1].to_vec()
        } else {
            t
        }
    };

    let quit = Arc::new(AtomicBool::new(false));

    // Diagnostics hook: a bounded lifetime is what makes the render capturable.
    let autoclose = std::env::var("FAIRY_AUTOCLOSE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_millis);

    // Automated verification hook.  Only honoured together with a bounded
    // lifetime, so it can never turn the saver into an input-eating lock screen.
    let ignore_input = autoclose.is_some() && std::env::var_os("FAIRY_IGNORE_INPUT").is_some();
    if ignore_input {
        diag::log("input handling suspended for this run");
    }

    // ---- windows ---------------------------------------------------------
    let hwnds: Vec<(HWND, Monitor)> = unsafe {
        targets
            .iter()
            .filter_map(|m| {
                create_saver_window(m, quit.clone(), ignore_input).map(|h| (h, m.clone()))
            })
            .collect()
    };
    if hwnds.is_empty() {
        diag::log("failed to create any window");
        return 1;
    }

    // ---- Vulkan ----------------------------------------------------------
    let mut gfx = match unsafe { Gfx::new() } {
        Ok(g) => g,
        Err(e) => {
            diag::log(&format!("vulkan init failed: {e}"));
            return 1;
        }
    };

    let mut windows = match unsafe { build_surfaces(&mut gfx, &hwnds) } {
        Ok(w) => w,
        Err(e) => {
            diag::log(&format!("surface setup failed: {e}"));
            unsafe { gfx.destroy() };
            return 1;
        }
    };

    diag::log(&format!(
        "screensaver ready: {} window(s), gpu={}, mode={}",
        windows.len(),
        unsafe { gfx.device_name() },
        cfg.monitor_mode
    ));

    let mut world = World::new(cfg.clone(), monitors.clone());
    let vision = spawn_vision(&cfg);

    let started = Instant::now();
    let mut last_vision_log = Instant::now();
    let mut last = Instant::now();


    let mut frame_no: u64 = 0;
    let result = loop {
        if !pump_messages() {
            diag::log("exit: WM_QUIT");
            break 0;
        }
        if quit.load(Ordering::Relaxed) {
            diag::log("exit: input detected");
            break 0;
        }
        if let Some(limit) = autoclose {
            if started.elapsed() >= limit {
                diag::log(&format!("exit: autoclose after {frame_no} frames"));
                break 0;
            }
        }

        // Backstop input check: catches activity on monitors this instance does
        // not own, and cases where focus never reached us.
        // Backstop: catches input that landed on a monitor this instance does
        // not own, or before focus ever reached the window.  The pointer is
        // deliberately not consulted here -- see the window procedure.
        if !ignore_input {
            unsafe {
                // `GetAsyncKeyState` reads the physical key state and does not
                // care about focus, which makes this the reliable half of the
                // check -- see `take_foreground`.  Scanning the whole virtual
                // key range rather than a short list keeps a key from being
                // invisible just because nobody thought to list it.  The range
                // starts at 1 because 0 is not a valid virtual key, and stops
                // short of 0xFF for the same reason.
                let mut pressed = false;
                for vk in 0x01i32..0xFE {
                    if (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 {
                        pressed = true;
                        break;
                    }
                }
                if started.elapsed() >= INPUT_SETTLE && pressed {
                    diag::log("exit: input [polled]");
                    break 0;
                }
            }
        }

        let now = Instant::now();
        let dt = (now - last).as_secs_f32().clamp(0.0, 0.1);
        last = now;

        let reading = vision.as_ref().map(|v| v.reading());
        world.update(dt, reading);
        let params = world.params();

        for entry in windows.iter_mut() {
            let size = client_size(entry.hwnd);
            if size != entry.size {
                let extent = ash::vk::Extent2D { width: size.0, height: size.1 };
                if unsafe { entry.surface.resize(&gfx, extent) }.is_ok() {
                    entry.size = size;
                }
            }
            let g = build_globals(&params, &world, &entry.monitor, entry.size);
            match unsafe { entry.surface.draw(&gfx, &g) } {
                Ok(true) => {}
                Ok(false) => {
                    let extent = ash::vk::Extent2D {
                        width: entry.size.0,
                        height: entry.size.1,
                    };
                    let _ = unsafe { entry.surface.resize(&gfx, extent) };
                }
                Err(e) => {
                    diag::log(&format!("draw error: {e}"));
                }
            }
        }
        frame_no += 1;
        if vision.as_ref().map(|v| v.status()).is_some() && last_vision_log.elapsed().as_secs() >= 3 {
            if let Some(v) = vision.as_ref() {
                diag::log(&format!(
                    "vision: status={} frames={} device={:?} present={} src={} conf={:.4} at=({:.3},{:.3}) | {}",
                    v.status(), v.frames(), v.device_name(),
                    reading.map(|r| r.present).unwrap_or(false),
                    reading.map(|r| r.source.label()).unwrap_or("none"),
                    reading.map(|r| r.confidence).unwrap_or(0.0),
                    reading.map(|r| r.x).unwrap_or(0.0),
                    reading.map(|r| r.y).unwrap_or(0.0),
                    world.gaze_debug(),
                ));
                let camera_err = v.camera_error();
                if !camera_err.is_empty() {
                    diag::log(&format!("  {camera_err}"));
                }
                let err = v.face_error();
                if !err.is_empty() {
                    diag::log(&format!("  face detector unavailable, using motion only: {err}"));
                }
            }
            last_vision_log = Instant::now();
        }
        if frame_no <= 2 || frame_no % 900 == 0 {
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let visible = unsafe { IsWindowVisible(windows[0].hwnd) };
            unsafe { GetWindowRect(windows[0].hwnd, &mut r) };
            diag::log(&format!(
                "frame {frame_no}: eye=({:.0},{:.0}) size={:.0} visible={} rect=({},{},{},{}) state={}",
                world.eye_pos.0, world.eye_pos.1, params.eye_size_px,
                visible,
                r.left, r.top, r.right, r.bottom,
                world.status_line()
            ));
        }
    };

    drop(vision);
    unsafe {
        for entry in windows.iter_mut() {
            entry.surface.destroy(&gfx);
            DestroyWindow(entry.hwnd);
        }
        gfx.destroy();
    }
    result
}

/// `/d` — diagnostic dump.  Everything a support request needs, written to
/// the log file instead of stdout (a screen saver has no console).
pub fn dump_diagnostics() -> i32 {
    diag::log("=== FairyScreenSaver diagnostics ===");
    let monitors = monitor::enumerate();
    diag::log(&format!("monitors: {}", monitors.len()));
    for m in &monitors {
        diag::log(&format!(
            "  [{}] {} primary={} rect=({},{},{},{}) {}x{}",
            m.index, m.device, m.primary, m.left, m.top, m.right, m.bottom,
            m.width(), m.height()
        ));
    }
    match unsafe { Gfx::new() } {
        Ok(gfx) => {
            let names = unsafe { gfx.vulkan_devices() };
            diag::log(&format!("vulkan: {} adapter(s)", names.len()));
            for n in names {
                diag::log(&format!("  {n}"));
            }
        }
        Err(e) => diag::log(&format!("vulkan: unavailable ({e})")),
    }
    let devices = Vision::list_devices();
    diag::log(&format!("cameras: {}", devices.len()));
    for (i, (idx, name)) in devices.iter().enumerate() {
        diag::log(&format!("  [{i}] index={idx} {name}"));
    }
    // Grab one frame per camera with the detection drawn on top.  "Is the eye
    // looking at the right place" is not a question a text log can answer, and
    // this is the only way for the user to check the aim without first working
    // out where the detected bearing came from.
    for (i, (idx, name)) in devices.iter().enumerate() {
        let out = Config::path().with_file_name(format!("camera-probe-{i}.bmp"));
        diag::log(&format!("camera probe [{i}] {name}:"));
        diag::log(&format!("  {}", crate::vision::probe(*idx, &out)));
    }
    diag::log(&format!("config: {}", Config::path().display()));
    diag::log("=== end ===");
    0
}

/// `/p <hwnd>` — the little animated thumbnail in the settings page.
pub fn run_preview(cfg: Config, parent: HWND) -> i32 {
    if parent.is_null() {
        return 1;
    }
    let quit = Arc::new(AtomicBool::new(false));
    let Some(hwnd) = (unsafe { create_preview_window(parent, quit.clone()) }) else {
        return 1;
    };

    let mut gfx = match unsafe { Gfx::new() } {
        Ok(g) => g,
        Err(e) => {
            diag::log(&format!("preview vulkan init failed: {e}"));
            unsafe { DestroyWindow(hwnd) };
            return 1;
        }
    };

    let monitors = {
        let m = monitor::enumerate();
        if m.is_empty() {
            vec![monitor::fallback()]
        } else {
            m
        }
    };
    // The preview is a single window; it renders as if it were the monitor it
    // happens to sit on.
    let mon = unsafe {
        let h = MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST);
        monitors
            .iter()
            .find(|m| m.handle == h)
            .cloned()
            .unwrap_or_else(|| monitors[0].clone())
    };

    let size = client_size(hwnd);
    let Some(mut surface) = (unsafe { build_one_surface(&mut gfx, hwnd, size) }) else {
        unsafe {
            gfx.destroy();
            DestroyWindow(hwnd);
        }
        return 1;
    };

    let mut world = World::new(cfg.clone(), monitors);
    // A preview is decorative: no camera, and a smaller eye so it fits.
    let mut preview_cfg = cfg;
    preview_cfg.animation_rate = 1.0;
    world.set_glitch_enabled(preview_cfg.glitch_enabled);

    let frame_budget = std::time::Duration::from_millis(1000 / preview_cfg.preview_fps.clamp(5, 60) as u64);
    let mut last = Instant::now();
    let mut next_frame = Instant::now();

    let result = loop {
        if !pump_messages() {
            diag::log("exit: WM_QUIT");
            break 0;
        }
        if quit.load(Ordering::Relaxed) {
            diag::log("exit: input detected");
            break 0;
        }
        if unsafe { IsWindow(parent) } == 0 {
            break 0;
        }

        let now = Instant::now();
        let dt = (now - last).as_secs_f32().clamp(0.0, 0.1);
        last = now;
        world.update(dt, None);

        let size = client_size(hwnd);
        if size != surface_size(&surface) {
            let extent = ash::vk::Extent2D { width: size.0, height: size.1 };
            let _ = unsafe { surface.resize(&gfx, extent) };
        }
        // The preview is a thumbnail: scale the eye to the widget, not to the
        // monitor the settings window happens to be sitting on.
        world.set_eye_size_px(preview_cfg.eye_size_px(size.1 as i32));
        // `build_globals` places the eye at `world.eye_pos` expressed in the
        // given monitor's coordinates.  For the saver that is exactly right --
        // each window covers one whole screen.  Here it is not: the eye's home
        // is the *desktop* centre, which lands far outside a 150x110 thumbnail,
        // so the preview drew nothing but background.  Handing over a monitor
        // whose origin puts `eye_pos` at the centre of the client area fixes it
        // without teaching the renderer that previews exist.
        let mut preview_mon = mon.clone();
        preview_mon.left = (world.eye_pos.0 - size.0 as f32 * 0.5).round() as i32;
        preview_mon.top = (world.eye_pos.1 - size.1 as f32 * 0.5).round() as i32;
        preview_mon.right = preview_mon.left + size.0 as i32;
        preview_mon.bottom = preview_mon.top + size.1 as i32;
        let g = build_globals(&world.params(), &world, &preview_mon, size);
        match unsafe { surface.draw(&gfx, &g) } {
            Ok(_) => {}
            Err(e) => diag::log(&format!("preview draw error: {e}")),
        }

        next_frame += frame_budget;
        let now = Instant::now();
        if next_frame > now {
            std::thread::sleep((next_frame - now).min(frame_budget));
        } else {
            next_frame = now;
        }
    };

    unsafe {
        surface.destroy(&gfx);
        gfx.destroy();
        DestroyWindow(hwnd);
    }
    result
}

fn surface_size(s: &Surface) -> (u32, u32) {
    (s.extent.width, s.extent.height)
}

unsafe fn build_one_surface(gfx: &mut Gfx, hwnd: HWND, size: (u32, u32)) -> Option<Surface> {
    let raw = hwnd as isize;
    let surface = gfx.create_surface(raw, GetModuleHandleW(std::ptr::null()) as isize).ok()?;
    if gfx.pdev == ash::vk::PhysicalDevice::null() {
        gfx.select_device(surface).ok()?;
    }
    let format = Surface::preferred_format(gfx, surface).ok()?;
    gfx.init_pipeline(format).ok()?;
    let extent = ash::vk::Extent2D { width: size.0, height: size.1 };
    match Surface::new(gfx, surface, format, extent) {
        Ok(s) => Some(s),
        Err(e) => {
            diag::log(&format!("surface creation failed: {e}"));
            gfx.destroy_surface(surface);
            None
        }
    }
}

unsafe fn build_surfaces(gfx: &mut Gfx, hwnds: &[(HWND, Monitor)]) -> Result<Vec<WindowEntry>, String> {
    let mut out = Vec::with_capacity(hwnds.len());
    let mut format = None;

    for (hwnd, mon) in hwnds {
        let raw = *hwnd as isize;
        let surface = gfx.create_surface(raw, GetModuleHandleW(std::ptr::null()) as isize)?;
        if format.is_none() {
            // The surface is what lets us pick a device that can actually
            // present to these windows.
            gfx.select_device(surface)?;
        }
        let f = Surface::preferred_format(gfx, surface)?;
        if format.is_none() {
            format = Some(f);
                gfx.init_pipeline(f)?;
        }
        let size = client_size(*hwnd);
        let extent = ash::vk::Extent2D { width: size.0, height: size.1 };
        let s = Surface::new(gfx, surface, f, extent)?;
        out.push(WindowEntry {
            hwnd: *hwnd,
            monitor: mon.clone(),
            surface: s,
            size,
        });
    }
    Ok(out)
}
