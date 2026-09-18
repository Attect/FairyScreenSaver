//! Frame-difference motion sensing.
//!
//! This is the fallback for when the face detector comes up empty -- somebody
//! turned away, sat sideways, or dropped out of frame for a moment.  "Nobody's
//! face is visible" is not the same as "nobody is here", and the eye going
//! blank the moment its owner looks at a second monitor is exactly the kind of
//! behaviour that reads as broken.
//!
//! Movement is cheap to find and agnostic about what moved, so it also keeps
//! the eye following a hand waved off to one side.  What it cannot do is hold
//! an aim on somebody who is sitting perfectly still; the caller covers that by
//! remembering the last known position instead of falling back to "gone".

use crate::pixels;

/// Grid width the frame is reduced to.  Coarse on purpose: a cell is ~2% of the
/// frame across, which is the scale at which a person's movement lives and far
/// above the scale of sensor noise.
const GRID_COLS: usize = 48;

/// A cell is "moved" once its luma shifts by this much.  Camera noise and slow
/// auto-exposure ramps stay well under it; a hand does not.
const DIFF_THRESHOLD: i32 = 20;

/// Fewer moved cells than this is noise, not activity.
const MIN_CELLS: u32 = 6;

/// Bounding box and bearing of the winning motion blob, normalised to the frame.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Fraction of the frame that changed.
    pub coverage: f32,
}

pub struct Detector {
    prev: Vec<u8>,
    work: Vec<u8>,
    mask: Vec<bool>,
    visited: Vec<bool>,
    stack: Vec<usize>,
    cols: usize,
    rows: usize,
    primed: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Detector {
            prev: Vec::new(),
            work: Vec::new(),
            mask: Vec::new(),
            visited: Vec::new(),
            stack: Vec::new(),
            cols: 0,
            rows: 0,
            primed: false,
        }
    }

    /// Feeds one frame and returns the largest moving region, if any.
    ///
    /// Call this on *every* frame even when the result is not needed: the
    /// previous-frame buffer has to stay fresh, or the first frame after a
    /// switch to the fallback would compare against something seconds old and
    /// light up the whole image.
    pub fn update(&mut self, rgb: &[u8], w: usize, h: usize) -> Option<Motion> {
        if w == 0 || h == 0 || rgb.len() < w * h * 3 {
            return None;
        }

        // Keep the cell aspect square so a blob's footprint means the same
        // thing in both directions.
        let cols = GRID_COLS;
        let rows = ((GRID_COLS as f32 * h as f32 / w as f32).round() as usize).clamp(4, 96);
        if self.cols != cols || self.rows != rows || self.prev.len() != cols * rows {
            self.cols = cols;
            self.rows = rows;
            self.prev.clear();
            self.prev.resize(cols * rows, 0);
            self.mask.clear();
            self.mask.resize(cols * rows, false);
            self.visited.clear();
            self.visited.resize(cols * rows, false);
            self.primed = false;
        }

        pixels::gray_into(rgb, w, h, cols, rows, &mut self.work);

        if !self.primed {
            // First frame of a new geometry establishes the baseline; there is
            // nothing to difference against yet.
            self.prev.copy_from_slice(&self.work);
            self.primed = true;
            return None;
        }

        let cells = cols * rows;
        let mut moved = 0u32;
        for i in 0..cells {
            let d = self.work[i] as i32 - self.prev[i] as i32;
            let on = d.abs() >= DIFF_THRESHOLD;
            self.mask[i] = on;
            if on {
                moved += 1;
            }
        }

        let best = if moved < MIN_CELLS {
            None
        } else {
            self.largest_blob()
        };

        // Refresh the baseline after scanning, never before.
        self.prev.copy_from_slice(&self.work);
        best
    }

    /// Largest 4-connected run of moved cells.
    fn largest_blob(&mut self) -> Option<Motion> {
        let (cols, rows) = (self.cols, self.rows);
        self.visited.iter_mut().for_each(|v| *v = false);

        let mut best: Option<(u32, f32, f32, usize, usize, usize, usize)> = None;

        for seed in 0..cols * rows {
            if !self.mask[seed] || self.visited[seed] {
                continue;
            }
            self.stack.clear();
            self.stack.push(seed);
            self.visited[seed] = true;

            let (mut area, mut sum_x, mut sum_y) = (0u32, 0f32, 0f32);
            let (mut min_x, mut max_x, mut min_y, mut max_y) = (cols, 0usize, rows, 0usize);

            while let Some(cell) = self.stack.pop() {
                let cx = cell % cols;
                let cy = cell / cols;
                area += 1;
                sum_x += cx as f32;
                sum_y += cy as f32;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                // 4-connected: diagonal neighbours are excluded so a diagonal
                // streak of noise does not chain into one giant blob.
                if cx > 0 && self.mask[cell - 1] && !self.visited[cell - 1] {
                    self.visited[cell - 1] = true;
                    self.stack.push(cell - 1);
                }
                if cx + 1 < cols && self.mask[cell + 1] && !self.visited[cell + 1] {
                    self.visited[cell + 1] = true;
                    self.stack.push(cell + 1);
                }
                if cy > 0 && self.mask[cell - cols] && !self.visited[cell - cols] {
                    self.visited[cell - cols] = true;
                    self.stack.push(cell - cols);
                }
                if cy + 1 < rows && self.mask[cell + cols] && !self.visited[cell + cols] {
                    self.visited[cell + cols] = true;
                    self.stack.push(cell + cols);
                }
            }

            if area < MIN_CELLS {
                continue;
            }
            if best.as_ref().map(|b| area > b.0).unwrap_or(true) {
                best = Some((area, sum_x, sum_y, min_x, max_x, min_y, max_y));
            }
        }

        let (area, sum_x, sum_y, min_x, max_x, min_y, max_y) = best?;
        Some(Motion {
            x: (sum_x / area as f32) / (cols - 1) as f32,
            y: (sum_y / area as f32) / (rows - 1) as f32,
            w: (max_x - min_x + 1) as f32 / cols as f32,
            h: (max_y - min_y + 1) as f32 / rows as f32,
            coverage: area as f32 / (cols * rows) as f32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, luma: u8) -> Vec<u8> {
        vec![luma; w * h * 3]
    }

    #[test]
    fn a_still_scene_reports_nothing() {
        let mut d = Detector::new();
        let f = frame(320, 180, 120);
        assert!(d.update(&f, 320, 180).is_none(), "first frame primes");
        assert!(d.update(&f, 320, 180).is_none(), "no change, no motion");
    }

    #[test]
    fn slow_drift_under_the_threshold_is_ignored() {
        let mut d = Detector::new();
        assert!(d.update(&frame(320, 180, 100), 320, 180).is_none());
        // Auto-exposure nudging the whole image by less than DIFF_THRESHOLD
        // must not register as somebody moving.
        assert!(d.update(&frame(320, 180, 100 + (DIFF_THRESHOLD as u8 - 2)), 320, 180).is_none());
    }

    #[test]
    fn a_local_change_is_found_where_it_happened() {
        let (w, h) = (320, 180);
        let mut d = Detector::new();
        assert!(d.update(&frame(w, h, 60), w, h).is_none());

        // Brighten a patch on the right-hand third of the frame.
        let mut f = frame(w, h, 60);
        for y in 40..120 {
            for x in 220..300 {
                let i = (y * w + x) * 3;
                f[i] = 200;
                f[i + 1] = 200;
                f[i + 2] = 200;
            }
        }
        let m = d.update(&f, w, h).expect("motion should be detected");
        assert!(m.x > 0.6, "blob should sit on the right, got x={}", m.x);
        assert!((m.y - 0.44).abs() < 0.15, "blob should be mid-height, got y={}", m.y);
        assert!(m.coverage > 0.0);
    }
}
