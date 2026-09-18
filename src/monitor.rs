//! Monitor enumeration in *virtual desktop* coordinates.
//!
//! The virtual desktop origin sits at the primary monitor's top-left corner, so
//! a monitor placed to the left of (or above) the primary gets negative
//! coordinates.  All geometry the eye uses is expressed in this one space, which
//! is what lets a single eye slide across monitor boundaries.

use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};

#[derive(Clone, Debug)]
pub struct Monitor {
    pub index: u32,
    pub handle: HMONITOR,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub primary: bool,
    pub device: String,
}

impl Monitor {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn center(&self) -> (f32, f32) {
        (
            (self.left + self.right) as f32 * 0.5,
            (self.top + self.bottom) as f32 * 0.5,
        )
    }
}

struct Collect {
    monitors: Vec<Monitor>,
}

unsafe extern "system" fn enum_proc(
    handle: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> i32 {
    let collect = &mut *(data as *mut Collect);
    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(handle, &mut info.monitorInfo) == 0 {
        return 1;
    }
    let r = info.monitorInfo.rcMonitor;
    let len = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let device = String::from_utf16_lossy(&info.szDevice[..len]);
    collect.monitors.push(Monitor {
        index: collect.monitors.len() as u32,
        handle,
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
        primary: (info.monitorInfo.dwFlags & 1) != 0, // MONITORINFOF_PRIMARY
        device,
    });
    1
}

/// Enumerates every monitor.  The primary monitor is always first so that
/// `monitors[0]` is the safe default home for the eye.
pub fn enumerate() -> Vec<Monitor> {
    let mut collect = Collect { monitors: Vec::new() };
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(enum_proc),
            &mut collect as *mut Collect as LPARAM,
        );
    }
    collect.monitors.sort_by_key(|m| (!m.primary, m.index));
    for (i, m) in collect.monitors.iter_mut().enumerate() {
        m.index = i as u32;
    }
    collect.monitors
}

/// Falls back to the whole virtual screen when enumeration somehow yields
/// nothing (headless session, RDP edge cases, ...).
pub fn fallback() -> Monitor {
    Monitor {
        index: 0,
        handle: std::ptr::null_mut(),
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
        primary: true,
        device: String::from("\\\\.\\DISPLAY1"),
    }
}
