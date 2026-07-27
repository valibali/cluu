//! Composition — XRGB8888 row-copy fast path, integer nearest scaling,
//! clipping, and occlusion.
//!
//! # Fast path (unscaled)
//!
//! When `display_w == width` and `display_h == height`, the composition
//! uses `copy_from_slice` per row — a nonvolatile slice copy from SHM to
//! the scanout buffer. This is the XRGB8888 row-copy fast path.
//!
//! `src[y * src_pitch/4 ..][x .. x+w]` → `dst[y * dst_pitch/4 ..][x .. x+w]`
//!
//! # Integer nearest scaling
//!
//! When `display_w > width` (integer multiple), the composition uses
//! precomputed step tables mapping destination coords to source coords:
//! `src_x = dst_x * width / display_w`. No floating point.
//!
//! # Clipping
//!
//! Source and destination rects are clipped to their respective bounds.
//! Surfaces partially off-screen are composited only in the visible region.
//!
//! # Occlusion
//!
//! Surfaces are painted back-to-front (lower z-order first). Higher
//! z-order surfaces overwrite lower ones in the overlap — only the
//! top surface's pixels appear in the output for overlapping regions.
//!
//! # Overlay re-apply
//!
//! Overlays are re-applied after every composition pass, unconditionally.
//! This is the fix for the "cursor clobbered by animated window" gotcha:
//! client content may overwrite overlay cells at any time, so overlays
//! must be repainted every frame.

use alloc::vec;
use alloc::vec::Vec;
use cluu_wire::display::{DamageList, PixelFormat, Rect};

use crate::backend::Backend;
use crate::scene::{Overlay, Scene};
use crate::surface::Surface;

/// Precomputed source-coordinate lookup tables for integer nearest scaling.
/// `src_x_table[dst_x] = dst_x * src_dim / dst_dim`.
struct ScaleTable {
    src_x: Vec<u32>,
    src_y: Vec<u32>,
}

impl ScaleTable {
    fn build(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Self {
        let mut src_x = vec![0u32; dst_w as usize];
        for i in 0..dst_w {
            src_x[i as usize] = i * src_w / dst_w;
        }
        let mut src_y = vec![0u32; dst_h as usize];
        for i in 0..dst_h {
            src_y[i as usize] = i * src_h / dst_h;
        }
        ScaleTable { src_x, src_y }
    }
}

/// Composite all visible surfaces into the backend's scanout buffer.
///
/// 1. Take pending damage (from moves, hides, shows, content commits).
/// 2. Clear damaged regions to black (background).
/// 3. Paint all visible surfaces back-to-front (z-order).
/// 4. Re-apply overlays (always — cursor invariant).
/// 5. Return the frame damage.
pub fn composite_frame<B: Backend>(scene: &mut Scene, backend: &mut B) -> DamageList {
    let damage = scene.pending_damage().take();
    if damage.count == 0 {
        return damage;
    }

    let output = backend.output_info();
    // Only XRGB8888 is supported.
    debug_assert_eq!(output.format, PixelFormat::Xrgb8888);

    let scanout = backend.scanout_buffer_mut();
    let dst_pitch_words = output.pitch as usize / 4;
    let output_rect = Rect { x: 0, y: 0, w: output.width, h: output.height };

    // 1. Clear damaged regions to black.
    for r in damage.rects() {
        if let Some(clipped) = r.clip_to(output_rect) {
            clear_rect(scanout, dst_pitch_words, clipped);
        }
    }

    // 2. Paint all visible surfaces back-to-front (z-order).
    //    Collect indices and sort by z_order (lower = farther back).
    //    Clipped to damage rects so idle 1 Hz clock ticks don't blit the
    //    full surface (Bug B — was 12% CPU at idle).
    let mut indices: Vec<usize> = (0..scene.surface_count())
        .filter(|&i| {
            let s = &scene.surfaces()[i];
            !s.destroyed && s.visible
        })
        .collect();
    indices.sort_by_key(|&i| scene.surfaces()[i].z_order);

    let damage_rects = damage.rects();
    for &idx in &indices {
        let surface: &Surface = &scene.surfaces()[idx];
        if let Some(src) = surface.displayed_pixels() {
            blit_surface(scanout, dst_pitch_words, output.width, output.height, surface, src, damage_rects);
        }
    }

    // 3. Re-apply overlays — ALWAYS, per cursor invariant.
    //    Client content may have overwritten overlay cells; overlays must
    //    be repainted every frame regardless of whether they "moved".
    for overlay in scene.overlays() {
        if overlay.visible {
            blit_overlay(scanout, dst_pitch_words, output.width, output.height, overlay);
        }
    }

    // 4. Flush to backend (hardware barrier if needed).
    backend.flush(&damage);

    damage
}

/// Clear a rect in the scanout buffer to black (0x00000000).
fn clear_rect(dst: &mut [u32], dst_pitch_words: usize, rect: Rect) {
    for row in 0..rect.h {
        let off = (rect.y + row) as usize * dst_pitch_words + rect.x as usize;
        let end = off + rect.w as usize;
        if end <= dst.len() {
            for px in &mut dst[off..end] {
                *px = 0;
            }
        }
    }
}

/// Blit a surface to the scanout buffer, clipped to the damage rects.
/// Only rows/columns that intersect a damage rect are copied; undamaged
/// scanout area is left untouched. Handles unscaled (row-copy fast path)
/// and integer-scaled (nearest-neighbor) cases, with output-bound clipping.
fn blit_surface(
    dst: &mut [u32],
    dst_pitch_words: usize,
    out_w: u32,
    out_h: u32,
    surface: &Surface,
    src: &[u32],
    damage: &[Rect],
) {
    let src_pitch_words = surface.pitch_words();
    let sx = surface.x;
    let sy = surface.y;
    let dw = surface.display_w;
    let dh = surface.display_h;

    // Surface display rect in output coords, clipped to output bounds.
    let surf_x0 = sx.max(0) as u32;
    let surf_y0 = sy.max(0) as u32;
    let surf_x1 = sx
        .saturating_add(dw as i32)
        .min(out_w as i32)
        .max(0) as u32;
    let surf_y1 = sy
        .saturating_add(dh as i32)
        .min(out_h as i32)
        .max(0) as u32;

    if surf_x1 <= surf_x0 || surf_y1 <= surf_y0 {
        return;
    }

    let scaled = dw != surface.width || dh != surface.height;
    let table = if scaled {
        Some(ScaleTable::build(surface.width, surface.height, dw, dh))
    } else {
        None
    };

    // Blit each damage rect's intersection with the surface display rect.
    for dr in damage {
        let x0 = dr.x.max(surf_x0);
        let y0 = dr.y.max(surf_y0);
        let x1 = dr.right().min(surf_x1);
        let y1 = dr.bottom().min(surf_y1);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }

        if !scaled {
            // Unscaled: XRGB8888 row-copy fast path.
            // src[y * src_pitch/4 ..][x .. x+w] → dst[y * dst_pitch/4 ..][x .. x+w]
            let copy_w = (x1 - x0) as usize;
            let src_x_start = (x0 as i32 - sx) as usize;
            for row in 0..(y1 - y0) {
                let oy = y0 + row;
                let dy = (oy as i32 - sy) as usize;
                let dst_off = oy as usize * dst_pitch_words + x0 as usize;
                let src_off = dy * src_pitch_words + src_x_start;
                if dst_off + copy_w <= dst.len() && src_off + copy_w <= src.len() {
                    dst[dst_off..dst_off + copy_w]
                        .copy_from_slice(&src[src_off..src_off + copy_w]);
                }
            }
        } else if let Some(ref table) = table {
            // Scaled: integer nearest scaling with precomputed steps.
            for row in 0..(y1 - y0) {
                let oy = y0 + row;
                let dy = (oy as i32 - sy) as usize;
                if dy >= table.src_y.len() {
                    break;
                }
                let src_y = table.src_y[dy] as usize;
                let dst_off = oy as usize * dst_pitch_words + x0 as usize;
                for col in 0..(x1 - x0) as usize {
                    let ox = x0 as usize + col;
                    let dx = (ox as i32 - sx) as usize;
                    if dx >= table.src_x.len() {
                        break;
                    }
                    let src_x = table.src_x[dx] as usize;
                    let src_off = src_y * src_pitch_words + src_x;
                    if dst_off + col < dst.len() && src_off < src.len() {
                        dst[dst_off + col] = src[src_off];
                    }
                }
            }
        }
    }
}

/// Blit an overlay to the scanout buffer. Overlays are always opaque
/// (XRGB8888) — no alpha blending in the pure core.
fn blit_overlay(
    dst: &mut [u32],
    dst_pitch_words: usize,
    out_w: u32,
    out_h: u32,
    overlay: &Overlay,
) {
    let src = &overlay.pixels;
    let src_pitch_words = overlay.pitch_words();
    let sx = overlay.x;
    let sy = overlay.y;
    let w = overlay.width;
    let h = overlay.height;

    let x0 = sx.max(0) as u32;
    let y0 = sy.max(0) as u32;
    let x1 = sx
        .saturating_add(w as i32)
        .min(out_w as i32)
        .max(0) as u32;
    let y1 = sy
        .saturating_add(h as i32)
        .min(out_h as i32)
        .max(0) as u32;

    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let copy_w = (x1 - x0) as usize;
    let src_x_start = (x0 as i32 - sx) as usize;
    for row in 0..(y1 - y0) {
        let oy = y0 + row;
        let dy = (oy as i32 - sy) as usize;
        let dst_off = oy as usize * dst_pitch_words + x0 as usize;
        let src_off = dy * src_pitch_words + src_x_start;
        if dst_off + copy_w <= dst.len() && src_off + copy_w <= src.len() {
            dst[dst_off..dst_off + copy_w]
                .copy_from_slice(&src[src_off..src_off + copy_w]);
        }
    }
}
