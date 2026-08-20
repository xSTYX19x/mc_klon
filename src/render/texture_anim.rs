use bytemuck::{Pod, Zeroable};
use serde::Deserialize;

use crate::assets::AssetSource;
use crate::config::render::{
    ANIM_FLAG_ANIMATED, DEFAULT_FRAMETIME_TICKS, DEFAULT_LAVA_FRAMETIME_TICKS,
    DEFAULT_WATER_FRAMETIME_TICKS,
};

pub(crate) fn make_missing_texture_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0_u8; (width as usize) * (height as usize) * 4];
    let check = 4_u32;
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let odd = ((x / check) + (y / check)) % 2 == 0;
            let (r, g, b) = if odd {
                (255_u8, 0_u8, 255_u8)
            } else {
                (0_u8, 0_u8, 0_u8)
            };
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = 255;
        }
    }
    rgba
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnimAxis {
    Vertical,
}

#[derive(Clone, Debug)]
pub(crate) struct AnimMeta {
    pub(crate) axis: AnimAxis,
    pub(crate) sheet_frames: u32,
    pub(crate) frametime_ticks: u32,
    pub(crate) sequence: Vec<u16>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AnimState {
    pub(crate) seq_index: u32,
    pub(crate) accum_ticks: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct AnimGpu {
    pub(crate) sheet_frames: u32,
    pub(crate) current_frame: u32,
    pub(crate) flags: u32,
    pub(crate) _pad: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct TextureAnimTrack {
    pub(crate) texture_id: u32,
    pub(crate) meta: AnimMeta,
    pub(crate) state: AnimState,
}

#[derive(Deserialize)]
struct McmetaFile {
    animation: Option<McmetaAnimation>,
}

#[derive(Deserialize)]
struct McmetaAnimation {
    frametime: Option<u32>,
    frames: Option<Vec<McmetaFrame>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum McmetaFrame {
    Index(u16),
    Indexed { index: u16, time: Option<u32> },
}

/// The `.mcmeta` sidecar has to come from the same source as the texture itself,
/// or a pack's animation timings would be applied to the default texture.
fn mcmeta_key_for(key: &str) -> String {
    let relative = crate::assets::normalize_relative(key);
    match relative.strip_suffix(".png") {
        Some(stem) => format!("{stem}.png.mcmeta"),
        None => format!("{relative}.mcmeta"),
    }
}

fn default_frametime_ticks_for_key(key: &str) -> u32 {
    if key.contains("water_") {
        DEFAULT_WATER_FRAMETIME_TICKS
    } else if key.contains("lava_") {
        DEFAULT_LAVA_FRAMETIME_TICKS
    } else {
        DEFAULT_FRAMETIME_TICKS
    }
}

pub(crate) fn resize_rgba_nearest(
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    if src_w == dst_w && src_h == dst_h {
        return src_rgba.to_vec();
    }
    let mut out = vec![0_u8; (dst_w * dst_h * 4) as usize];
    for y in 0..dst_h {
        let sy = (y as u64 * src_h as u64 / dst_h as u64) as u32;
        for x in 0..dst_w {
            let sx = (x as u64 * src_w as u64 / dst_w as u64) as u32;
            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((y * dst_w + x) * 4) as usize;
            out[dst_idx..(dst_idx + 4)].copy_from_slice(&src_rgba[src_idx..(src_idx + 4)]);
        }
    }
    out
}

pub(crate) fn decode_texture_frames(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> (u32, Vec<Vec<u8>>, u32) {
    if height > width && height % width == 0 {
        let sheet_frames = height / width;
        let row_bytes = (width * 4) as usize;
        let mut frames = Vec::with_capacity(sheet_frames as usize);
        for frame_idx in 0..sheet_frames {
            let mut frame = vec![0_u8; (width * width * 4) as usize];
            let start_row = (frame_idx * width) as usize;
            for y in 0..(width as usize) {
                let src_start = (start_row + y) * row_bytes;
                let src_end = src_start + row_bytes;
                let dst_start = y * row_bytes;
                let dst_end = dst_start + row_bytes;
                frame[dst_start..dst_end].copy_from_slice(&rgba[src_start..src_end]);
            }
            frames.push(frame);
        }
        (sheet_frames, frames, width)
    } else {
        let frame_size = width.max(height).max(1);
        let frame = if width == height {
            rgba.to_vec()
        } else {
            resize_rgba_nearest(rgba, width, height, frame_size, frame_size)
        };
        (1, vec![frame], frame_size)
    }
}

pub(crate) fn parse_anim_meta_for_texture(
    source: &AssetSource,
    key: &str,
    sheet_frames: u32,
) -> AnimMeta {
    let mut frametime_ticks = default_frametime_ticks_for_key(key);
    let mut sequence: Vec<u16> = (0..sheet_frames).map(|f| f as u16).collect();

    let mcmeta_key = mcmeta_key_for(key);
    if let Some(text) = source.read_to_string(&mcmeta_key) {
        match serde_json::from_str::<McmetaFile>(&text) {
            Ok(parsed) => {
                if let Some(anim) = parsed.animation {
                    frametime_ticks = anim.frametime.unwrap_or(DEFAULT_FRAMETIME_TICKS).max(1);
                    if let Some(raw_frames) = anim.frames {
                        let mut parsed_sequence = Vec::with_capacity(raw_frames.len());
                        for frame in raw_frames {
                            let idx = match frame {
                                McmetaFrame::Index(i) => i,
                                McmetaFrame::Indexed { index, time } => {
                                    if let Some(frame_time) = time {
                                        for _ in 1..frame_time.max(1) {
                                            parsed_sequence.push(index);
                                        }
                                    }
                                    index
                                }
                            };
                            if (idx as u32) < sheet_frames {
                                parsed_sequence.push(idx);
                            } else {
                                crate::console_error!(
                                    "[warn] animation frame {} out of range for '{}' (sheet_frames={})",
                                    idx,
                                    key,
                                    sheet_frames
                                );
                            }
                        }
                        if !parsed_sequence.is_empty() {
                            sequence = parsed_sequence;
                        }
                    }
                }
            }
            Err(err) => {
                crate::console_error!(
                    "[warn] failed to parse animation mcmeta for '{}': {}",
                    key,
                    err
                );
            }
        }
    }

    AnimMeta {
        axis: AnimAxis::Vertical,
        sheet_frames: sheet_frames.max(1),
        frametime_ticks: frametime_ticks.max(1),
        sequence,
    }
}

pub(crate) fn initial_anim_gpu_for_meta(meta: &AnimMeta) -> AnimGpu {
    let current_frame = meta.sequence.first().copied().unwrap_or(0) as u32;
    AnimGpu {
        sheet_frames: meta.sheet_frames.max(1),
        current_frame,
        flags: if meta.sheet_frames > 1 {
            ANIM_FLAG_ANIMATED
        } else {
            0
        },
        _pad: 0,
    }
}
