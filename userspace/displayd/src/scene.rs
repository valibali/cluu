//! Scene management — z-order, visibility, geometry, and surface ownership.
//!
//! The `Scene` owns all surfaces and overlays, tracks pending damage from
//! operations (move, hide, show, destroy, z-order change, content commit),
//! and drives composition via `composite_frame`.
//!
//! # Overlay invariant
//!
//! Overlays (cursor, status bar) are re-applied after every dirty pass
//! in `composite_frame`. This is the fix for the "cursor clobbered by
//! animated window" gotcha: an overlay must not be gated on a flag set
//! only by its own input handler — clients can damage the overlay's
//! cells at any time.

use alloc::vec::Vec;
use cluu_wire::display::{DamageList, Error, OutputInfo, Rect};

use crate::backend::Backend;
use crate::compose;
use crate::damage::DamageAccumulator;
use crate::surface::Surface;

/// Compositor overlay — always painted on top of all surfaces.
/// Used for cursor, status bar, etc.
pub struct Overlay {
    pub pixels: Vec<u32>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub visible: bool,
}

impl Overlay {
    /// Create a new overlay with the given pixel data and position.
    pub fn new(pixels: Vec<u32>, x: i32, y: i32, width: u32, height: u32, pitch: u32) -> Self {
        Overlay {
            pixels,
            x,
            y,
            width,
            height,
            pitch,
            visible: true,
        }
    }

    /// Pitch in u32 words.
    pub fn pitch_words(&self) -> usize {
        self.pitch as usize / 4
    }

    /// Scene rect clipped to output bounds.
    pub fn clipped_rect(&self, output_w: u32, output_h: u32) -> Option<Rect> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let x0 = self.x.max(0) as u32;
        let y0 = self.y.max(0) as u32;
        let x1 = self
            .x
            .saturating_add(self.width as i32)
            .min(output_w as i32)
            .max(0) as u32;
        let y1 = self
            .y
            .saturating_add(self.height as i32)
            .min(output_h as i32)
            .max(0) as u32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// The scene owns all surfaces and overlays, and tracks pending damage.
pub struct Scene {
    /// Output dimensions and format.
    pub output: OutputInfo,
    surfaces: Vec<Surface>,
    overlays: Vec<Overlay>,
    pending_damage: DamageAccumulator,
}

impl Scene {
    /// Create a new scene with the given output info.
    pub fn new(output: OutputInfo) -> Self {
        Scene {
            output,
            surfaces: Vec::new(),
            overlays: Vec::new(),
            pending_damage: DamageAccumulator::new(),
        }
    }

    // ----- Surface lifecycle -----

    /// Create a new surface. Returns error if the token is already in use.
    pub fn create_surface(
        &mut self,
        token: u64,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> Result<(), Error> {
        if self.find_surface_idx(token).is_some() {
            return Err(Error::ForeignSurface);
        }
        let surface = Surface::new(token, width, height, pitch)?;
        self.surfaces.push(surface);
        Ok(())
    }

    /// Destroy a surface. Returns damage at the surface's last position.
    /// Buffer memory is released.
    pub fn destroy_surface(&mut self, token: u64) -> Result<DamageList, Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        let old_rect = if self.surfaces[idx].visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };
        self.surfaces[idx].destroy();

        let mut damage = DamageAccumulator::new();
        if let Some(r) = old_rect {
            damage.add(r);
        }
        let dl = damage.take();
        self.pending_damage.merge(&dl);
        Ok(dl)
    }

    // ----- Surface content -----

    /// Write pixel data into a surface's buffer.
    pub fn write_surface_buffer(
        &mut self,
        token: u64,
        buffer_index: u8,
        pixels: &[u32],
    ) -> Result<(), Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        self.surfaces[idx].write_buffer(buffer_index, pixels)
    }

    /// Present a surface's buffer (set as front) and record content damage.
    /// Returns the scene-space damage translated from the content damage.
    pub fn present_surface(
        &mut self,
        token: u64,
        buffer_index: u8,
        damage: DamageList,
    ) -> Result<DamageList, Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        let (sx, sy) = (self.surfaces[idx].x, self.surfaces[idx].y);
        self.surfaces[idx].present(buffer_index, damage)?;

        // Translate content damage to scene damage.
        // CRITICAL: offset (sx, sy) MUST match the composition's read offset.
        let mut scene_damage = DamageAccumulator::new();
        scene_damage.add_content_damage(&damage, sx, sy, self.output.width, self.output.height);
        let dl = scene_damage.take();
        self.pending_damage.merge(&dl);
        Ok(dl)
    }

    // ----- Geometry / visibility / z-order -----

    /// Move a surface to (x, y). Returns damage at old and new position.
    pub fn move_surface(&mut self, token: u64, x: i32, y: i32) -> Result<DamageList, Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        if self.surfaces[idx].destroyed {
            return Err(Error::InvalidCapability);
        }
        let old_rect = if self.surfaces[idx].visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };
        self.surfaces[idx].x = x;
        self.surfaces[idx].y = y;
        let new_rect = if self.surfaces[idx].visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };

        let mut damage = DamageAccumulator::new();
        if let Some(r) = old_rect {
            damage.add(r);
        }
        if let Some(r) = new_rect {
            damage.add(r);
        }
        let dl = damage.take();
        self.pending_damage.merge(&dl);
        Ok(dl)
    }

    /// Set surface visibility. Returns damage at the surface's position
    /// (old position if hiding, position if showing).
    pub fn set_visible(&mut self, token: u64, visible: bool) -> Result<DamageList, Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        if self.surfaces[idx].destroyed {
            return Err(Error::InvalidCapability);
        }
        let was_visible = self.surfaces[idx].visible;
        if was_visible == visible {
            return Ok(DamageList::empty());
        }
        let rect = if was_visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };

        self.surfaces[idx].visible = visible;

        let rect_after = if visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };

        let mut damage = DamageAccumulator::new();
        if let Some(r) = rect {
            damage.add(r);
        }
        if let Some(r) = rect_after {
            damage.add(r);
        }
        let dl = damage.take();
        self.pending_damage.merge(&dl);
        Ok(dl)
    }

    /// Set z-order. Returns damage covering the surface and all surfaces
    /// it overlaps (occlusion may change).
    pub fn set_z_order(&mut self, token: u64, z_order: i32) -> Result<DamageList, Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        if self.surfaces[idx].destroyed {
            return Err(Error::InvalidCapability);
        }
        self.surfaces[idx].z_order = z_order;

        // Damage the surface's rect and all overlapping surface rects —
        // occlusion may have changed in the overlap regions.
        let mut damage = DamageAccumulator::new();
        let changed_rect = self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height);
        if let Some(r) = changed_rect {
            damage.add(r);
        }
        // Damage all surfaces that overlap the changed surface.
        let changed_token = self.surfaces[idx].token;
        for s in &self.surfaces {
            if s.token == changed_token || !s.visible || s.destroyed {
                continue;
            }
            if let Some(r) = s.clipped_scene_rect(self.output.width, self.output.height) {
                if let Some(cr) = changed_rect {
                if r.clip_to(cr).is_some() {
                        damage.add(r);
                    }
                }
            }
        }
        let dl = damage.take();
        self.pending_damage.merge(&dl);
        Ok(dl)
    }

    /// Set the display (scaled) size of a surface. Returns damage at old
    /// and new display rect.
    pub fn set_display_size(
        &mut self,
        token: u64,
        display_w: u32,
        display_h: u32,
    ) -> Result<DamageList, Error> {
        let idx = self.find_surface_idx(token).ok_or(Error::InvalidCapability)?;
        if self.surfaces[idx].destroyed {
            return Err(Error::InvalidCapability);
        }
        let old_rect = if self.surfaces[idx].visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };
        self.surfaces[idx].display_w = display_w;
        self.surfaces[idx].display_h = display_h;
        let new_rect = if self.surfaces[idx].visible {
            self.surfaces[idx].clipped_scene_rect(self.output.width, self.output.height)
        } else {
            None
        };

        let mut damage = DamageAccumulator::new();
        if let Some(r) = old_rect {
            damage.add(r);
        }
        if let Some(r) = new_rect {
            damage.add(r);
        }
        let dl = damage.take();
        self.pending_damage.merge(&dl);
        Ok(dl)
    }

    // ----- Overlays -----

    /// Add an overlay (cursor, status bar). Returns the overlay index.
    pub fn add_overlay(&mut self, overlay: Overlay) -> usize {
        self.overlays.push(overlay);
        self.overlays.len() - 1
    }

    /// Set overlay position.
    pub fn set_overlay_position(&mut self, idx: usize, x: i32, y: i32) {
        if idx < self.overlays.len() {
            self.overlays[idx].x = x;
            self.overlays[idx].y = y;
        }
    }

    /// Set overlay visibility.
    pub fn set_overlay_visible(&mut self, idx: usize, visible: bool) {
        if idx < self.overlays.len() {
            self.overlays[idx].visible = visible;
        }
    }

    /// Get a reference to an overlay.
    pub fn overlay(&self, idx: usize) -> Option<&Overlay> {
        self.overlays.get(idx)
    }

    // ----- Composition -----

    /// Composite all visible surfaces into the backend's scanout buffer.
    /// Clears damaged regions to black, paints surfaces back-to-front,
    /// re-applies overlays (always — cursor invariant), and returns the
    /// accumulated frame damage.
    pub fn composite_frame<B: Backend>(&mut self, backend: &mut B) -> DamageList {
        compose::composite_frame(self, backend)
    }

    // ----- Accessors -----

    /// Find a surface index by token (non-destroyed surfaces only).
    pub fn find_surface_idx(&self, token: u64) -> Option<usize> {
        self.surfaces
            .iter()
            .position(|s| s.token == token && !s.destroyed)
    }

    /// Get a surface by index.
    pub fn surface(&self, idx: usize) -> Option<&Surface> {
        self.surfaces.get(idx)
    }

    /// Get a mutable surface by index.
    pub fn surface_mut(&mut self, idx: usize) -> Option<&mut Surface> {
        self.surfaces.get_mut(idx)
    }

    /// Number of surfaces (including destroyed).
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    /// Get the pending damage accumulator (for compose module access).
    pub(crate) fn pending_damage(&mut self) -> &mut DamageAccumulator {
        &mut self.pending_damage
    }

    /// Get all surfaces (for compose module access).
    pub(crate) fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// Get all overlays (for compose module access).
    pub(crate) fn overlays(&self) -> &[Overlay] {
        &self.overlays
    }

    /// Damage the entire output (force full repaint).
    pub fn damage_all(&mut self) {
        self.pending_damage
            .add(Rect { x: 0, y: 0, w: self.output.width, h: self.output.height });
    }
}
