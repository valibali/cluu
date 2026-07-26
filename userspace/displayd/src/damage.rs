//! Damage tracking — content/scene/backend damage union and coordinate
//! space translation.
//!
//! # Three coordinate spaces
//!
//! See `cluu_wire::display` module docs for the full specification.
//!
//! - **Content damage** — client-local surface coords. Origin (0,0) is the
//!   surface's top-left. This is what the client commits in `BufferCommit`.
//! - **Scene damage** — compositor output coords. Origin (0,0) is the
//!   output's top-left. Content damage is translated by the surface's
//!   `(x, y)` offset to produce scene damage.
//! - **Backend damage** — hardware/scanout coords. The backend narrows
//!   scene damage to scanout bounds (may be identity if scanout maps 1:1).
//!
//! # CRITICAL invariant
//!
//! The offset used to translate content damage to scene damage MUST be
//! the same offset the composition pass uses to read surface content.
//! If content damage is translated with a `+1, +1` offset, the compose
//! pass MUST read surface content at `+1, +1` — otherwise cells never
//! refresh from SHM and appear "stale until hovered".
//!
//! Source: `cluu-modal-damage-clamps-border-out.md` gotcha.
//!
//! # Overlay invariant
//!
//! Overlays (cursor, status bar) must be re-applied after every dirty
//! pass. The composition core re-applies all overlays unconditionally
//! after compositing client surfaces — see `compose::composite_frame`.
//!
//! Source: `cluu-compositor-cursor-clobbered-by-animated-win.md` gotcha.

use alloc::vec::Vec;
use cluu_wire::display::{DamageList, Rect};

/// Accumulator for scene-space damage rects. Merges overlapping rects
/// and falls back to bounding-box when more than `MAX_DAMAGE_RECTS` (8).
pub struct DamageAccumulator {
    rects: Vec<Rect>,
}

impl DamageAccumulator {
    pub fn new() -> Self {
        Self { rects: Vec::new() }
    }

    /// Add a scene-space rect. Zero-dimension rects are ignored.
    pub fn add(&mut self, rect: Rect) {
        if rect.w > 0 && rect.h > 0 {
            self.rects.push(rect);
        }
    }

    /// Add all rects from a slice.
    pub fn add_all(&mut self, rects: &[Rect]) {
        for r in rects {
            self.add(*r);
        }
    }

    /// Translate content damage to scene damage and add it.
    ///
    /// CRITICAL INVARIANT: `(offset_x, offset_y)` MUST be the same offset
    /// the composition pass uses to place the surface. If compose reads
    /// surface content at `(x, y)`, damage must be translated by `(x, y)`.
    /// A mismatch causes "stale until hovered" rendering.
    ///
    /// Scene damage is clipped to output bounds `(output_w, output_h)`.
    pub fn add_content_damage(
        &mut self,
        damage: &DamageList,
        offset_x: i32,
        offset_y: i32,
        output_w: u32,
        output_h: u32,
    ) {
        for r in damage.rects() {
            let scene_x = offset_x.saturating_add(r.x as i32);
            let scene_y = offset_y.saturating_add(r.y as i32);
            let x0 = scene_x.max(0) as u32;
            let y0 = scene_y.max(0) as u32;
            let x1 = (scene_x.saturating_add(r.w as i32))
                .min(output_w as i32)
                .max(0) as u32;
            let y1 = (scene_y.saturating_add(r.h as i32))
                .min(output_h as i32)
                .max(0) as u32;
            if x1 > x0 && y1 > y0 {
                self.add(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
            }
        }
    }

    /// Merge damage from another accumulator (e.g. per-operation into scene).
    pub fn merge(&mut self, other: &DamageList) {
        self.add_all(other.rects());
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rects.len()
    }

    /// Convert to `DamageList` with overlapping-rect merge and bounding-box
    /// fallback when > 8 rects. Does not consume the accumulator.
    pub fn to_damage_list(&self) -> DamageList {
        let merged = merge_overlapping(self.rects.clone());
        DamageList::from_rects(&merged)
    }

    /// Take and convert, clearing the accumulator.
    pub fn take(&mut self) -> DamageList {
        let merged = merge_overlapping(core::mem::take(&mut self.rects));
        DamageList::from_rects(&merged)
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }
}

impl Default for DamageAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Merge overlapping rects in-place. O(n^2) but n is small (<= 8 after
/// bounding-box fallback). Adjacent rects (sharing an edge) are also
/// merged to reduce count.
fn merge_overlapping(mut rects: Vec<Rect>) -> Vec<Rect> {
    if rects.len() <= 1 {
        return rects;
    }
    let mut changed = true;
    while changed && rects.len() > 1 {
        changed = false;
        'outer: for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                if rects_overlap_or_adjacent(rects[i], rects[j]) {
                    rects[i] = rects[i].extend(rects[j]);
                    rects.swap_remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    rects
}

/// Two rects can be merged if they overlap or are adjacent (share an edge).
fn rects_overlap_or_adjacent(a: Rect, b: Rect) -> bool {
    let x_overlap = a.x < b.right() && b.x < a.right();
    let x_adjacent = a.right() == b.x || b.right() == a.x;
    let y_overlap = a.y < b.bottom() && b.y < a.bottom();
    let y_adjacent = a.bottom() == b.y || b.bottom() == a.y;

    (x_overlap || x_adjacent) && (y_overlap || y_adjacent)
}
