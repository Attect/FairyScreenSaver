//! The configuration UI (`/c`), built entirely from code so the project needs
//! no message compiler and no `.rc` resource.

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, FillRect,
    GetDC, GetStockObject, InvalidateRect, LineTo, MapWindowPoints, MoveToEx, Rectangle, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, TextOutW, HFONT, HGDIOBJ, PAINTSTRUCT, PS_SOLID,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::SetScrollInfo;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetDpiForSystem};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Config, MonitorMode};
use crate::diag;
use crate::monitor::{self, Monitor};
use crate::sim;
use crate::vision::{Reading, Vision};

// ---- message / control constants (kept local so we do not depend on the exact
// ---- set of names exported by the bindings crate) ---------------------------

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_COMMAND: u32 = 0x0111;
const WM_SETFONT: u32 = 0x0030;
const WM_CTLCOLORSTATIC: u32 = 0x0138;
const WM_CTLCOLORBTN: u32 = 0x0135;
const WM_NCCREATE: u32 = 0x0081;
const WM_NCDESTROY: u32 = 0x0082;
const WM_ERASEBKGND: u32 = 0x0014;

const CB_ADDSTRING: u32 = 0x0143;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const BM_SETCHECK: u32 = 0x00F1;
const BM_GETCHECK: u32 = 0x00F0;
const BST_CHECKED: usize = 1;
const BST_UNCHECKED: usize = 0;

const WS_CAPTION_SYSMENU: u32 = 0x00C0_0000 | 0x0008_0000 | 0x0002_0000;
const WS_CHILD_VISIBLE: u32 = 0x4000_0000 | 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_EX_CONTROLPARENT: u32 = 0x0001_0000;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SPI_GETWORKAREA: u32 = 0x0030;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;

const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const BS_PUSHBUTTON: u32 = 0x0000;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const ES_AUTOHSCROLL: u32 = 0x0080;

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_DEFAULTS: i32 = 100;
const ID_MONITOR_MODE: i32 = 110;
const ID_EYE: i32 = 111;
const ID_RATE: i32 = 112;
const ID_GLITCH: i32 = 113;
const ID_GAZE: i32 = 114;
const ID_CAM_ON: i32 = 120;
const ID_CAM_DEV: i32 = 121;
const ID_CAM_MON: i32 = 122;
const ID_OFFX: i32 = 123;
const ID_OFFY: i32 = 124;
const ID_FOV: i32 = 125;
const ID_DIST: i32 = 126;
const ID_PPMM: i32 = 127;
const ID_THR: i32 = 128;
const ID_LOST: i32 = 129;
const ID_ENGAGE: i32 = 130;
const ID_RELEASE: i32 = 131;
const ID_LOG_ON: i32 = 132;
const ID_YAW: i32 = 132;
const ID_PITCH: i32 = 133;

const CLASS_CFG: &str = "FairyScreenSaverConfigClass";
const CLASS_MAP: &str = "FairyScreenSaverMonitorMapClass";

const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_SETCURSOR: u32 = 0x0020;
const EN_CHANGE: u32 = 0x0300;
const WM_TIMER: u32 = 0x0113;
const WM_VSCROLL: u32 = 0x0115;
const WM_MOUSEWHEEL: u32 = 0x020A;
const TIMER_LIVE: usize = 1;
/// Repaint cadence of the live preview, milliseconds.
const LIVE_INTERVAL_MS: u32 = 120;

/// Full logical height of the settings content at 96 DPI.
const CONTENT_H: i32 = 756;
const CONTENT_W: i32 = 700;
/// Margin left between the window frame and the edges of the work area.
const SCREEN_MARGIN: i32 = 24;
/// One scroll notch, device pixels.
const SCROLL_STEP: i32 = 28;
const CBN_SELCHANGE: u32 = 1;
const IDC_HAND: usize = 32649;
const NULL_BRUSH: i32 = 5;

/// 0x00BBGGRR
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

// The settings page follows the same flat, light scheme as the rest of the
// project: a near-white sheet, quiet greys, and one blue accent.
const COLOR_BG: u32 = rgb(0xF5, 0xF5, 0xF7);
const COLOR_TITLE: u32 = rgb(0x86, 0x86, 0x8B); // section labels: quiet grey
const COLOR_LABEL: u32 = rgb(0x1D, 0x1D, 0x1F); // field labels: full ink
const COLOR_FAINT: u32 = rgb(0x86, 0x86, 0x8B); // explanatory text
const COLOR_MAP_BG: u32 = rgb(0xFF, 0xFF, 0xFF);

/// Section headers: `x, y, width, title`.  The hairline lands on `y + 22`, just
/// above the first row of controls.
const SECTIONS: &[(i32, i32, i32, &str)] = &[
    (14, 12, 672, "显示"),
    (14, 108, 672, "动画特效"),
    (14, 182, 672, "摄像头观察"),
    (14, 596, 672, "诊断"),
];

// dwmapi is loaded on demand so the binary still starts on anything without it.
type DwmSetWindowAttributeFn =
    unsafe extern "system" fn(HWND, u32, *const std::ffi::c_void, u32) -> i32;

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_BORDER_COLOR: u32 = 34;
const DWMWCP_ROUND: i32 = 2;
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

/// Gives the window the same rounded, light frame every other window on
/// Windows 11 has, instead of a hard-edged dark one.
unsafe fn apply_modern_frame(hwnd: HWND) {
    let lib_name = wide("dwmapi.dll");
    let lib = LoadLibraryW(lib_name.as_ptr());
    if lib.is_null() {
        return;
    }
    let Some(proc) = GetProcAddress(lib, c"DwmSetWindowAttribute".as_ptr() as *const u8) else {
        return;
    };
    let set: DwmSetWindowAttributeFn = std::mem::transmute(proc);

    let light: i32 = 0; // FALSE -> light title bar
    set(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &light as *const i32 as *const std::ffi::c_void,
        4,
    );
    set(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &DWMWCP_ROUND as *const i32 as *const std::ffi::c_void,
        4,
    );
    // Let the system draw the hairline border that matches the theme.
    set(
        hwnd,
        DWMWA_BORDER_COLOR,
        &DWMWA_COLOR_NONE as *const u32 as *const std::ffi::c_void,
        4,
    );
}

/// Fills the sheet and draws the section rules.  Group boxes are avoided on
/// purpose: their etched borders are the most dated thing a Win32 dialog can do.
unsafe fn paint_background(hwnd: HWND) -> LRESULT {
    let hdc = GetDC(hwnd);
    if hdc.is_null() {
        return 1;
    }
    let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetClientRect(hwnd, &mut rc);
    let bg = CreateSolidBrush(COLOR_BG);
    FillRect(hdc, &rc, bg);
    DeleteObject(bg);

    ReleaseDC(hwnd, hdc);
    1
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------------- state --

#[allow(dead_code)]
struct CfgState {
    cfg: Config,
    monitors: Vec<Monitor>,
    devices: Vec<(u32, String)>,
    scale: f32,
    font: HFONT,
    title_font: HFONT,
    hint_font: HFONT,
    brush: HGDIOBJ,
    /// Handles of the section headers, so the colour handler can tell them apart
    /// from ordinary labels.
    titles: Vec<HWND>,
    /// Explanatory text, drawn a shade lighter than the labels.
    faint: Vec<HWND>,
    /// The live detection readout, which shares the faint font but not the
    /// faint colour.
    h_status_faint: HWND,
    saved: bool,
    h_monitor_mode: HWND,
    h_eye: HWND,
    h_rate: HWND,
    h_glitch: HWND,
    h_gaze: HWND,
    h_cam_on: HWND,
    h_cam_dev: HWND,
    h_cam_mon: HWND,
    h_offx: HWND,
    h_offy: HWND,
    h_fov: HWND,
    h_dist: HWND,
    h_ppmm: HWND,
    h_thr: HWND,
    h_lost: HWND,
    h_engage: HWND,
    h_release: HWND,
    h_log_on: HWND,
    h_yaw: HWND,
    h_pitch: HWND,
    h_status: HWND,
    /// Live capture owned by the dialog so the map can show where the camera
    /// currently sees somebody.  Dropping it stops the capture thread.
    vision: Option<Vision>,
    vision_device: Option<u32>,
    /// Every child control with the client position it has when unscrolled.
    /// The settings page is taller than a small or heavily scaled display can
    /// show, so the whole form scrolls as one surface.
    children: Vec<(HWND, i32, i32)>,
    content_h: i32,
    view_h: i32,
    scroll_y: i32,
    h_hint: HWND,
    h_path: HWND,
    h_map: HWND,
    camera_controls: Vec<HWND>,
}

impl CfgState {
    fn null() -> Self {
        CfgState {
            cfg: Config::default(),
            monitors: Vec::new(),
            devices: Vec::new(),
            scale: 1.0,
            font: std::ptr::null_mut(),
            title_font: std::ptr::null_mut(),
            hint_font: std::ptr::null_mut(),
            brush: std::ptr::null_mut(),
            titles: Vec::new(),
            faint: Vec::new(),
            h_status_faint: std::ptr::null_mut(),
            saved: false,
            h_monitor_mode: std::ptr::null_mut(),
            h_eye: std::ptr::null_mut(),
            h_rate: std::ptr::null_mut(),
            h_glitch: std::ptr::null_mut(),
            h_gaze: std::ptr::null_mut(),
            h_cam_on: std::ptr::null_mut(),
            h_cam_dev: std::ptr::null_mut(),
            h_cam_mon: std::ptr::null_mut(),
            h_offx: std::ptr::null_mut(),
            h_offy: std::ptr::null_mut(),
            h_fov: std::ptr::null_mut(),
            h_dist: std::ptr::null_mut(),
            h_ppmm: std::ptr::null_mut(),
            h_thr: std::ptr::null_mut(),
            h_lost: std::ptr::null_mut(),
            h_engage: std::ptr::null_mut(),
            h_release: std::ptr::null_mut(),
            h_log_on: std::ptr::null_mut(),
            h_yaw: std::ptr::null_mut(),
            h_pitch: std::ptr::null_mut(),
            h_status: std::ptr::null_mut(),
            vision: None,
            vision_device: None,
            children: Vec::new(),
            content_h: 0,
            view_h: 0,
            scroll_y: 0,
            h_hint: std::ptr::null_mut(),
            h_path: std::ptr::null_mut(),
            h_map: std::ptr::null_mut(),
            camera_controls: Vec::new(),
        }
    }
}

impl CfgState {
    fn reading(&self) -> Option<Reading> {
        self.vision.as_ref().map(|v| v.reading())
    }
}

/// Starts, stops or restarts the preview capture so it always matches the
/// checkbox and the selected device.
unsafe fn sync_camera(st: &mut CfgState) {
    let enabled = is_checked(st.h_cam_on);
    let index = st.cfg.camera_index;
    let available = st.devices.iter().any(|(i, _)| *i == index);
    let want = enabled && available;

    if st.vision_device == Some(index) && want {
        return;
    }
    st.vision = None; // dropping the old one joins its thread
    st.vision_device = None;
    if want {
        st.vision = Some(Vision::start(index, st.cfg.presence_threshold));
        st.vision_device = Some(index);
    }
}

/// Refreshes the live readout under the map.
unsafe fn update_status(st: &CfgState) {
    if st.h_status.is_null() {
        return;
    }
    let text = match st.reading() {
        Some(r) if r.present => {
            let d = sim::estimate_distance(&st.cfg, r) / 1000.0;
            let host = {
                let ppm = st.cfg.px_per_mm.max(0.01);
                let idx = sim::camera_monitor_index(&st.cfg, &st.monitors);
                let (px, py, _) = sim::project_viewer(&st.cfg, &st.monitors, idx, r, d * 1000.0);
                let (mx, my) = (px * ppm, py * ppm);
                st.monitors
                    .iter()
                    .position(|m| {
                        mx >= m.left as f32
                            && mx < m.right as f32
                            && my >= m.top as f32
                            && my < m.bottom as f32
                    })
                    .map(|i| format!("显示器 {}", i + 1))
                    .unwrap_or_else(|| "屏幕间隙".to_string())
            };
            format!("● 已检测到人物 · 约 {d:.1} m · 判定在{host}")
        }
        Some(r) if r.confidence > 0.002 => "○ 似乎有人，但不够确定".to_string(),
        _ => "○ 未检测到人物（坐到摄像头前试试）".to_string(),
    };
    set_text(st.h_status, &text);
}

struct Collector {
    st: *mut CfgState,
    parent: HWND,
}

unsafe extern "system" fn collect_child(child: HWND, data: LPARAM) -> i32 {
    let c = &mut *(data as *mut Collector);
    let st = &mut *c.st;
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetWindowRect(child, &mut r);
    let mut pt = POINT { x: r.left, y: r.top };
    MapWindowPoints(std::ptr::null_mut(), c.parent, &mut pt, 1);
    st.children.push((child, pt.x, pt.y));
    1
}

/// Shifts every control by `-y` and syncs the scroll bar.
unsafe fn set_scroll(st: &mut CfgState, hwnd: HWND, want: i32) -> bool {
    let max_scroll = (st.content_h - st.view_h).max(0);
    let y = want.clamp(0, max_scroll);
    if y == st.scroll_y {
        return false;
    }
    st.scroll_y = y;
    for &(child, x, base_y) in &st.children {
        SetWindowPos(
            child,
            std::ptr::null_mut(),
            x,
            base_y - y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    let info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: st.content_h,
        nPage: st.view_h.max(1) as u32,
        nPos: y,
        nTrackPos: 0,
    };
    SetScrollInfo(hwnd, SB_VERT, &info, 1);
    true
}

/// Scrolls the form and re-draws the live map that scrolled with it.
unsafe fn scroll_to(st: &mut CfgState, hwnd: HWND, want: i32) {
    if set_scroll(st, hwnd, want) {
        repaint_map(st);
    }
}

// ----------------------------------------------------------------- helpers --

unsafe fn create_child(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: i32,
    scale: f32,
) -> HWND {
    let cls = wide(class);
    let txt = wide(text);
    let hinstance = GetModuleHandleW(std::ptr::null());
    let s = |v: i32| (v as f32 * scale).round() as i32;
    CreateWindowExW(
        0,
        cls.as_ptr(),
        txt.as_ptr(),
        style,
        s(x),
        s(y),
        s(w),
        s(h),
        parent,
        id as usize as *mut std::ffi::c_void,
        hinstance,
        std::ptr::null(),
    )
}

unsafe fn set_font(hwnd: HWND, font: HFONT) {
    SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let t = wide(text);
    SetWindowTextW(hwnd, t.as_ptr());
}

unsafe fn get_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd).max(0) as usize;
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len + 2];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn set_f32(hwnd: HWND, value: f32, decimals: usize) {
    set_text(hwnd, &format!("{value:.decimals$}"));
}

unsafe fn get_f32(hwnd: HWND, fallback: f32) -> f32 {
    get_text(hwnd).trim().parse::<f32>().unwrap_or(fallback)
}

unsafe fn set_checked(hwnd: HWND, on: bool) {
    SendMessageW(hwnd, BM_SETCHECK, if on { BST_CHECKED } else { BST_UNCHECKED }, 0);
}

unsafe fn is_checked(hwnd: HWND) -> bool {
    SendMessageW(hwnd, BM_GETCHECK, 0, 0) as usize == BST_CHECKED
}

unsafe fn combo_add(hwnd: HWND, text: &str) {
    let t = wide(text);
    SendMessageW(hwnd, CB_ADDSTRING, 0, t.as_ptr() as LPARAM);
}

unsafe fn combo_select(hwnd: HWND, index: usize) {
    SendMessageW(hwnd, CB_SETCURSEL, index, 0);
}

unsafe fn combo_selected(hwnd: HWND) -> usize {
    let r = SendMessageW(hwnd, CB_GETCURSEL, 0, 0);
    if r < 0 {
        0
    } else {
        r as usize
    }
}


// ------------------------------------------------------------- monitor map --
//
// A to-scale picture of the desktop's monitor arrangement.  Clicking it is how
// the camera's physical position gets specified: the click is converted back to
// virtual-desktop pixels, snapped to whichever monitor it landed on, and stored
// as that monitor's index plus a normalised within-monitor offset.
//
// Since every monitor lives in one shared coordinate space, a click is also the
// only sane way to express "the camera is on the left edge of the middle screen"
// -- typing two floats cannot convey which screen that is.

struct MapGeom {
    scale: f32,
    ox: f32,
    oy: f32,
    min_x: f32,
    min_y: f32,
}

unsafe fn map_geom(hwnd: HWND, st: &CfgState) -> Option<MapGeom> {
    if st.monitors.is_empty() {
        return None;
    }
    let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetClientRect(hwnd, &mut rc);
    let w = (rc.right - rc.left) as f32;
    let h = (rc.bottom - rc.top) as f32;
    if w < 16.0 || h < 16.0 {
        return None;
    }

    let min_x = st.monitors.iter().map(|m| m.left as f32).fold(f32::MAX, f32::min);
    let max_x = st.monitors.iter().map(|m| m.right as f32).fold(f32::MIN, f32::max);
    let min_y = st.monitors.iter().map(|m| m.top as f32).fold(f32::MAX, f32::min);
    let max_y = st.monitors.iter().map(|m| m.bottom as f32).fold(f32::MIN, f32::max);
    let (dw, dh) = ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));

    let pad = 12.0;
    let scale = (((w - pad * 2.0) / dw).min((h - pad * 2.0) / dh)).max(0.0005);
    Some(MapGeom {
        scale,
        ox: (w - dw * scale) * 0.5,
        oy: (h - dh * scale) * 0.5,
        min_x,
        min_y,
    })
}

/// Which monitor currently owns the camera.
fn map_owner(st: &CfgState) -> usize {
    if st.cfg.camera_monitor < 0 {
        st.monitors.iter().position(|m| m.primary).unwrap_or(0)
    } else {
        (st.cfg.camera_monitor as usize).min(st.monitors.len() - 1)
    }
}

unsafe fn map_paint(hwnd: HWND, st: &CfgState) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetClientRect(hwnd, &mut rc);
    let bg = CreateSolidBrush(COLOR_MAP_BG);
    FillRect(hdc, &rc, bg);
    DeleteObject(bg);

    SetBkMode(hdc, 1); // TRANSPARENT

    if let Some(g) = map_geom(hwnd, st) {
        let owner = map_owner(st);

        for (i, m) in st.monitors.iter().enumerate() {
            let x0 = (g.ox + (m.left as f32 - g.min_x) * g.scale).round() as i32;
            let y0 = (g.oy + (m.top as f32 - g.min_y) * g.scale).round() as i32;
            let x1 = (g.ox + (m.right as f32 - g.min_x) * g.scale).round() as i32;
            let y1 = (g.oy + (m.bottom as f32 - g.min_y) * g.scale).round() as i32;
            let r = RECT { left: x0, top: y0, right: x1, bottom: y1 };

            let fill = if i == owner { rgb(0xDC, 0xEC, 0xFF) } else { rgb(0xE6, 0xEB, 0xF2) };
            let brush = CreateSolidBrush(fill);
            FillRect(hdc, &r, brush);
            DeleteObject(brush);

            let border = if m.primary { rgb(0x00, 0x71, 0xE3) } else { rgb(0x9A, 0xAA, 0xBE) };
            let pen = CreatePen(PS_SOLID as i32, if m.primary { 2 } else { 1 }, border);
            let old_pen = SelectObject(hdc, pen);
            let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            Rectangle(hdc, x0, y0, x1, y1);
            SelectObject(hdc, old_brush);
            SelectObject(hdc, old_pen);
            DeleteObject(pen);

            SetTextColor(hdc, rgb(0x25, 0x33, 0x42));
            let label: Vec<u16> = format!("{}{}", i + 1, if m.primary { " 主" } else { "" })
                .encode_utf16()
                .collect();
            // Bottom-left: the top edge is where a bezel-mounted camera usually
            // sits, and it would cover the number.
            TextOutW(hdc, x0 + 5, (y1 - 18).max(y0 + 4), label.as_ptr(), label.len() as i32);
        }

        // ---- camera marker ----
        let m = &st.monitors[owner];
        let cx = g.ox + (m.left as f32 + st.cfg.camera_off_x * m.width() as f32 - g.min_x) * g.scale;
        let cy = g.oy + (m.top as f32 + st.cfg.camera_off_y * m.height() as f32 - g.min_y) * g.scale;
        let (cx, cy) = (cx.round() as i32, cy.round() as i32);

        let ring = CreateSolidBrush(rgb(0xFF, 0xFF, 0xFF));
        let ring_pen = CreatePen(PS_SOLID as i32, 1, rgb(0xFF, 0xFF, 0xFF));
        let op = SelectObject(hdc, ring_pen);
        let ob = SelectObject(hdc, ring);
        Ellipse(hdc, cx - 9, cy - 9, cx + 10, cy + 10);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        DeleteObject(ring);
        DeleteObject(ring_pen);

        // ---- live person position ----------------------------------------
        // Same maths the renderer uses, so what the dialog shows here is exactly
        // where the eye will look.  This is the correction loop: adjust the FOV
        // or the trim until the red dot sits on top of the real you.
        if let Some(r) = st.reading() {
            if r.present {
                let ppm = st.cfg.px_per_mm.max(0.01);
                let dist = sim::estimate_distance(&st.cfg, r);
                let idx = sim::camera_monitor_index(&st.cfg, &st.monitors);
                let (vx, vy, _) = sim::project_viewer(&st.cfg, &st.monitors, idx, r, dist);
                let mx = g.ox + (vx * ppm - g.min_x) * g.scale;
                let my = g.oy + (vy * ppm - g.min_y) * g.scale;

                let mut rc_client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(hwnd, &mut rc_client);
                let px = (mx.round() as i32).clamp(rc_client.left + 4, rc_client.right - 5);
                let py = (my.round() as i32).clamp(rc_client.top + 4, rc_client.bottom - 5);

                // Thin line from the camera to the viewer: the "where it is aimed"
                // indicator that the mount correction is judged against.
                let aim = CreatePen(PS_SOLID as i32, 1, rgb(0xC9, 0x6A, 0x3C));
                let old_pen = SelectObject(hdc, aim);
                MoveToEx(hdc, cx, cy, std::ptr::null_mut());
                LineTo(hdc, mx.round() as i32, my.round() as i32);
                SelectObject(hdc, old_pen);
                DeleteObject(aim);

                let halo = CreateSolidBrush(rgb(0xFF, 0xFF, 0xFF));
                let halo_pen = CreatePen(PS_SOLID as i32, 1, rgb(0xFF, 0xFF, 0xFF));
                let op = SelectObject(hdc, halo_pen);
                let ob = SelectObject(hdc, halo);
                Ellipse(hdc, px - 8, py - 8, px + 9, py + 9);
                SelectObject(hdc, ob);
                SelectObject(hdc, op);
                DeleteObject(halo);
                DeleteObject(halo_pen);

                let dot = CreateSolidBrush(rgb(0xE0, 0x30, 0x30));
                let dot_pen = CreatePen(PS_SOLID as i32, 1, rgb(0xE0, 0x30, 0x30));
                let op = SelectObject(hdc, dot_pen);
                let ob = SelectObject(hdc, dot);
                Ellipse(hdc, px - 4, py - 4, px + 5, py + 5);
                SelectObject(hdc, ob);
                SelectObject(hdc, op);
                DeleteObject(dot);
                DeleteObject(dot_pen);
            }
        }

        let mark = CreateSolidBrush(rgb(0xFF, 0x7A, 0x1A));
        let mark_pen = CreatePen(PS_SOLID as i32, 1, rgb(0xFF, 0x7A, 0x1A));
        let op = SelectObject(hdc, mark_pen);
        let ob = SelectObject(hdc, mark);
        Ellipse(hdc, cx - 5, cy - 5, cx + 6, cy + 6);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        DeleteObject(mark);
        DeleteObject(mark_pen);
    }

    EndPaint(hwnd, &ps);
}

unsafe fn map_click(hwnd: HWND, st: &mut CfgState, lparam: LPARAM) {
    let Some(g) = map_geom(hwnd, st) else { return };
    let mx = (lparam & 0xFFFF) as u16 as i16 as f32;
    let my = ((lparam >> 16) & 0xFFFF) as u16 as i16 as f32;
    let dx = g.min_x + (mx - g.ox) / g.scale;
    let dy = g.min_y + (my - g.oy) / g.scale;

    // Prefer the monitor under the cursor; otherwise snap to the nearest one so
    // a click in the gap between screens still means something sensible.
    let hit = st
        .monitors
        .iter()
        .position(|m| {
            dx >= m.left as f32 && dx < m.right as f32 && dy >= m.top as f32 && dy < m.bottom as f32
        })
        .unwrap_or_else(|| {
            let (cxp, cyp) = (dx, dy);
            st.monitors
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = (cxp - a.center().0).abs() + (cyp - a.center().1).abs();
                    let db = (cxp - b.center().0).abs() + (cyp - b.center().1).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
                .unwrap_or(0)
        });

    // Deliberately *not* clamped: a click in the bezel between two stacked
    // screens has to stay where it was put, which means an offset slightly
    // outside 0..1 relative to whichever screen is nearest.
    let (left, top, mw, mh) = {
        let m = &st.monitors[hit];
        (
            m.left as f32,
            m.top as f32,
            m.width() as f32,
            m.height() as f32,
        )
    };
    let (lo, hi) = crate::config::OFFSET_RANGE;
    let fx = ((dx - left) / mw).clamp(lo, hi);
    let fy = ((dy - top) / mh).clamp(lo, hi);

    st.cfg.camera_monitor = hit as i32;
    st.cfg.camera_off_x = fx;
    st.cfg.camera_off_y = fy;

    set_f32(st.h_offx, fx, 3);
    set_f32(st.h_offy, fy, 3);
    combo_select(st.h_cam_mon, hit + 1);

    InvalidateRect(hwnd, std::ptr::null(), 1);
}

unsafe extern "system" fn map_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = lparam as *const CREATESTRUCTW;
            if !cs.is_null() {
                // A borrowed pointer to the dialog's state; the dialog outlives us.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        _ => {}
    }

    let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CfgState;
    if st.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            map_paint(hwnd, &*st);
            0
        }
        WM_LBUTTONDOWN => {
            map_click(hwnd, &mut *st, lparam);
            0
        }
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND as *const u16));
            1
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn register_map_class(hinstance: *mut std::ffi::c_void) {
    let class = wide(CLASS_MAP);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(map_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: LoadCursorW(std::ptr::null_mut(), IDC_HAND as *const u16),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    RegisterClassExW(&wc);
}

// ---------------------------------------------------------------- controls --

unsafe fn build_controls(hwnd: HWND, st: &mut CfgState, state_ptr: *mut CfgState) {
    let s = st.scale;
    let font = st.font;
    let hinstance = GetModuleHandleW(std::ptr::null());

    let add = |h: HWND| {
        set_font(h, font);
        h
    };

    // Section headers: a medium-weight label with a rule drawn underneath in the
    // background pass.
    for &(x, y, w, title) in SECTIONS {
        let head = create_child(hwnd, "STATIC", title, WS_CHILD_VISIBLE, x, y - 3, w, 24, 0, s);
        set_font(head, st.title_font);
        st.titles.push(head);
    }

    // ---- display ---------------------------------------------------------
    add(create_child(hwnd, "STATIC", "显示屏幕:", WS_CHILD_VISIBLE, 28, 40, 120, 20, 0, s));
    st.h_monitor_mode = add(create_child(
        hwnd,
        "COMBOBOX",
        "",
        WS_CHILD_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST,
        152,
        38,
        240,
        160,
        ID_MONITOR_MODE,
        s,
    ));
    add(create_child(hwnd, "STATIC", "眼睛大小 (%屏幕高):", WS_CHILD_VISIBLE, 28, 70, 130, 20, 0, s));
    st.h_eye = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 162, 68, 60, 22, ID_EYE, s));
    add(create_child(hwnd, "STATIC", "动画速度:", WS_CHILD_VISIBLE, 250, 70, 72, 20, 0, s));
    st.h_rate = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 326, 68, 60, 22, ID_RATE, s));

    // ---- effects ---------------------------------------------------------
    st.h_glitch = add(create_child(hwnd, "BUTTON", "启用故障撕裂特效", WS_CHILD_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX, 28, 136, 170, 20, ID_GLITCH, s));
    st.h_gaze = add(create_child(hwnd, "BUTTON", "瞳孔跟随人物转动", WS_CHILD_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX, 232, 136, 210, 20, ID_GAZE, s));

    // ---- camera ----------------------------------------------------------
    st.h_cam_on = add(create_child(hwnd, "BUTTON", "启用摄像头观察电脑前的人", WS_CHILD_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX, 28, 210, 300, 20, ID_CAM_ON, s));

    add(create_child(hwnd, "STATIC", "摄像头设备:", WS_CHILD_VISIBLE, 28, 240, 118, 20, 0, s));
    st.h_cam_dev = add(create_child(hwnd, "COMBOBOX", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST, 152, 238, 190, 200, ID_CAM_DEV, s));

    add(create_child(hwnd, "STATIC", "摄像头所在屏幕:", WS_CHILD_VISIBLE, 28, 268, 118, 20, 0, s));
    st.h_cam_mon = add(create_child(hwnd, "COMBOBOX", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST, 152, 266, 190, 160, ID_CAM_MON, s));

    add(create_child(hwnd, "STATIC", "摄像头水平位置:", WS_CHILD_VISIBLE, 28, 296, 118, 20, 0, s));
    st.h_offx = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 152, 294, 62, 22, ID_OFFX, s));
    add(create_child(hwnd, "STATIC", "垂直位置:", WS_CHILD_VISIBLE, 216, 296, 58, 20, 0, s));
    st.h_offy = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 276, 294, 62, 22, ID_OFFY, s));

    add(create_child(hwnd, "STATIC", "水平视野 (度):", WS_CHILD_VISIBLE, 28, 324, 118, 20, 0, s));
    st.h_fov = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 152, 322, 56, 22, ID_FOV, s));
    add(create_child(hwnd, "STATIC", "估计距离(mm):", WS_CHILD_VISIBLE, 216, 324, 80, 20, 0, s));
    st.h_dist = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 298, 322, 60, 22, ID_DIST, s));

    add(create_child(hwnd, "STATIC", "像素 / 毫米:", WS_CHILD_VISIBLE, 28, 352, 118, 20, 0, s));
    st.h_ppmm = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 152, 350, 56, 22, ID_PPMM, s));
    add(create_child(hwnd, "STATIC", "识别灵敏度:", WS_CHILD_VISIBLE, 216, 352, 80, 20, 0, s));
    st.h_thr = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 298, 350, 60, 22, ID_THR, s));

    add(create_child(hwnd, "STATIC", "丢失后放弃 (秒):", WS_CHILD_VISIBLE, 28, 380, 130, 20, 0, s));
    st.h_lost = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 152, 378, 56, 22, ID_LOST, s));

    add(create_child(hwnd, "STATIC", "接近距离 (毫米):", WS_CHILD_VISIBLE, 28, 408, 118, 20, 0, s));
    st.h_engage = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 152, 406, 56, 22, ID_ENGAGE, s));
    add(create_child(hwnd, "STATIC", "离开距离(mm):", WS_CHILD_VISIBLE, 216, 408, 80, 20, 0, s));
    st.h_release = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 298, 406, 60, 22, ID_RELEASE, s));

    // ---- the interactive monitor layout ----
    add(create_child(hwnd, "STATIC", "在布局图上点一下，标出摄像头的位置（可点屏幕缝隙）：", WS_CHILD_VISIBLE, 368, 208, 304, 20, 0, s));
    {
        let sc = |v: i32| (v as f32 * s).round() as i32;
        st.h_map = CreateWindowExW(
            0,
            wide(CLASS_MAP).as_ptr(),
            wide("").as_ptr(),
            WS_CHILD_VISIBLE | WS_BORDER,
            sc(368),
            sc(230),
            sc(300),
            sc(176),
            hwnd,
            0 as usize as *mut std::ffi::c_void,
            hinstance,
            state_ptr as *const std::ffi::c_void,
        );
    }
    st.h_status = add(create_child(
        hwnd,
        "STATIC",
        "○ 未检测到人物",
        WS_CHILD_VISIBLE,
        368,
        410,
        304,
        18,
        0,
        s,
    ));

    add(create_child(hwnd, "STATIC", "视角校正（让红点落在你的真实位置）：", WS_CHILD_VISIBLE, 368, 432, 304, 18, 0, s));
    add(create_child(hwnd, "STATIC", "偏航 (度):", WS_CHILD_VISIBLE, 368, 456, 58, 20, 0, s));
    st.h_yaw = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 428, 454, 48, 22, ID_YAW, s));
    add(create_child(hwnd, "STATIC", "俯仰 (度):", WS_CHILD_VISIBLE, 488, 456, 58, 20, 0, s));
    st.h_pitch = add(create_child(hwnd, "EDIT", "", WS_CHILD_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, 548, 454, 48, 22, ID_PITCH, s));

    // ---- diagnostics -----------------------------------------------------
    st.h_log_on = add(create_child(
        hwnd,
        "BUTTON",
        "记录诊断日志（写入配置文件旁的 log.txt）",
        WS_CHILD_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX,
        28,
        626,
        420,
        20,
        ID_LOG_ON,
        s,
    ));
    st.faint.push(add(create_child(
        hwnd,
        "STATIC",
        "默认关闭：屏保会连续运行很久，不需要一直写盘。诊断开关 /d 和 /f 不受此设置影响，崩溃记录始终会写。",
        WS_CHILD_VISIBLE,
        28,
        648,
        640,
        18,
        0,
        s,
    )));

    st.h_hint = add(create_child(
        hwnd,
        "STATIC",
        "橙点=摄像头，红点=看到的人，连线=视线。\n橙点可点在屏幕角落，也可以点在屏幕缝隙上；\n拖偏航 / 俯仰让红点对准你的真实位置，\n再用「水平视野」校准远近。走近到「接近距离」\n以内会换屏并眨眼。",
        WS_CHILD_VISIBLE,
        368,
        482,
        304,
        94,
        0,
        s,
    ));

    st.faint.push(st.h_hint);
    set_font(st.h_hint, st.hint_font);
    st.h_status_faint = st.h_status;
    st.faint.push(st.h_status);
    st.h_path = add(create_child(
        hwnd,
        "STATIC",
        &format!("配置文件: {}", Config::path().display()),
        WS_CHILD_VISIBLE,
        14,
        674,
        672,
        22,
        0,
        s,
    ));
    st.faint.push(st.h_path);
    set_font(st.h_path, st.hint_font);

    // ---- buttons ---------------------------------------------------------
    let defaults = add(create_child(hwnd, "BUTTON", "恢复默认", WS_CHILD_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON, 360, 712, 92, 28, ID_DEFAULTS, s));
    let ok = add(create_child(hwnd, "BUTTON", "确定", WS_CHILD_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON, 464, 712, 100, 28, IDOK, s));
    let cancel = add(create_child(hwnd, "BUTTON", "取消", WS_CHILD_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON, 572, 712, 100, 28, IDCANCEL, s));

    st.camera_controls = vec![
        st.h_cam_dev,
        st.h_cam_mon,
        st.h_offx,
        st.h_offy,
        st.h_fov,
        st.h_dist,
        st.h_ppmm,
        st.h_thr,
        st.h_lost,
        st.h_engage,
        st.h_release,
        st.h_yaw,
        st.h_pitch,
        st.h_status,
        st.h_map,
    ];
    let _ = (defaults, ok, cancel);

    // ---- populate --------------------------------------------------------
    combo_add(st.h_monitor_mode, "仅主屏幕");
    combo_add(st.h_monitor_mode, "所有屏幕（眼睛可跨屏移动）");
    combo_select(
        st.h_monitor_mode,
        match st.cfg.monitor_mode {
            MonitorMode::Primary => 0,
            MonitorMode::All => 1,
        },
    );

    st.devices = Vision::list_devices();
    SendMessageW(st.h_cam_dev, CB_RESETCONTENT, 0, 0);
    if st.devices.is_empty() {
        combo_add(st.h_cam_dev, "（未检测到摄像头）");
    } else {
        for (i, (_, name)) in st.devices.iter().enumerate() {
            combo_add(st.h_cam_dev, &format!("{name}"));
            if st.devices[i].0 == st.cfg.camera_index {
                combo_select(st.h_cam_dev, i);
            }
        }
    }

    combo_add(st.h_cam_mon, "主屏幕（自动）");
    for (i, m) in st.monitors.iter().enumerate() {
        combo_add(
            st.h_cam_mon,
            &format!("显示器 {}  {}x{}", i + 1, m.width(), m.height()),
        );
    }
    let cam_mon_index = if st.cfg.camera_monitor < 0 {
        0
    } else {
        (st.cfg.camera_monitor as usize + 1).min(st.monitors.len())
    };
    combo_select(st.h_cam_mon, cam_mon_index);

    load_values(st);
    sync_enabled(st);
}

unsafe fn load_values(st: &CfgState) {
    set_f32(st.h_eye, st.cfg.eye_scale_percent, 1);
    set_f32(st.h_rate, st.cfg.animation_rate, 2);
    set_checked(st.h_glitch, st.cfg.glitch_enabled);
    set_checked(st.h_gaze, st.cfg.gaze_enabled);

    set_checked(st.h_cam_on, st.cfg.camera_enabled);
    set_f32(st.h_offx, st.cfg.camera_off_x, 3);
    set_f32(st.h_offy, st.cfg.camera_off_y, 3);
    set_f32(st.h_fov, st.cfg.camera_fov_deg, 0);
    set_f32(st.h_dist, st.cfg.camera_distance_mm, 0);
    set_f32(st.h_ppmm, st.cfg.px_per_mm, 2);
    set_f32(st.h_thr, st.cfg.presence_threshold, 4);
    set_f32(st.h_lost, st.cfg.lost_timeout_sec, 1);
    set_f32(st.h_engage, st.cfg.engage_distance_mm, 0);
    set_checked(st.h_log_on, st.cfg.log_enabled);
    set_f32(st.h_release, st.cfg.release_distance_mm, 0);
    set_f32(st.h_yaw, st.cfg.camera_yaw_trim_deg, 1);
    set_f32(st.h_pitch, st.cfg.camera_pitch_trim_deg, 1);
}

unsafe fn sync_enabled(st: &CfgState) {
    let on = is_checked(st.h_cam_on);
    for &h in &st.camera_controls {
        EnableWindow(h, if on { 1 } else { 0 });
    }
    // The device combo is useless when no camera exists.
    if on && st.devices.is_empty() {
        EnableWindow(st.h_cam_dev, 0);
    }
}

unsafe fn repaint_map(st: &CfgState) {
    if !st.h_map.is_null() {
        InvalidateRect(st.h_map, std::ptr::null(), 1);
    }
}

unsafe fn apply_and_save(st: &mut CfgState) {
    let c = &mut st.cfg;
    c.monitor_mode = if combo_selected(st.h_monitor_mode) == 1 {
        MonitorMode::All
    } else {
        MonitorMode::Primary
    };
    c.eye_scale_percent = get_f32(st.h_eye, c.eye_scale_percent).clamp(8.0, 300.0);
    c.animation_rate = get_f32(st.h_rate, c.animation_rate).clamp(0.1, 4.0);
    c.glitch_enabled = is_checked(st.h_glitch);
    c.gaze_enabled = is_checked(st.h_gaze);

    c.camera_enabled = is_checked(st.h_cam_on);
    c.log_enabled = is_checked(st.h_log_on);
    let dev_index = combo_selected(st.h_cam_dev);
    if let Some((idx, _)) = st.devices.get(dev_index) {
        c.camera_index = *idx;
    }
    let mon_index = combo_selected(st.h_cam_mon);
    c.camera_monitor = if mon_index == 0 { -1 } else { (mon_index - 1) as i32 };
    let (lo, hi) = crate::config::OFFSET_RANGE;
    c.camera_off_x = get_f32(st.h_offx, c.camera_off_x).clamp(lo, hi);
    c.camera_off_y = get_f32(st.h_offy, c.camera_off_y).clamp(lo, hi);
    c.camera_fov_deg = get_f32(st.h_fov, c.camera_fov_deg).clamp(10.0, 170.0);
    c.camera_yaw_trim_deg = get_f32(st.h_yaw, c.camera_yaw_trim_deg).clamp(-80.0, 80.0);
    c.camera_pitch_trim_deg = get_f32(st.h_pitch, c.camera_pitch_trim_deg).clamp(-80.0, 80.0);
    c.camera_distance_mm = get_f32(st.h_dist, c.camera_distance_mm).clamp(150.0, 5000.0);
    c.px_per_mm = get_f32(st.h_ppmm, c.px_per_mm).clamp(0.2, 40.0);
    c.presence_threshold = get_f32(st.h_thr, c.presence_threshold).clamp(0.0005, 0.5);
    c.lost_timeout_sec = get_f32(st.h_lost, c.lost_timeout_sec).clamp(0.5, 600.0);
    c.engage_distance_mm = get_f32(st.h_engage, c.engage_distance_mm).clamp(150.0, 5000.0);
    c.release_distance_mm = get_f32(st.h_release, c.release_distance_mm).clamp(150.0, 5000.0);
    c.normalize();

    // Apply the switch before anything logs, so ticking the box makes the very
    // save that enabled it visible, and clearing it silences from here on.
    diag::set_enabled(c.log_enabled);

    match c.save() {
        Ok(()) => {
            st.saved = true;
            LAST_SAVED.with(|c| c.set(true));
            diag::log(&format!("config saved to {}", Config::path().display()));
        }
        Err(e) => diag::log(&format!("config save failed: {e}")),
    }
}

// ------------------------------------------------------------------ wndproc --

unsafe extern "system" fn cfg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CfgState;

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

    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *state_ptr;

    match msg {
        WM_CREATE => {
            build_controls(hwnd, st, state_ptr);

            // Everything is laid out at `DOC_H`; if the window came out shorter
            // (small screen, aggressive scaling) the form scrolls instead.
            let mut rc_client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetClientRect(hwnd, &mut rc_client);
            st.content_h = (CONTENT_H as f32 * st.scale).round() as i32;
            st.view_h = rc_client.bottom - rc_client.top;

            let mut collector = Collector { st, parent: hwnd };
            EnumChildWindows(
                hwnd,
                Some(collect_child),
                &mut collector as *mut Collector as LPARAM,
            );
            set_scroll(st, hwnd, 0);
            // Force the bar to appear even before the first scroll.
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: st.content_h,
                nPage: st.view_h.max(1) as u32,
                nPos: 0,
                nTrackPos: 0,
            };
            SetScrollInfo(hwnd, SB_VERT, &info, 1);

            sync_camera(st);
            SetTimer(hwnd, TIMER_LIVE, LIVE_INTERVAL_MS, None);
            0
        }
        WM_VSCROLL => {
            let code = (wparam & 0xFFFF) as i32;
            let mut info: SCROLLINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<SCROLLINFO>() as u32;
            info.fMask = SIF_ALL;
            GetScrollInfo(hwnd, SB_VERT, &mut info);

            let target = match code {
                SB_LINEUP => st.scroll_y - SCROLL_STEP,
                SB_LINEDOWN => st.scroll_y + SCROLL_STEP,
                SB_PAGEUP => st.scroll_y - st.view_h,
                SB_PAGEDOWN => st.scroll_y + st.view_h,
                SB_THUMBTRACK => info.nTrackPos,
                SB_TOP => 0,
                SB_BOTTOM => st.content_h,
                _ => st.scroll_y,
            };
            scroll_to(st, hwnd, target);
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32;
            // Wheel up (positive delta) reveals earlier content.
            scroll_to(st, hwnd, st.scroll_y - delta / 120 * SCROLL_STEP * 3);
            0
        }
        WM_TIMER => {
            // Keep the map and the readout alive while the user tunes things.
            update_status(st);
            repaint_map(st);
            0
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match id {
                ID_CAM_ON if code == 0 => {
                    sync_enabled(st);
                    sync_camera(st);
                }
                ID_CAM_DEV if code == CBN_SELCHANGE => {
                    let sel = combo_selected(st.h_cam_dev);
                    if let Some((idx, _)) = st.devices.get(sel) {
                        st.cfg.camera_index = *idx;
                    }
                    sync_camera(st);
                    repaint_map(st);
                }
                // Numeric fields that change the projection repaint immediately,
                // so the red dot is a live correction loop rather than a
                // save-and-pray.
                ID_FOV | ID_DIST | ID_PPMM | ID_YAW | ID_PITCH | ID_OFFX | ID_OFFY
                    if code == EN_CHANGE =>
                {
                    if id == ID_FOV {
                        st.cfg.camera_fov_deg =
                            get_f32(st.h_fov, st.cfg.camera_fov_deg).clamp(10.0, 170.0);
                    } else if id == ID_DIST {
                        st.cfg.camera_distance_mm =
                            get_f32(st.h_dist, st.cfg.camera_distance_mm).clamp(150.0, 5000.0);
                    } else if id == ID_PPMM {
                        st.cfg.px_per_mm = get_f32(st.h_ppmm, st.cfg.px_per_mm).clamp(0.2, 40.0);
                    } else if id == ID_YAW {
                        st.cfg.camera_yaw_trim_deg =
                            get_f32(st.h_yaw, st.cfg.camera_yaw_trim_deg).clamp(-80.0, 80.0);
                    } else if id == ID_PITCH {
                        st.cfg.camera_pitch_trim_deg =
                            get_f32(st.h_pitch, st.cfg.camera_pitch_trim_deg).clamp(-80.0, 80.0);
                    }
                    repaint_map(st);
                }
                // Keep the layout marker and the numeric fields in step, whichever
                // one the user touched.
                ID_CAM_MON if code == CBN_SELCHANGE => {
                    let sel = combo_selected(st.h_cam_mon);
                    st.cfg.camera_monitor = if sel == 0 { -1 } else { (sel - 1) as i32 };
                    repaint_map(st);
                }
                ID_DEFAULTS if code == 0 => {
                    st.cfg = Config::default();
                    load_values(st);
                    combo_select(st.h_monitor_mode, 0);
                    combo_select(st.h_cam_mon, 0);
                    sync_enabled(st);
                    sync_camera(st);
                    repaint_map(st);
                }
                IDOK if code == 0 => {
                    apply_and_save(st);
                    DestroyWindow(hwnd);
                }
                IDCANCEL if code == 0 => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as *mut std::ffi::c_void;
            let child = lparam as HWND;
            let colour = if st.titles.iter().any(|&t| t == child) {
                COLOR_TITLE
            } else if child == st.h_status_faint {
                COLOR_LABEL
            } else if st.faint.iter().any(|&t| t == child) {
                COLOR_FAINT
            } else {
                COLOR_LABEL
            };
            SetBkMode(hdc, 1); // TRANSPARENT so the sheet shows through
            SetTextColor(hdc, colour);
            st.brush as LRESULT
        }
        WM_CTLCOLORBTN => {
            // Check boxes paint their own square, and every one of them sits
            // directly on the sheet, so match the sheet colour.
            let hdc = wparam as *mut std::ffi::c_void;
            SetBkMode(hdc, 1);
            SetTextColor(hdc, COLOR_LABEL);
            st.brush as LRESULT
        }
        WM_ERASEBKGND => paint_background(hwnd),
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_LIVE);
            st.vision = None; // joins the capture thread before the box is freed
            st.vision_device = None;
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// --------------------------------------------------------------------- run --

/// Shows the configuration window.  Returns `true` when the user pressed 确定.
pub fn run(cfg: &mut Config, parent: HWND) -> bool {
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());

        let class = wide(CLASS_CFG);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(cfg_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: LoadIconW(hinstance, 1 as *const u16),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (15 + 1) as usize as *mut std::ffi::c_void, // COLOR_BTNFACE + 1
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: LoadIconW(hinstance, 1 as *const u16),
        };
        RegisterClassExW(&wc);
        register_map_class(hinstance);

        let dpi = if parent.is_null() {
            GetDpiForSystem()
        } else {
            let d = GetDpiForWindow(parent);
            if d == 0 {
                GetDpiForSystem()
            } else {
                d
            }
        };
        let scale = (dpi as f32 / 96.0).clamp(1.0, 3.0);

        let mut state = Box::new(CfgState::null());
        state.cfg = cfg.clone();
        state.monitors = monitor::enumerate();
        state.scale = scale;
        state.brush = windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(COLOR_BG) as HGDIOBJ;

        // A Chinese-capable UI font, sized for the current DPI.
        let face = wide("Microsoft YaHei UI");
        state.hint_font = windows_sys::Win32::Graphics::Gdi::CreateFontW(
            -((12.0 * scale).round() as i32),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            face.as_ptr(),
        );
        state.font = windows_sys::Win32::Graphics::Gdi::CreateFontW(
            -((13.0 * scale).round() as i32),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            5, // CLEARTYPE_QUALITY
            0,
            face.as_ptr(),
        );
        // Section labels are small and quiet; the content is what should read.
        state.title_font = windows_sys::Win32::Graphics::Gdi::CreateFontW(
            -((12.0 * scale).round() as i32),
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            face.as_ptr(),
        );

        LAST_SAVED.with(|c| c.set(false));
        let state_ptr = Box::into_raw(state);
        let title = wide("Fairy 屏幕保护程序 — 设置");
        let mut client_w = (CONTENT_W as f32 * scale).round() as i32;
        let mut client_h = (CONTENT_H as f32 * scale).round() as i32;

        // Grow the window as far as the work area allows, then scroll.
        let mut work = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut std::ffi::c_void,
            0,
        );
        let avail_w = (work.right - work.left) - SCREEN_MARGIN;
        let avail_h = (work.bottom - work.top) - SCREEN_MARGIN;
        let scrolls = client_h > avail_h;
        client_h = client_h.min(avail_h);
        client_w = client_w.min(avail_w);
        let style = WS_CAPTION_SYSMENU | WS_CLIPCHILDREN | if scrolls { WS_VSCROLL } else { 0 };

        // CreateWindowExW sizes the *outer* rectangle, so grow the requested
        // client box by the caption and border before handing it over.
        let mut frame = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: client_w,
            bottom: client_h,
        };
        AdjustWindowRectEx(&mut frame, style, 0, WS_EX_CONTROLPARENT);
        let outer_w = frame.right - frame.left;
        let outer_h = frame.bottom - frame.top;

        let owner = if parent.is_null() { std::ptr::null_mut() } else { parent };
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            title.as_ptr(),
            style,
            0x8000_0000u32 as i32, // CW_USEDEFAULT
            0x8000_0000u32 as i32,
            outer_w,
            outer_h,
            owner,
            std::ptr::null_mut(),
            hinstance,
            state_ptr as *const std::ffi::c_void,
        );
        if hwnd.is_null() {
            drop(Box::from_raw(state_ptr));
            return false;
        }

        // Centre on the owner (or on the primary monitor) and show.
        let mut rect = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(hwnd, &mut rect);
        let mut target = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        if !parent.is_null() {
            GetWindowRect(parent, &mut target);
        } else if let Some(m) = monitor::enumerate().into_iter().find(|m| m.primary) {
            target.left = m.left;
            target.top = m.top;
            target.right = m.right;
            target.bottom = m.bottom;
        }
        let cx = (target.left + target.right) / 2 - (rect.right - rect.left) / 2;
        let cy = (target.top + target.bottom) / 2 - (rect.bottom - rect.top) / 2;

        if !parent.is_null() {
            EnableWindow(parent, 0);
        }
        SetWindowPos(
            hwnd,
            HWND_TOP,
            cx,
            cy,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        );
        apply_modern_frame(hwnd);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        if !parent.is_null() {
            EnableWindow(parent, 1);
            SetForegroundWindow(parent);
        }

        // The state box is freed in WM_NCDESTROY, so the outcome travels back
        // through a thread-local flag rather than through the box.
        let saved = LAST_SAVED.with(|c| c.replace(false));
        if saved {
            *cfg = Config::load();
        }
        saved
    }
}

use std::cell::Cell;
thread_local! {
    static LAST_SAVED: Cell<bool> = const { Cell::new(false) };
}
