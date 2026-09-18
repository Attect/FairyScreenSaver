//! Shared pixel plumbing for the camera pipeline.

/// Converts RGB8 to 8-bit luminance while resampling in one pass.
///
/// A box filter rather than nearest-neighbour.  Point sampling a 4x downscale
/// aliases away exactly the edges that matter: eyelid and brow contours for the
/// face detector, and small genuine movements for the motion detector, which
/// would otherwise be indistinguishable from per-pixel sensor noise.
pub fn gray_into(rgb: &[u8], w: usize, h: usize, dw: usize, dh: usize, out: &mut Vec<u8>) {
    out.clear();
    out.resize(dw * dh, 0);
    let sx = w as f32 / dw as f32;
    let sy = h as f32 / dh as f32;

    for dy in 0..dh {
        let y0 = (dy as f32 * sy) as usize;
        let y1 = (((dy + 1) as f32 * sy) as usize).clamp(y0 + 1, h);
        for dx in 0..dw {
            let x0 = (dx as f32 * sx) as usize;
            let x1 = (((dx + 1) as f32 * sx) as usize).clamp(x0 + 1, w);

            let mut sum = 0u32;
            let mut n = 0u32;
            for y in y0..y1 {
                let row = y * w;
                for x in x0..x1 {
                    let i = (row + x) * 3;
                    // Rec.601 luma, integer form; both consumers expect a plain
                    // grayscale image and neither is sensitive to the weights.
                    sum += (77 * rgb[i] as u32 + 150 * rgb[i + 1] as u32 + 29 * rgb[i + 2] as u32)
                        >> 8;
                    n += 1;
                }
            }
            out[dy * dw + dx] = (sum / n.max(1)) as u8;
        }
    }
}

/// Writes a 24-bit BMP.  Used by the diagnostics dump so the user can see the
/// exact frame the detector worked from, with the result drawn on top.
pub fn write_bmp(path: &std::path::Path, rgb: &[u8], w: usize, h: usize) -> std::io::Result<()> {
    const HEADER: usize = 54;
    // BMP rows are padded to a 4-byte boundary and stored bottom-up.
    let stride = (w * 3).div_ceil(4) * 4;
    let pixel_bytes = stride * h;
    let mut out = Vec::with_capacity(HEADER + pixel_bytes);

    let file_size = (HEADER + pixel_bytes) as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&(HEADER as u32).to_le_bytes());

    out.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes()); // 72 dpi
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    for y in (0..h).rev() {
        let row_start = out.len();
        for x in 0..w {
            let i = (y * w + x) * 3;
            out.push(rgb[i + 2]);
            out.push(rgb[i + 1]);
            out.push(rgb[i]);
        }
        out.resize(row_start + stride, 0);
    }

    std::fs::write(path, &out)?;
    Ok(())
}

/// Reads a 24-bit BMP back into RGB8.
///
/// The counterpart to [`write_bmp`], and only used by the image-probe entry
/// point: being able to point the detector at a file is what makes camera
/// placement checkable without a person in front of the lens.
pub fn read_bmp(path: &std::path::Path) -> Result<(Vec<u8>, usize, usize), String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() < 54 || &data[0..2] != b"BM" {
        return Err(format!("{}: not a BMP", path.display()));
    }

    let offset = u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize;
    let raw_w = i32::from_le_bytes(data[18..22].try_into().unwrap());
    let raw_h = i32::from_le_bytes(data[22..26].try_into().unwrap());
    let bpp = u16::from_le_bytes(data[28..30].try_into().unwrap());
    if bpp != 24 {
        return Err(format!("{}: only 24-bit BMP is supported, got {bpp}", path.display()));
    }

    // A negative height means the rows are already top-down.
    let top_down = raw_h < 0;
    let (w, h) = (raw_w.unsigned_abs() as usize, raw_h.unsigned_abs() as usize);
    if w == 0 || h == 0 {
        return Err(format!("{}: empty image", path.display()));
    }
    let stride = (w * 3).div_ceil(4) * 4;
    if offset + stride * h > data.len() {
        return Err(format!("{}: truncated pixel data", path.display()));
    }

    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        let source_y = if top_down { y } else { h - 1 - y };
        let row = offset + source_y * stride;
        for x in 0..w {
            let s = row + x * 3;
            let d = (y * w + x) * 3;
            rgb[d] = data[s + 2];
            rgb[d + 1] = data[s + 1];
            rgb[d + 2] = data[s];
        }
    }
    Ok((rgb, w, h))
}

/// Draws a hollow rectangle directly into an RGB8 buffer, clamped to the image.
pub fn draw_rect(rgb: &mut [u8], w: usize, h: usize, x0: i32, y0: i32, x1: i32, y1: i32, colour: [u8; 3]) {
    let put = |rgb: &mut [u8], x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return;
        }
        let i = (y as usize * w + x as usize) * 3;
        rgb[i..i + 3].copy_from_slice(&colour);
    };
    for x in x0..=x1 {
        put(rgb, x, y0);
        put(rgb, x, y1);
    }
    for y in y0..=y1 {
        put(rgb, x0, y);
        put(rgb, x1, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_conversion_averages_instead_of_sampling() {
        // Four pixels, two dark and two light: a 2x downscale must land in the
        // middle, which is the whole point of not using nearest-neighbour.
        let mut px = Vec::new();
        for v in [0u8, 0, 200, 200] {
            px.extend_from_slice(&[v, v, v]);
        }
        let mut out = Vec::new();
        gray_into(&px, 2, 2, 1, 1, &mut out);
        assert_eq!(out.len(), 1);
        assert!((out[0] as i32 - 100).abs() <= 1, "got {}", out[0]);
    }

    #[test]
    fn bmp_round_trips_geometry_and_channel_order() {
        // 3px wide is deliberately not a multiple of 4, so the row padding path
        // is exercised too.
        let (w, h) = (3usize, 2usize);
        let mut rgb = vec![0u8; w * h * 3];
        rgb[0] = 10;
        rgb[1] = 20;
        rgb[2] = 30; // top-left pixel, RGB

        let dir = std::env::temp_dir().join("fairy-bmp-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.bmp");
        write_bmp(&path, &rgb, w, h).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(i32::from_le_bytes(bytes[18..22].try_into().unwrap()), w as i32);
        assert_eq!(i32::from_le_bytes(bytes[22..26].try_into().unwrap()), h as i32);
        // Bottom-up storage, so the *last* row of the file is the first row of
        // the image, and BMP is BGR.  Rows are padded to 4 bytes: 3px * 3B = 9B
        // of data in a 12-byte row.
        let stride = (w * 3).div_ceil(4) * 4;
        assert_eq!(stride, 12);
        let last_row = bytes.len() - stride;
        assert_eq!(&bytes[last_row..last_row + 3], &[30, 20, 10]);
        assert_eq!(&bytes[last_row + 3..last_row + stride], &[0u8; 9][..]);
        let _ = std::fs::remove_file(&path);
    }
}
