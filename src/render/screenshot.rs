//! Backbuffer -> PNG capture.
//!
//! The GPU copy has to happen inside the frame's command encoder (before
//! `present()` hands the swapchain image back), so `State::render` drives that
//! part. Everything after the readback — unpadding rows, swizzling to RGBA and
//! PNG encoding — runs on a worker thread so a capture costs one GPU sync
//! instead of a visible stall.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Screenshots land next to `save/`, not inside it: they are user output, not
/// world data, and must never be picked up by the saved-world scanner.
pub(crate) const SCREENSHOT_DIR: &str = "screenshots";

/// Raw readback of one frame, still in the swapchain's layout.
pub(crate) struct RawFrameCapture {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Row stride in the buffer, padded up to `COPY_BYTES_PER_ROW_ALIGNMENT`.
    pub(crate) padded_bytes_per_row: u32,
    /// True when the source format stores blue in the first channel.
    pub(crate) bgra: bool,
    pub(crate) bytes: Vec<u8>,
}

impl RawFrameCapture {
    /// Drops the row padding and normalizes to opaque RGBA8.
    ///
    /// The swapchain alpha channel is not meaningful for a screenshot (it can be
    /// premultiplied or plain garbage depending on the compositor), so it is
    /// forced to fully opaque.
    pub(crate) fn into_rgba8(self) -> Vec<u8> {
        let width = self.width as usize;
        let height = self.height as usize;
        let padded = self.padded_bytes_per_row as usize;
        let mut out = vec![0u8; width * height * 4];

        for y in 0..height {
            let row = &self.bytes[y * padded..y * padded + width * 4];
            let dst = &mut out[y * width * 4..(y + 1) * width * 4];
            for (src_px, dst_px) in row.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                if self.bgra {
                    dst_px[0] = src_px[2];
                    dst_px[1] = src_px[1];
                    dst_px[2] = src_px[0];
                } else {
                    dst_px[0] = src_px[0];
                    dst_px[1] = src_px[1];
                    dst_px[2] = src_px[2];
                }
                dst_px[3] = 0xff;
            }
        }
        out
    }
}

/// Encodes an RGBA8 buffer as PNG and writes it to `path`.
pub(crate) fn write_png_rgba8(path: &Path, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("rgba buffer does not match {width}x{height}"),
        )
    })?;
    buffer
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|err| io::Error::other(format!("png encode failed: {err}")))
}

/// `screenshots/<YYYY-MM-DD_HH-MM-SS>.png`, with a `-N` suffix if that second
/// already produced a file.
pub(crate) fn next_screenshot_path() -> PathBuf {
    let dir = PathBuf::from(SCREENSHOT_DIR);
    let stem = utc_timestamp_stem(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    );
    let candidate = dir.join(format!("{stem}.png"));
    if !candidate.exists() {
        return candidate;
    }
    for attempt in 1..1000 {
        let candidate = dir.join(format!("{stem}-{attempt}.png"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-overflow.png"))
}

/// `YYYY-MM-DD_HH-MM-SS` in UTC from a Unix timestamp. Written out here because
/// the crate has no date/time dependency and a screenshot folder full of raw
/// epoch numbers is unusable.
fn utc_timestamp_stem(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let secs_of_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}")
}

/// Howard Hinnant's `civil_from_days`, for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::{RawFrameCapture, utc_timestamp_stem};

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(utc_timestamp_stem(0), "1970-01-01_00-00-00");
        // 2026-08-20T10:47:04Z, taken from a real run label.
        assert_eq!(utc_timestamp_stem(1_787_215_624), "2026-08-20_08-47-04");
        // Leap day.
        assert_eq!(utc_timestamp_stem(1_709_164_800), "2024-02-29_00-00-00");
    }

    #[test]
    fn unpads_rows_and_swizzles_bgra() {
        // 2x2 image with a padded stride of 12 bytes (2px * 4 + 4 padding).
        let padded = 12;
        let mut bytes = vec![0u8; padded * 2];
        // row 0: blue pixel, green pixel (stored BGRA)
        bytes[0..4].copy_from_slice(&[255, 0, 0, 7]);
        bytes[4..8].copy_from_slice(&[0, 255, 0, 7]);
        // row 1: red pixel, white pixel
        bytes[12..16].copy_from_slice(&[0, 0, 255, 7]);
        bytes[16..20].copy_from_slice(&[255, 255, 255, 7]);

        let rgba = RawFrameCapture {
            width: 2,
            height: 2,
            padded_bytes_per_row: padded as u32,
            bgra: true,
            bytes,
        }
        .into_rgba8();

        assert_eq!(rgba.len(), 2 * 2 * 4);
        assert_eq!(&rgba[0..4], &[0, 0, 255, 255], "blue pixel");
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255], "green pixel");
        assert_eq!(&rgba[8..12], &[255, 0, 0, 255], "red pixel");
        assert_eq!(&rgba[12..16], &[255, 255, 255, 255], "white pixel");
    }
}
