//! CLUU display daemon — pure scene/composition core.
//!
//! This crate implements the displayd scene graph, surface ownership,
//! damage tracking, and composition logic with no framebuffer, registry,
//! IPC, or PCI dependencies. The core is `no_std` + `alloc` and host-testable.
//!
//! # Modules
//!
//! - `surface` — surface ownership, double-buffered pixel data, geometry.
//! - `scene` — z-order, visibility, geometry management, overlay ownership.
//! - `damage` — content/scene/backend damage union, coordinate translation.
//! - `compose` — XRGB8888 row-copy fast path, integer nearest scaling,
//!   clipping, occlusion, overlay re-apply.
//! - `backend` — `Backend` trait abstracting linear-fb and virtio-gpu.
//!
//! # Authority model
//!
//! No runtime ACL or sender-identity checks (AGENTS.md §3). Authority is
//! possession of the per-surface capability token. The pure core validates
//! tokens structurally but does not interrogate the caller.
//!
//! # Damage coordinate invariant
//!
//! The offset used to translate content damage to scene damage MUST match
//! the offset the composition pass uses to read surface content. See
//! `damage` module docs and the `cluu-modal-damage-clamps-border-out` gotcha.

#![cfg_attr(not(any(test, feature = "host-test")), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod backend;
pub mod compose;
pub mod damage;
pub mod direct_damage;
pub mod lease;
pub mod scene;
pub mod surface;

// Re-exports for ergonomic access.
pub use backend::{Backend, MemoryBackend};
pub use damage::DamageAccumulator;
pub use scene::{Overlay, Scene};
pub use surface::Surface;

// Re-export wire types that are part of the core API.
pub use cluu_wire::display::{
    DamageList, Error, Geometry, OutputInfo, PixelFormat, Rect, SurfaceState,
};

#[cfg(test)]
mod tests {
    use super::*;
    use cluu_wire::display::{DamageList, Rect};

    // ----- Test constants -----

    const TOKEN_A: u64 = 0xA000_0000_0000_0001;
    const TOKEN_B: u64 = 0xA000_0000_0000_0002;
    const TOKEN_C: u64 = 0xA000_0000_0000_0003;

    // Colors (XRGB8888: 0x00RRGGBB)
    const RED: u32 = 0x00FF_0000;
    const GREEN: u32 = 0x0000_FF00;
    const BLUE: u32 = 0x0000_00FF;
    const WHITE: u32 = 0x00FF_FFFF;
    const YELLOW: u32 = 0x00FF_FF00;

    const OUT_W: u32 = 32;
    const OUT_H: u32 = 24;
    const OUT_PITCH: u32 = OUT_W * 4;

    fn make_scene() -> Scene {
        Scene::new(OutputInfo {
            width: OUT_W,
            height: OUT_H,
            pitch: OUT_PITCH,
            format: PixelFormat::Xrgb8888,
        })
    }

    fn make_backend() -> MemoryBackend {
        MemoryBackend::new(OUT_W, OUT_H, OUT_PITCH)
    }

    /// Fill a buffer with a solid color.
    fn fill_buffer(width: u32, height: u32, pitch: u32, color: u32) -> Vec<u32> {
        let words_per_row = (pitch / 4) as usize;
        let mut buf = vec![0u32; words_per_row * height as usize];
        for y in 0..height {
            for x in 0..width {
                buf[y as usize * words_per_row + x as usize] = color;
            }
        }
        buf
    }

    /// Full-surface damage.
    fn full_damage(w: u32, h: u32) -> DamageList {
        DamageList::from_rects(&[Rect { x: 0, y: 0, w, h }])
    }

    // ================================================================
    // Test 1: Move surface → damage at old and new position
    // ================================================================
    #[test]
    fn move_surface_damage_at_old_and_new_position() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(4, 4))
            .expect("present");
        scene.move_surface(TOKEN_A, 10, 10).expect("move initial");

        // Now move from (10,10) to (20,20)
        let damage = scene.move_surface(TOKEN_A, 20, 20).expect("move");
        let rects = damage.rects();

        // Should have 2 rects: old (10,10,4,4) and new (20,20,4,4)
        assert_eq!(rects.len(), 2, "move should damage old and new position");
        let has_old = rects.iter().any(|r| *r == Rect { x: 10, y: 10, w: 4, h: 4 });
        let has_new = rects.iter().any(|r| *r == Rect { x: 20, y: 20, w: 4, h: 4 });
        assert!(has_old, "damage must include old position (10,10,4,4)");
        assert!(has_new, "damage must include new position (20,20,4,4)");

        // Verify backend has RED at new position, black at old position.
        let mut backend = make_backend();
        scene.composite_frame(&mut backend);
        assert_eq!(backend.pixel(20, 20), RED, "surface should be at new position");
        assert_eq!(backend.pixel(10, 10), 0, "old position should be cleared");
    }

    // ================================================================
    // Test 2: Hide surface → damage at previous position
    // ================================================================
    #[test]
    fn hide_surface_damage_at_previous_position() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        scene.move_surface(TOKEN_A, 10, 10).expect("move");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(4, 4))
            .expect("present");

        let damage = scene.set_visible(TOKEN_A, false).expect("hide");
        let rects = damage.rects();

        assert!(!rects.is_empty(), "hide should produce damage");
        let has_pos = rects.iter().any(|r| *r == Rect { x: 10, y: 10, w: 4, h: 4 });
        assert!(has_pos, "damage must include surface's previous position");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);
        assert_eq!(backend.pixel(10, 10), 0, "hidden surface position should be black");
    }

    // ================================================================
    // Test 3: Show surface → damage at position
    // ================================================================
    #[test]
    fn show_surface_damage_at_position() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        scene.move_surface(TOKEN_A, 10, 10).expect("move");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(4, 4))
            .expect("present");
        scene.set_visible(TOKEN_A, false).expect("hide");

        let damage = scene.set_visible(TOKEN_A, true).expect("show");
        let rects = damage.rects();

        assert!(!rects.is_empty(), "show should produce damage");
        let has_pos = rects.iter().any(|r| *r == Rect { x: 10, y: 10, w: 4, h: 4 });
        assert!(has_pos, "damage must include surface's position");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);
        assert_eq!(backend.pixel(10, 10), RED, "shown surface should be visible");
    }

    // ================================================================
    // Test 4: Destroy surface → damage + buffer release
    // ================================================================
    #[test]
    fn destroy_surface_damage_and_buffer_release() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        scene.move_surface(TOKEN_A, 10, 10).expect("move");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(4, 4))
            .expect("present");

        let damage = scene.destroy_surface(TOKEN_A).expect("destroy");
        let rects = damage.rects();
        let has_pos = rects.iter().any(|r| *r == Rect { x: 10, y: 10, w: 4, h: 4 });
        assert!(has_pos, "destroy should damage the surface's position");

        // Further operations on destroyed surface should fail.
        assert_eq!(
            scene.move_surface(TOKEN_A, 0, 0).err(),
            Some(Error::InvalidCapability)
        );

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);
        assert_eq!(backend.pixel(10, 10), 0, "destroyed surface position should be black");
    }

    // ================================================================
    // Test 5: Z-order change → damage where occlusion changes
    // ================================================================
    #[test]
    fn z_order_change_damage_where_occlusion_changes() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create A");
        scene.create_surface(TOKEN_B, 4, 4, 16).expect("create B");
        scene.move_surface(TOKEN_A, 0, 0).expect("move A");
        scene.move_surface(TOKEN_B, 2, 2).expect("move B");
        scene.write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED)).expect("write A");
        scene.write_surface_buffer(TOKEN_B, 0, &fill_buffer(4, 4, 16, BLUE)).expect("write B");
        scene.present_surface(TOKEN_A, 0, full_damage(4, 4)).expect("present A");
        scene.present_surface(TOKEN_B, 0, full_damage(4, 4)).expect("present B");
        // A has z=0 (back), B has z=0 (same). Set B to z=1 (front).
        scene.set_z_order(TOKEN_A, 0).expect("z A");
        scene.set_z_order(TOKEN_B, 1).expect("z B");

        // Composite with B on top.
        let mut backend = make_backend();
        scene.composite_frame(&mut backend);
        // Overlap region (2,2)-(3,3): B (Blue) should be on top.
        assert_eq!(backend.pixel(2, 2), BLUE, "B should be on top in overlap");
        assert_eq!(backend.pixel(0, 0), RED, "A should be visible outside overlap");

        // Now change z: A to z=2 (front), B stays at z=1.
        let damage = scene.set_z_order(TOKEN_A, 2).expect("z A up");
        assert!(!damage.rects().is_empty(), "z-order change should produce damage");

        // Composite with A on top.
        let mut backend2 = make_backend();
        scene.composite_frame(&mut backend2);
        // Overlap region: A (Red) should now be on top.
        assert_eq!(backend2.pixel(2, 2), RED, "A should now be on top in overlap");
        assert_eq!(backend2.pixel(0, 0), RED, "A still visible outside overlap");
    }

    // ================================================================
    // Test 6: Clipping — surface partially off-screen
    // ================================================================
    #[test]
    fn clipping_surface_partially_off_screen() {
        let mut scene = make_scene();
        // Place a 4x4 surface at (OUT_W-2, OUT_H-2) — only 2x2 visible.
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        scene
            .move_surface(TOKEN_A, (OUT_W - 2) as i32, (OUT_H - 2) as i32)
            .expect("move");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(4, 4))
            .expect("present");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        // Visible 2x2 region should be RED.
        assert_eq!(backend.pixel(OUT_W - 2, OUT_H - 2), RED, "visible corner should be RED");
        assert_eq!(backend.pixel(OUT_W - 1, OUT_H - 1), RED, "visible corner should be RED");
        // Off-screen region simply doesn't exist in the output — no panic, no OOB.
        // Top-left should be black (no surface there).
        assert_eq!(backend.pixel(0, 0), 0, "uncovered area should be black");
    }

    // ================================================================
    // Test 6b: Clipping — negative position (partially off top-left)
    // ================================================================
    #[test]
    fn clipping_negative_position() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        scene.move_surface(TOKEN_A, -2, -2).expect("move");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, GREEN))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(4, 4))
            .expect("present");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        // Only (0,0)-(1,1) visible (2x2 of the 4x4 surface).
        assert_eq!(backend.pixel(0, 0), GREEN, "visible part of negative-position surface");
        assert_eq!(backend.pixel(1, 1), GREEN, "visible part of negative-position surface");
    }

    // ================================================================
    // Test 7: Occlusion — two overlapping surfaces, only top composited
    // ================================================================
    #[test]
    fn occlusion_two_overlapping_only_top_composited() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create A");
        scene.create_surface(TOKEN_B, 4, 4, 16).expect("create B");
        scene.move_surface(TOKEN_A, 0, 0).expect("move A");
        scene.move_surface(TOKEN_B, 2, 2).expect("move B");
        scene.set_z_order(TOKEN_A, 0).expect("z A back");
        scene.set_z_order(TOKEN_B, 1).expect("z B front");
        scene.write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED)).expect("write A");
        scene.write_surface_buffer(TOKEN_B, 0, &fill_buffer(4, 4, 16, BLUE)).expect("write B");
        scene.present_surface(TOKEN_A, 0, full_damage(4, 4)).expect("present A");
        scene.present_surface(TOKEN_B, 0, full_damage(4, 4)).expect("present B");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        // Non-overlap: A only.
        assert_eq!(backend.pixel(0, 0), RED, "A visible where no overlap");
        // Overlap (2,2)-(3,3): B on top.
        assert_eq!(backend.pixel(2, 2), BLUE, "B on top in overlap");
        assert_eq!(backend.pixel(3, 3), BLUE, "B on top in overlap");
        // Non-overlap: B only.
        assert_eq!(backend.pixel(5, 5), BLUE, "B visible where no overlap with A");
    }

    // ================================================================
    // Test 8: Unscaled copy — 1:1 pixel match
    // ================================================================
    #[test]
    fn unscaled_copy_1_to_1_pixel_match() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");

        // Write a checkerboard pattern.
        let mut buf = fill_buffer(4, 4, 16, 0);
        for y in 0..4 {
            for x in 0..4 {
                let pitch_words = 16 / 4;
                buf[y as usize * pitch_words + x as usize] = if (x + y) % 2 == 0 {
                    RED
                } else {
                    GREEN
                };
            }
        }
        scene.write_surface_buffer(TOKEN_A, 0, &buf).expect("write");
        scene.present_surface(TOKEN_A, 0, full_damage(4, 4)).expect("present");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        for y in 0..4 {
            for x in 0..4 {
                let expected = if (x + y) % 2 == 0 { RED } else { GREEN };
                assert_eq!(
                    backend.pixel(x, y),
                    expected,
                    "pixel ({},{}) should match 1:1",
                    x,
                    y
                );
            }
        }
    }

    // ================================================================
    // Test 9: Integer scale 2x — nearest-neighbor pixel match
    // ================================================================
    #[test]
    fn integer_scale_2x_nearest_neighbor() {
        let mut scene = make_scene();
        // 2x2 source, displayed at 4x4 (2x scale).
        scene.create_surface(TOKEN_A, 2, 2, 8).expect("create");
        scene.set_display_size(TOKEN_A, 4, 4).expect("display size");
        scene.move_surface(TOKEN_A, 0, 0).expect("move");

        // Source: (0,0)=Red, (1,0)=Green, (0,1)=Blue, (1,1)=White
        let mut buf = fill_buffer(2, 2, 8, 0);
        let pitch_words = 8 / 4;
        buf[0 * pitch_words + 0] = RED;
        buf[0 * pitch_words + 1] = GREEN;
        buf[1 * pitch_words + 0] = BLUE;
        buf[1 * pitch_words + 1] = WHITE;
        scene.write_surface_buffer(TOKEN_A, 0, &buf).expect("write");
        scene.present_surface(TOKEN_A, 0, full_damage(2, 2)).expect("present");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        // 2x nearest-neighbor: each source pixel → 2x2 block.
        // (0,0)=Red (1,0)=Red (0,1)=Red (1,1)=Red
        // (2,0)=Green (3,0)=Green (2,1)=Green (3,1)=Green
        // (0,2)=Blue (1,2)=Blue (0,3)=Blue (1,3)=Blue
        // (2,2)=White (3,2)=White (2,3)=White (3,3)=White
        assert_eq!(backend.pixel(0, 0), RED, "scale 2x (0,0)");
        assert_eq!(backend.pixel(1, 0), RED, "scale 2x (1,0)");
        assert_eq!(backend.pixel(0, 1), RED, "scale 2x (0,1)");
        assert_eq!(backend.pixel(1, 1), RED, "scale 2x (1,1)");
        assert_eq!(backend.pixel(2, 0), GREEN, "scale 2x (2,0)");
        assert_eq!(backend.pixel(3, 0), GREEN, "scale 2x (3,0)");
        assert_eq!(backend.pixel(2, 1), GREEN, "scale 2x (2,1)");
        assert_eq!(backend.pixel(3, 1), GREEN, "scale 2x (3,1)");
        assert_eq!(backend.pixel(0, 2), BLUE, "scale 2x (0,2)");
        assert_eq!(backend.pixel(1, 2), BLUE, "scale 2x (1,2)");
        assert_eq!(backend.pixel(0, 3), BLUE, "scale 2x (0,3)");
        assert_eq!(backend.pixel(1, 3), BLUE, "scale 2x (1,3)");
        assert_eq!(backend.pixel(2, 2), WHITE, "scale 2x (2,2)");
        assert_eq!(backend.pixel(3, 2), WHITE, "scale 2x (3,2)");
        assert_eq!(backend.pixel(2, 3), WHITE, "scale 2x (2,3)");
        assert_eq!(backend.pixel(3, 3), WHITE, "scale 2x (3,3)");
    }

    // ================================================================
    // Test 10: Odd pitch — pitch > width, correct row stride
    // ================================================================
    #[test]
    fn odd_pitch_correct_row_stride() {
        let mut scene = make_scene();
        // 4 pixels wide, but pitch = 8*4 = 32 bytes (8 words per row, 4 used).
        let pitch: u32 = 8 * 4;
        let width: u32 = 4;
        let height: u32 = 4;
        scene.create_surface(TOKEN_A, width, height, pitch).expect("create");
        scene.move_surface(TOKEN_A, 0, 0).expect("move");

        // Write with odd pitch: row 0 = [A,B,C,D,_,_,_,_], row 1 = [E,F,G,H,...]
        let mut buf = vec![0u32; (pitch / 4) as usize * height as usize];
        let pw = (pitch / 4) as usize;
        for x in 0..width as usize {
            buf[0 * pw + x] = RED;
            buf[1 * pw + x] = GREEN;
            buf[2 * pw + x] = BLUE;
            buf[3 * pw + x] = WHITE;
        }
        // Padding bytes should not appear in output.
        for y in 0..height as usize {
            for x in width as usize..pw {
                buf[y * pw + x] = 0xDEAD_BEEF;
            }
        }
        scene.write_surface_buffer(TOKEN_A, 0, &buf).expect("write");
        scene.present_surface(TOKEN_A, 0, full_damage(width, height)).expect("present");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        // Row 0 should be RED, row 1 GREEN, etc. — padding must not bleed.
        for x in 0..width {
            assert_eq!(backend.pixel(x, 0), RED, "row 0 pixel {} with odd pitch", x);
            assert_eq!(backend.pixel(x, 1), GREEN, "row 1 pixel {} with odd pitch", x);
            assert_eq!(backend.pixel(x, 2), BLUE, "row 2 pixel {} with odd pitch", x);
            assert_eq!(backend.pixel(x, 3), WHITE, "row 3 pixel {} with odd pitch", x);
        }
    }

    // ================================================================
    // Test 11: RGB channel pattern — specific color values verified
    // ================================================================
    #[test]
    fn rgb_channel_pattern_specific_values() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 3, 1, 12).expect("create");
        scene.move_surface(TOKEN_A, 0, 0).expect("move");

        // Three pixels: pure Red, pure Green, pure Blue.
        let mut buf = vec![0u32; 3];
        buf[0] = 0x00FF_0000; // Red
        buf[1] = 0x0000_FF00; // Green
        buf[2] = 0x0000_00FF; // Blue
        scene.write_surface_buffer(TOKEN_A, 0, &buf).expect("write");
        scene.present_surface(TOKEN_A, 0, full_damage(3, 1)).expect("present");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        assert_eq!(backend.pixel(0, 0), 0x00FF_0000, "pixel 0 should be pure Red");
        assert_eq!(backend.pixel(1, 0), 0x0000_FF00, "pixel 1 should be pure Green");
        assert_eq!(backend.pixel(2, 0), 0x0000_00FF, "pixel 2 should be pure Blue");
    }

    // ================================================================
    // Test 12: Multiple surfaces — 3+ surfaces composited correctly
    // ================================================================
    #[test]
    fn multiple_surfaces_composited_correctly() {
        let mut scene = make_scene();
        // Three non-overlapping surfaces at different positions.
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create A");
        scene.create_surface(TOKEN_B, 4, 4, 16).expect("create B");
        scene.create_surface(TOKEN_C, 4, 4, 16).expect("create C");
        scene.move_surface(TOKEN_A, 0, 0).expect("move A");
        scene.move_surface(TOKEN_B, 8, 0).expect("move B");
        scene.move_surface(TOKEN_C, 16, 0).expect("move C");
        scene.write_surface_buffer(TOKEN_A, 0, &fill_buffer(4, 4, 16, RED)).expect("write A");
        scene.write_surface_buffer(TOKEN_B, 0, &fill_buffer(4, 4, 16, GREEN)).expect("write B");
        scene.write_surface_buffer(TOKEN_C, 0, &fill_buffer(4, 4, 16, BLUE)).expect("write C");
        scene.present_surface(TOKEN_A, 0, full_damage(4, 4)).expect("present A");
        scene.present_surface(TOKEN_B, 0, full_damage(4, 4)).expect("present B");
        scene.present_surface(TOKEN_C, 0, full_damage(4, 4)).expect("present C");

        let mut backend = make_backend();
        scene.composite_frame(&mut backend);

        // Surface A: RED at (0,0)-(3,3)
        assert_eq!(backend.pixel(0, 0), RED, "surface A at (0,0)");
        assert_eq!(backend.pixel(3, 3), RED, "surface A at (3,3)");
        // Surface B: GREEN at (8,0)-(11,3)
        assert_eq!(backend.pixel(8, 0), GREEN, "surface B at (8,0)");
        assert_eq!(backend.pixel(11, 3), GREEN, "surface B at (11,3)");
        // Surface C: BLUE at (16,0)-(19,3)
        assert_eq!(backend.pixel(16, 0), BLUE, "surface C at (16,0)");
        assert_eq!(backend.pixel(19, 3), BLUE, "surface C at (19,3)");
        // Gap between A and B: black
        assert_eq!(backend.pixel(4, 0), 0, "gap between A and B");
        assert_eq!(backend.pixel(7, 0), 0, "gap between A and B");
    }

    // ================================================================
    // Test 13: Overlay re-applied after every dirty pass (cursor invariant)
    // ================================================================
    #[test]
    fn overlay_reapplied_after_dirty_pass() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 8, 8, 32).expect("create surface");
        scene.move_surface(TOKEN_A, 0, 0).expect("move");
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(8, 8, 32, RED))
            .expect("write");
        scene
            .present_surface(TOKEN_A, 0, full_damage(8, 8))
            .expect("present");

        // Add a 2x2 yellow overlay (cursor) at (2, 2).
        let overlay_pixels = fill_buffer(2, 2, 8, YELLOW);
        scene.add_overlay(Overlay::new(overlay_pixels, 2, 2, 2, 2, 8));

        // First frame: overlay should appear on top of RED.
        let mut backend = make_backend();
        scene.composite_frame(&mut backend);
        assert_eq!(backend.pixel(2, 2), YELLOW, "overlay should be on top");
        assert_eq!(backend.pixel(0, 0), RED, "surface should be visible around overlay");

        // Now re-present the surface (damage includes the area under the overlay).
        scene
            .write_surface_buffer(TOKEN_A, 0, &fill_buffer(8, 8, 32, BLUE))
            .expect("write blue");
        scene
            .present_surface(TOKEN_A, 0, full_damage(8, 8))
            .expect("present blue");

        // Second frame: overlay must STILL be on top (re-applied after dirty pass).
        let mut backend2 = make_backend();
        scene.composite_frame(&mut backend2);
        assert_eq!(backend2.pixel(2, 2), YELLOW, "overlay must be re-applied after dirty pass");
        assert_eq!(backend2.pixel(0, 0), BLUE, "surface content updated to blue");
    }

    // ================================================================
    // Test 14: Damage union — overlapping rects merged, bounding-box fallback
    // ================================================================
    #[test]
    fn damage_union_overlapping_merged() {
        let mut acc = DamageAccumulator::new();
        acc.add(Rect { x: 0, y: 0, w: 10, h: 10 });
        acc.add(Rect { x: 5, y: 5, w: 10, h: 10 });
        let dl = acc.take();
        // These overlap, should merge to a single rect.
        assert_eq!(dl.count, 1, "overlapping rects should merge");
        assert_eq!(dl.rects[0], Rect { x: 0, y: 0, w: 15, h: 15 });
    }

    #[test]
    fn damage_union_bounding_box_fallback() {
        let mut acc = DamageAccumulator::new();
        // Add 10 non-overlapping rects → should fall back to bounding box.
        for i in 0..10u32 {
            acc.add(Rect { x: i * 20, y: 0, w: 5, h: 5 });
        }
        let dl = acc.take();
        assert!(dl.bounding_fallback, "should fall back to bounding box for >8 rects");
        assert_eq!(dl.count, 1, "bounding box should be a single rect");
    }

    // ================================================================
    // Test 15: Content → scene damage coordinate translation invariant
    // ================================================================
    #[test]
    fn content_to_scene_damage_translation() {
        let mut acc = DamageAccumulator::new();
        // Content damage at (2, 3, 4, 5) with surface offset (10, 10).
        let content = DamageList::from_rects(&[Rect { x: 2, y: 3, w: 4, h: 5 }]);
        acc.add_content_damage(&content, 10, 10, OUT_W, OUT_H);
        let dl = acc.take();
        assert_eq!(dl.count, 1);
        // Scene damage = content + offset = (12, 13, 4, 5).
        assert_eq!(dl.rects[0], Rect { x: 12, y: 13, w: 4, h: 5 });
    }

    #[test]
    fn content_to_scene_damage_clipped_to_output() {
        let mut acc = DamageAccumulator::new();
        // Content damage that extends beyond output bounds.
        let content = DamageList::from_rects(&[Rect { x: 0, y: 0, w: 100, h: 100 }]);
        acc.add_content_damage(&content, 28, 20, OUT_W, OUT_H);
        let dl = acc.take();
        assert_eq!(dl.count, 1);
        // Clipped to 32x24 output: (28, 20, 4, 4).
        assert_eq!(dl.rects[0], Rect { x: 28, y: 20, w: 4, h: 4 });
    }

    // ================================================================
    // Test 16: Foreign/destroyed surface operations return errors
    // ================================================================
    #[test]
    fn foreign_surface_operation_returns_error() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        // TOKEN_B was never created.
        assert_eq!(
            scene.move_surface(TOKEN_B, 0, 0).err(),
            Some(Error::InvalidCapability)
        );
    }

    #[test]
    fn double_create_same_token_returns_error() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create A");
        assert_eq!(
            scene.create_surface(TOKEN_A, 4, 4, 16).err(),
            Some(Error::ForeignSurface)
        );
    }

    // ================================================================
    // Test 17: Surface validation (via SurfaceState)
    // ================================================================
    #[test]
    fn surface_creation_validates_dimensions() {
        let mut scene = make_scene();
        // Zero width.
        assert_eq!(
            scene.create_surface(TOKEN_A, 0, 10, 40).err(),
            Some(Error::InvalidRect)
        );
        // Pitch overflow.
        assert_eq!(
            scene.create_surface(TOKEN_A, 100, 100, u32::MAX).err(),
            Some(Error::PitchOverflow)
        );
    }

    // ================================================================
    // Test 18: Buffer write validation
    // ================================================================
    #[test]
    fn buffer_write_out_of_range_returns_error() {
        let mut scene = make_scene();
        scene.create_surface(TOKEN_A, 4, 4, 16).expect("create");
        // Buffer index out of range.
        assert_eq!(
            scene.write_surface_buffer(TOKEN_A, 5, &[0]).err(),
            Some(Error::BufferOverflow)
        );
        // Pixels larger than buffer.
        let big = vec![0u32; 1000];
        assert_eq!(
            scene.write_surface_buffer(TOKEN_A, 0, &big).err(),
            Some(Error::InvalidRect)
        );
    }
}
