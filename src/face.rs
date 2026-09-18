//! Face detection on top of `Windows.Media.FaceAnalysis`.
//!
//! The skin-tone blob detector this replaces could only ever answer "where is
//! something skin coloured", and on a real desk that means a wooden surface, a
//! beige wall, a bare forearm, or the viewer's own torso.  Its winning blob was
//! therefore frequently the torso rather than the head, which put the centroid
//! somewhere around the chest -- exactly the "the bearing is basically wrong"
//! failure that motivated this module.
//!
//! A face detector answers the question that was actually being asked.  It also
//! hands back a real bounding box, and the height of a *face* is a far better
//! distance cue than the height of a skin blob, because face size barely varies
//! between people while blob size depends on what clothes they happen to wear.
//!
//! The runtime ships with Windows 10 and later, needs no model file, and runs
//! on the CPU.  When it is unavailable (N editions, Server Core, a stripped
//! image) [`Detector::new`] fails and the caller falls back to the old
//! skin-tone path.

use std::future::IntoFuture;

use crate::pixels;
use windows::Graphics::Imaging::{BitmapPixelFormat, BitmapSize, SoftwareBitmap};
use windows::Media::FaceAnalysis::FaceDetector;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Longest edge the frame is resampled to before detection.  FaceAnalysis walks
/// a fixed image pyramid, so handing it a 1080p frame costs a lot of CPU and
/// buys nothing: a face large enough to matter survives the downscale intact.
const WORK_MAX_EDGE: usize = 480;

/// Smallest face worth reporting, in *working* pixels.  At 480px across this is
/// roughly 3.5 m of reach with a 60 degree lens, which comfortably covers
/// "somebody at this desk".
const MIN_FACE_PX: i32 = 32;

/// One detected face, normalised to the full frame (0..1).
#[derive(Clone, Copy, Debug)]
pub struct Face {
    /// Centre of the bounding box.
    pub x: f32,
    pub y: f32,
    /// Bounding box size, as a fraction of the frame.
    pub w: f32,
    pub h: f32,
}

pub struct Detector {
    detector: FaceDetector,
    /// Reused grayscale scratch buffer, sized `WORK_MAX_EDGE` squared at most.
    gray: Vec<u8>,
}

impl Detector {
    /// Brings up the runtime detector.  Also initialises COM on the calling
    /// thread, which the WinRT activation path requires.
    pub fn new() -> Result<Self, String> {
        // SAFETY: the WinRT activation path requires the calling thread to be a
        // COM apartment.  The capture thread is not one otherwise, and
        // activation would fail with CO_E_NOTINITIALIZED.  A failed call is
        // harmless -- it usually means the thread is already initialised, which
        // is exactly what we wanted.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let detector = block_on(
            FaceDetector::CreateAsync()
                .map_err(|e| format!("FaceDetector::CreateAsync: {e}"))?
                .into_future(),
        )
        .map_err(|e| format!("FaceDetector creation: {e}"))?;

        // Ask for faces down to MIN_FACE_PX.  The default lower bound is tuned
        // for group photos and misses a face at arm's length on a downscaled
        // frame.
        let min = BitmapSize {
            Width: MIN_FACE_PX as u32,
            Height: MIN_FACE_PX as u32,
        };
        detector
            .SetMinDetectableFaceSize(min)
            .map_err(|e| format!("SetMinDetectableFaceSize: {e}"))?;

        Ok(Detector {
            detector,
            gray: Vec::new(),
        })
    }

    /// Detects every face in an RGB8 frame.  Coordinates come back normalised to
    /// the frame that was passed in, so the caller never sees the working size.
    pub fn detect(&mut self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Face>, String> {
        if w == 0 || h == 0 || rgb.len() < w * h * 3 {
            return Ok(Vec::new());
        }

        let (dw, dh) = working_size(w, h);
        pixels::gray_into(rgb, w, h, dw, dh, &mut self.gray);

        let writer = DataWriter::new().map_err(|e| format!("DataWriter: {e}"))?;
        writer
            .WriteBytes(&self.gray[..dw * dh])
            .map_err(|e| format!("WriteBytes: {e}"))?;
        let buffer = writer.DetachBuffer().map_err(|e| format!("DetachBuffer: {e}"))?;

        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Gray8,
            dw as i32,
            dh as i32,
        )
        .map_err(|e| format!("SoftwareBitmap: {e}"))?;

        let found = block_on(
            self.detector
                .DetectFacesAsync(&bitmap)
                .map_err(|e| format!("DetectFacesAsync: {e}"))?
                .into_future(),
        )
        .map_err(|e| format!("face detection: {e}"))?;

        let n = found.Size().map_err(|e| format!("Size: {e}"))?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let face = found.GetAt(i).map_err(|e| format!("GetAt: {e}"))?;
            let r = face.FaceBox().map_err(|e| format!("FaceBox: {e}"))?;
            if r.Width == 0 || r.Height == 0 {
                continue;
            }
            // FaceAnalysis reports in working-bitmap pixels; normalise against
            // that, not against the original frame.
            out.push(Face {
                x: (r.X as f32 + r.Width as f32 * 0.5) / dw as f32,
                y: (r.Y as f32 + r.Height as f32 * 0.5) / dh as f32,
                w: r.Width as f32 / dw as f32,
                h: r.Height as f32 / dh as f32,
            });
        }
        Ok(out)
    }
}

/// Blocks the calling thread until a WinRT async operation finishes.
///
/// `windows-future` does implement `Future` for `IAsyncOperation` but keeps its
/// `join` helper behind a private trait, so the saver parks its own thread here
/// rather than pulling in an async runtime for a single call site.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// The size the frame is resampled to: longest edge capped at `WORK_MAX_EDGE`,
/// never upscaled.
fn working_size(w: usize, h: usize) -> (usize, usize) {
    let longest = w.max(h);
    if longest <= WORK_MAX_EDGE {
        return (w, h);
    }
    let s = WORK_MAX_EDGE as f32 / longest as f32;
    (
        ((w as f32 * s).round() as usize).max(1),
        ((h as f32 * s).round() as usize).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_size_caps_the_long_edge() {
        assert_eq!(working_size(640, 480), (480, 360));
        assert_eq!(working_size(1920, 1080), (480, 270));
        // Never upscales a frame that is already small.
        assert_eq!(working_size(320, 240), (320, 240));
    }

}
