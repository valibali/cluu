//! Window lifecycle, focus management, VT switching, and input forwarding.
//!
//! All methods are `impl Compositor` blocks; the type itself lives in `state`.

use crate::state::{Compositor, DragMode, DragState, ShmMapping, Window, WindowId, WindowPixelRegion, WindowShm, WIN_SHM_MAGIC, WIN_SHM_VERSION, GLYPH_W, GLYPH_H};
use libcluu::Result;

impl Compositor {
    /// Allocate a window per the request. Returns
    /// `(id, frame_token, granted_w, granted_h)` on success.
    ///
    /// Granted dims are clamped to the screen minus row 0 (status bar) for
    /// normal windows. If `flags` has `COMP_WIN_FLAG_FULLSCREEN` set, the
    /// window covers the full cell grid (x=0, y=0, w=cols, h=rows) and no
    /// chrome or status bar will be drawn while it is focused.
    /// `owner_pid` is the authenticated sender's tid (CLUU does not yet
    /// distinguish tid from pid for one-thread apps).
    /// `input_endpoint` is the app's long-lived endpoint for FRAME_READY and INPUT_FORWARD.
    pub fn handle_win_register(
        &mut self,
        owner_pid: u32,
        req_w: u32,
        req_h: u32,
        title: &str,
        input_endpoint: usize,
        flags: u32,
    ) -> Result<(WindowId, u64, u32, u32)> {
        let fullscreen = (flags & libcluu::ipc::COMP_WIN_FLAG_FULLSCREEN) != 0;
        let no_chrome = (flags & libcluu::ipc::COMP_WIN_FLAG_NO_CHROME) != 0;
        let modal = (flags & libcluu::ipc::COMP_WIN_FLAG_MODAL) != 0;
        let (granted_w, granted_h) = if fullscreen {
            (self.cols, self.rows)
        } else {
            let gw = (req_w as u16).min(self.cols);
            let gh = (req_h as u16).min(self.rows.saturating_sub(1));
            (gw, gh)
        };
        // Minimum 3×3: 1-cell chrome on each side + at least 1 interior cell.
        if granted_w < 3 || granted_h < 3 {
            return Err(libcluu::Error::InvalidArgument);
        }

        let max_cells_bytes = self.cols as usize * self.rows as usize * 8;
        let header_bytes = core::mem::size_of::<WindowShm>();
        let total_bytes = header_bytes + max_cells_bytes;
        let (token, allocated) = crate::shm::alloc_frame(total_bytes)?;

        let id = self.next_id;
        self.next_id += 1;

        // Per-window VA slot, well above APP_FB_BASE. Each id reserves a
        // 4 MiB stride so neighbouring windows never collide regardless of
        // their pixel dimensions. 256 MiB region total before we run out.
        let va_base: usize = 0xC100_0000;
        let va = va_base + (id as usize) * 0x40_0000;
        crate::shm::map_frame_rw(va, token, allocated)?;

        // Build the ShmMapping before we initialise the header — we need
        // as_ptr() to write the WindowShm fields.
        let mapping = ShmMapping::new(va, allocated)
            .ok_or(libcluu::Error::InvalidArgument)?;

        unsafe {
            let hdr = mapping.as_ptr() as *mut WindowShm;
            (*hdr).magic = WIN_SHM_MAGIC;
            (*hdr).version = WIN_SHM_VERSION;
            (*hdr).width = granted_w as u32;
            (*hdr).height = granted_h as u32;
            (*hdr).cursor_x = 0;
            (*hdr).cursor_y = 0;
            (*hdr).cursor_visible = 0;
            (*hdr).generation = 0;
            // Zero cell area
            let cells_ptr = mapping.as_ptr().add(header_bytes);
            core::ptr::write_bytes(cells_ptr, 0, max_cells_bytes);
        }

        // Fullscreen windows are pinned at (0, 0). Normal windows cascade,
        // respecting row 0 as the status bar (y >= 1). The cascade step is
        // chosen to be obviously visible — id*2 made the second window
        // overlap the first by ≥ 95 % of its area, making Alt+Tab focus
        // changes look like nothing happened. id*8 cells / id*3 rows leaves
        // enough title bar and side chrome of every prior window peeking
        // out so users can see all windows at once.
        let (x, y) = if fullscreen {
            (0u16, 0u16)
        } else if modal {
            let x = (self.cols.saturating_sub(granted_w)) / 2;
            let y = 1 + (self.rows.saturating_sub(1).saturating_sub(granted_h)) / 2;
            (x, y)
        } else {
            let win_index = self.windows.len() as u16;
            let max_x = self.cols.saturating_sub(granted_w);
            let max_y = self.rows.saturating_sub(granted_h);
            let step = 8u16;
            let per_row = (max_x / step).max(1) + 1;
            let col = win_index % per_row;
            let row = win_index / per_row;
            let x = col.saturating_mul(step).min(max_x);
            let y = (1 + row.saturating_mul(3)).min(max_y.max(1));
            (x, y)
        };

        let mut title_owned = alloc::string::String::new();
        title_owned.push_str(title);
        if title_owned.len() > 31 {
            title_owned.truncate(31);
        }

        let new_win = Window {
            id,
            owner_pid,
            title: title_owned,
            x,
            y,
            w: granted_w,
            h: granted_h,
            mapping,
            shm_token: token,
            last_gen: 0,
            input_endpoint,
            pending_frame_ready: false,
            fullscreen,
            no_chrome,
            modal,
            session_id: None,
            pixel_region: None,
        };
        // Modal windows go to z-top (end of Vec). Non-modal windows are
        // inserted before the first existing modal so modals stay on top.
        if modal {
            self.windows.push(new_win);
        } else {
            let pos = self.windows.iter().position(|w| w.modal).unwrap_or(self.windows.len());
            self.windows.insert(pos, new_win);
        }
        // Mark the previously-focused window's cells dirty so its chrome
        // re-renders as unfocused (single-line borders) after focus moves.
        if let Some(prev_id) = self.focused {
            if let Some(prev) = self.windows.iter().find(|w| w.id == prev_id) {
                let (px, py, pw, ph) = (prev.x, prev.y, prev.w, prev.h);
                for cy in py..py.saturating_add(ph) {
                    for cx in px..px.saturating_add(pw) {
                        self.cell_dirty.push((cx, cy));
                    }
                }
            }
        }
        self.focused = Some(id);
        // Mark all the window's cells dirty so the (eventual) compose pass
        // emits chrome + interior.
        for cy in y..y.saturating_add(granted_h) {
            for cx in x..x.saturating_add(granted_w) {
                self.cell_dirty.push((cx, cy));
            }
        }

        // Notify app of initial interior dimensions via WIN_CONFIGURE.
        // Normal windows: interior = total - (chrome + padding) on each axis.
        // Modal windows: compose_cell suppresses chrome and reads SHM at
        // (local_x, local_y) directly, so the interior IS the full grant.
        if input_endpoint != 0 && granted_w > 2 && granted_h > 2 {
            let (iw_off, ih_off): (u16, u16) = if modal { (0, 0) } else { (4, 3) };
            let interior_w = granted_w.saturating_sub(iw_off);
            let interior_h = granted_h.saturating_sub(ih_off);
            let msg = libcluu::types::Message::new(
                libcluu::ipc::COMP_WIN_CONFIGURE_LABEL,
                [
                    id as usize,
                    interior_w as usize,
                    interior_h as usize,
                    0, 0, 0,
                ],
                3,
            );
            let _ = libcluu::ipc::send(input_endpoint, &msg, libcluu::types::IpcFlags::empty());
        }

        Ok((id, token, granted_w as u32, granted_h as u32))
    }
}

impl Compositor {
    /// App says "I redrew (x,y,w,h) inside my window's interior". Mark
    /// the corresponding total-grid cells dirty.
    ///
    /// Interior coords are 0-based relative to the window's interior origin.
    /// For normal windows that's local (2, 1) (chrome + padding); for modal
    /// windows it's local (0, 0) because `compose_cell` suppresses chrome
    /// and reads SHM at `(local_x, local_y)` directly. The chrome offset
    /// here MUST match `compose_cell`'s suppression decision, otherwise the
    /// cells where the client draws its own modal border are never dirtied
    /// by WIN_DAMAGE and stay stuck at the register-time compose (empty SHM)
    /// until something else damages them (e.g. mouse hover).
    /// See [[cluu-modal-damage-clamps-border-out]].
    pub fn handle_win_damage(&mut self, id: WindowId, x: u32, y: u32, w: u32, h: u32) {
        let Some(win) = self.windows.iter_mut().find(|w| w.id == id) else { return; };
        let has_pixel_region = win.pixel_region.is_some();
        let chrome_off_x: u16 = if win.modal { 0 } else { 2 };
        let chrome_off_y: u16 = if win.modal { 0 } else { 1 };
        let inner_w = win.w.saturating_sub(chrome_off_x * 2);
        let inner_h = win.h.saturating_sub(chrome_off_y * 2);
        let cx0 = (x as u16).min(inner_w);
        let cy0 = (y as u16).min(inner_h);
        let cx1 = ((x as u16).saturating_add(w as u16)).min(inner_w);
        let cy1 = ((y as u16).saturating_add(h as u16)).min(inner_h);
        win.pending_frame_ready = true;
        if has_pixel_region {
            self.pixel_dirty = true;
        }
        let (win_x, win_y) = (win.x, win.y);
        for iy in cy0..cy1 {
            for ix in cx0..cx1 {
                let gx = win_x + chrome_off_x + ix;
                let gy = win_y + chrome_off_y + iy;
                self.cell_dirty.push((gx, gy));
            }
        }
    }
}

impl Compositor {
    /// Update the title of a window and dirty the title row so chrome re-renders.
    pub fn handle_win_set_title(&mut self, id: WindowId, title: &str) {
        let win_idx = match self.windows.iter().position(|w| w.id == id) {
            Some(i) => i,
            None => return,
        };
        // Truncate to fit title strip (<=31 chars matches the storage cap
        // in handle_win_register).
        let safe = if title.len() > 31 { &title[..31] } else { title };
        self.windows[win_idx].title.clear();
        self.windows[win_idx].title.push_str(safe);
        let win = &self.windows[win_idx];
        // Title is in the top chrome row (ly=0), so global y = win.y.
        let title_y = win.y;
        for cx in win.x..win.x.saturating_add(win.w) {
            self.cell_dirty.push((cx, title_y));
        }
    }
}

impl Compositor {
    /// Cycle focus forward (Alt+Tab). The newly focused window is moved to the
    /// top of the z-order (end of the `windows` Vec) and the grid is fully
    /// dirtied so chrome repaints with the updated focus state.
    /// Modal windows grab focus — Alt+Tab is disabled while a modal is focused.
    pub fn focus_next(&mut self) {
        if self.windows.is_empty() { return; }
        if self.focused_is_modal() { return; }
        let cur = self.focused;
        let pos = cur
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .unwrap_or(0);
        let new = (pos + 1) % self.windows.len();
        let win = self.windows.remove(new);
        let id = win.id;
        self.windows.push(win);
        self.focused = Some(id);
        let _ = libcluu::debug_print(&alloc::format!("compositor: focus -> {}", id));
        self.repaint_all();
    }

    /// Cycle focus backward (Alt+Shift+Tab).
    /// Modal windows grab focus — Alt+Shift+Tab is disabled while a modal is focused.
    pub fn focus_prev(&mut self) {
        if self.windows.is_empty() { return; }
        if self.focused_is_modal() { return; }
        let len = self.windows.len();
        let pos = self.focused
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .unwrap_or(0);
        let new = (pos + len - 1) % len;
        let win = self.windows.remove(new);
        let id = win.id;
        self.windows.push(win);
        self.focused = Some(id);
        let _ = libcluu::debug_print(&alloc::format!("compositor: focus -> {}", id));
        self.repaint_all();
    }

    /// Move the focused window by (dx, dy) cells, clamped to screen bounds.
    /// Row 0 is the status bar; window top edge may not go above row 1.
    pub fn move_focused(&mut self, dx: i16, dy: i16) {
        let Some(id) = self.focused else { return; };
        let pos = match self.windows.iter().position(|w| w.id == id) {
            Some(p) => p,
            None => return,
        };
        let win = &self.windows[pos];
        let new_x = (win.x as i32 + dx as i32)
            .max(0)
            .min(self.cols as i32 - win.w as i32) as u16;
        let new_y = (win.y as i32 + dy as i32)
            .max(1)
            .min(self.rows as i32 - win.h as i32) as u16;
        let old_x = win.x;
        let old_y = win.y;
        let w = win.w;
        let h = win.h;
        self.windows[pos].x = new_x;
        self.windows[pos].y = new_y;
        // Dirty old and new footprints.
        for cy in old_y..old_y.saturating_add(h) {
            for cx in old_x..old_x.saturating_add(w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        for cy in new_y..new_y.saturating_add(h) {
            for cx in new_x..new_x.saturating_add(w) {
                self.cell_dirty.push((cx, cy));
            }
        }
    }

    /// Resize the focused window by (dw, dh) cells.
    /// Minimum size is 5×5; maximum is clamped to the screen edge.
    pub fn resize_focused(&mut self, dw: i16, dh: i16) {
        let Some(id) = self.focused else { return; };
        let pos = match self.windows.iter().position(|w| w.id == id) {
            Some(p) => p,
            None => return,
        };
        let win = &self.windows[pos];
        let new_w = ((win.w as i32 + dw as i32)
            .max(3)
            .min(self.cols as i32 - win.x as i32)) as u16;
        let new_h = ((win.h as i32 + dh as i32)
            .max(3)
            .min(self.rows as i32 - win.y as i32)) as u16;
        let old_w = win.w;
        let old_h = win.h;
        let x = win.x;
        let y = win.y;
        self.windows[pos].w = new_w;
        self.windows[pos].h = new_h;
        // Dirty the union of old and new footprints.
        let max_w = old_w.max(new_w);
        let max_h = old_h.max(new_h);
        for cy in y..y.saturating_add(max_h) {
            for cx in x..x.saturating_add(max_w) {
                self.cell_dirty.push((cx, cy));
            }
        }

        // Notify the app about the new interior dimensions via WIN_CONFIGURE.
        let input_ep = self.windows[pos].input_endpoint;
        if input_ep != 0 && new_w > 2 && new_h > 2 {
            let is_modal = self.windows[pos].modal;
            let (iw_off, ih_off): (u16, u16) = if is_modal { (0, 0) } else { (4, 3) };
            let interior_w = new_w.saturating_sub(iw_off);
            let interior_h = new_h.saturating_sub(ih_off);
            {
                let hdr = self.windows[pos].mapping.as_ptr() as *mut WindowShm;
                unsafe {
                    core::ptr::write_volatile(&mut (*hdr).width as *mut u32, interior_w as u32);
                    core::ptr::write_volatile(&mut (*hdr).height as *mut u32, interior_h as u32);
                }
            }
            let msg = libcluu::types::Message::new(
                libcluu::ipc::COMP_WIN_CONFIGURE_LABEL,
                [
                    id as usize,
                    interior_w as usize,
                    interior_h as usize,
                    0, 0, 0,
                ],
                3,
            );
            let _ = libcluu::ipc::send(input_ep, &msg, libcluu::types::IpcFlags::empty());
        }
    }

    /// Mark every cell on screen dirty (used after focus changes so chrome
    /// repaints with correct focused/unfocused colours).
    pub fn resize_window_by_id(&mut self, window_id: u64, cols: u16, rows: u16) {
        let pos = match self.windows.iter().position(|w| w.id == window_id) {
            Some(p) => p,
            None => return,
        };
        let win = &self.windows[pos];
        let is_modal = win.modal;
        let (iw_off, ih_off): (u16, u16) = if is_modal { (0, 0) } else { (4, 3) };
        let new_w = cols.saturating_add(iw_off);
        let new_h = rows.saturating_add(ih_off);
        let x = win.x;
        let y = win.y;
        let old_w = win.w;
        let old_h = win.h;
        let clamped_w = new_w.min((self.cols as i32 - x as i32) as u16);
        let clamped_h = new_h.min((self.rows as i32 - y as i32) as u16);
        self.windows[pos].w = clamped_w;
        self.windows[pos].h = clamped_h;
        let max_w = old_w.max(clamped_w);
        let max_h = old_h.max(clamped_h);
        for cy in y..y.saturating_add(max_h) {
            for cx in x..x.saturating_add(max_w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        let input_ep = self.windows[pos].input_endpoint;
        if input_ep != 0 && clamped_w > iw_off && clamped_h > ih_off {
            let interior_w = clamped_w.saturating_sub(iw_off);
            let interior_h = clamped_h.saturating_sub(ih_off);
            {
                let hdr = self.windows[pos].mapping.as_ptr() as *mut WindowShm;
                unsafe {
                    core::ptr::write_volatile(&mut (*hdr).width as *mut u32, interior_w as u32);
                    core::ptr::write_volatile(&mut (*hdr).height as *mut u32, interior_h as u32);
                }
            }
            let msg = libcluu::types::Message::new(
                libcluu::ipc::COMP_WIN_CONFIGURE_LABEL,
                [
                    window_id as usize,
                    interior_w as usize,
                    interior_h as usize,
                    0, 0, 0,
                ],
                3,
            );
            let _ = libcluu::ipc::send(input_ep, &msg, libcluu::types::IpcFlags::empty());
        }
    }

    pub fn repaint_all(&mut self) {
        for cy in 0..self.rows {
            for cx in 0..self.cols {
                self.cell_dirty.push((cx, cy));
            }
        }
    }
}

impl Compositor {
    /// VT switch: compositor's VT became active — resume fb writes.
    /// Invalidate `prev_cell_grid` to force every cell to re-blit, since the
    /// console (or another VT owner) overwrote the framebuffer while we were
    /// inactive; our cached grid no longer matches what's on-screen.
    pub fn handle_vt_activate(&mut self) {
        self.active = true;
        for slot in self.prev_cell_grid.iter_mut() {
            *slot = u64::MAX;
        }
        self.repaint_all();
    }

    /// VT switch: compositor's VT became inactive — suppress fb writes.
    pub fn handle_vt_deactivate(&mut self) {
        self.active = false;
    }
}

impl Compositor {
    /// True if the currently focused window is modal.
    pub fn focused_is_modal(&self) -> bool {
        let Some(id) = self.focused else { return false; };
        self.windows.iter().any(|w| w.id == id && w.modal)
    }

    /// Forward a raw kbd event to the focused window's input endpoint.
    /// `ascii`/`mods`/`scancode`/`extended` come straight from the
    /// `KbdEvent` variant of `protocol::Incoming`.
    pub fn forward_input_event(&self, ascii: u8, mods: u8, scancode: u8, extended: u8, kind: u32) {
        let Some(id) = self.focused else { return; };
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        if win.input_endpoint == 0 { return; }
        let msg = libcluu::types::Message::new(
            libcluu::ipc::COMP_INPUT_FORWARD_LABEL,
            [
                id as usize,
                ascii as usize,
                mods as usize,
                scancode as usize,
                extended as usize,
                kind as usize,
            ],
            6,
        );
        let _ = libcluu::ipc::send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty());
    }

    /// Send a close-request to the focused window's input endpoint.
    pub fn forward_close_request(&self) {
        let Some(id) = self.focused else { return; };
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        if win.input_endpoint == 0 { return; }
        let msg = libcluu::types::Message::new(
            libcluu::ipc::COMP_INPUT_FORWARD_LABEL,
            [id as usize, 0, 0, 0, 0, 99usize /* kind = 99 → close-request */],
            6,
        );
        let _ = libcluu::ipc::send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty());
    }
}

impl Compositor {
    /// Spawn a new cluuterm container via procmgr. The new cluuterm
    /// auto-registers a window with the compositor on startup.
    /// Uses the same payload format as vtmgr's spawn_vt_container:
    /// NUL-terminated image name, no param overrides.
    pub fn spawn_cluuterm(&self) {
        use procmgr_common::{labels::SESSION_PROCMGR_SPAWN_LABEL, wire::SpawnReq};

        let sid: u32 = 1;
        let spawn_ep_name = alloc::format!("session-procmgr:spawn:{}", sid);
        let ep = match libcluu::registry::lookup_service(&spawn_ep_name) {
            Some(ep) => ep,
            None => {
                let _ = libcluu::debug_print(&alloc::format!(
                    "compositor: spawn_cluuterm: no {} — falling back to root-procmgr",
                    spawn_ep_name
                ));
                self.spawn_cluuterm_via_root();
                return;
            }
        };

        let req = SpawnReq {
            image_path: alloc::string::String::from("/bin/cluuterm"),
            argv: alloc::vec![alloc::string::String::from("cluuterm")],
            envp: alloc::vec![
                (alloc::string::String::from("TERM"),
                 alloc::string::String::from("xterm-256color")),
                (alloc::string::String::from("CLUU_SESSION_ID"),
                 alloc::format!("{}", sid)),
            ],
            cwd: alloc::string::String::from("/"),
            fd_inherit: alloc::vec::Vec::new(),
            notify: None,
        };

        let payload = match postcard::to_allocvec(&req) {
            Ok(b) => b,
            Err(_) => {
                let _ = libcluu::debug_print("compositor: spawn_cluuterm: SpawnReq serialize failed");
                return;
            }
        };

        let msg = libcluu::types::Message::new(
            SESSION_PROCMGR_SPAWN_LABEL,
            [payload.len(), 0, 0, 0, 0, 0],
            0,
        );
        let _ = libcluu::ipc::send_msg_with_payload(ep, &msg, &payload);
        let _ = libcluu::debug_print("compositor: spawn_cluuterm: requested cluuterm via session-procmgr");
    }

    fn spawn_cluuterm_via_root(&self) {
        let ep = match libcluu::registry::lookup_service("root-procmgr:spawn") {
            Some(ep) => ep,
            None => {
                let _ = libcluu::debug_print("compositor: spawn_cluuterm: no root-procmgr:spawn");
                return;
            }
        };
        let payload = b"cluuterm\0";
        let msg = libcluu::types::Message::new(
            libcluu::ipc::PROCMGR_CONTAINER_RUN_LABEL,
            [payload.len(), 0, 0, payload.len(), 0, 0],
            5,
        );
        let _ = libcluu::ipc::send_msg_with_payload(ep, &msg, payload);
        let _ = libcluu::debug_print("compositor: spawn_cluuterm: requested cluuterm via root-procmgr");
    }
}

impl Compositor {
    /// Free the window's frame, drop it from the list, repaint covered cells.
    /// Called explicitly via WIN_DESTROY. Implicit destroy on owner-exit is
    /// deferred — would need procmgr to broadcast exits to non-spawner
    /// watchers (no such API today).
    pub fn handle_win_destroy(&mut self, id: WindowId) {
        let Some(pos) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        let win = self.windows.remove(pos);
        let _ = crate::shm::free_frame(win.shm_token);
        // Mark covered cells dirty so the next compose pass repaints bg.
        for cy in win.y..win.y.saturating_add(win.h) {
            for cx in win.x..win.x.saturating_add(win.w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        if self.focused == Some(id) {
            self.focused = self.windows.last().map(|w| w.id);
        }
    }
}

const BTN_LEFT: u8 = 1 << 0;

impl Compositor {
    pub fn handle_mouse_event(&mut self, dx: i32, dy: i32, buttons: u8) {
        static FIRST_EVENT_LOGGED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        if !FIRST_EVENT_LOGGED.swap(true, core::sync::atomic::Ordering::Relaxed) {
            let _ = libcluu::debug_print("compositor: first mouse event received");
        }

        let old_cell = self.cursor_cell();

        self.pointer_x = (self.pointer_x + dx).max(0).min(self.width_px as i32 - 1);
        self.pointer_y = (self.pointer_y + dy).max(0).min(self.height_px as i32 - 1);

        let left_pressed  = (buttons & BTN_LEFT) != 0 && (self.pointer_buttons & BTN_LEFT) == 0;
        let left_released = (buttons & BTN_LEFT) == 0 && (self.pointer_buttons & BTN_LEFT) != 0;
        self.pointer_buttons = buttons;

        let new_cell = self.cursor_cell();

        if left_pressed {
            if let Some(win_info) = self.window_at(new_cell.0, new_cell.1) {
                let is_modal = self.windows.iter()
                    .find(|w| w.id == win_info.id)
                    .map(|w| w.modal)
                    .unwrap_or(false);
                self.focus_window(win_info.id);
                if !is_modal {
                    let mode = if self.is_resize_handle(win_info, new_cell.0, new_cell.1) {
                        DragMode::Resize
                    } else {
                        DragMode::Move
                    };
                    self.drag_state = Some(DragState {
                        window_id: win_info.id,
                        mode,
                        start_cell_x: new_cell.0,
                        start_cell_y: new_cell.1,
                        start_win_x: win_info.x,
                        start_win_y: win_info.y,
                        start_win_w: win_info.w,
                        start_win_h: win_info.h,
                    });
                }
            }
        }

        if left_released {
            self.drag_state = None;
        }

        if self.drag_state.is_some() && (new_cell.0 != old_cell.0 || new_cell.1 != old_cell.1) {
            self.apply_drag(new_cell);
        }

        if old_cell != new_cell {
            self.cell_dirty.push(old_cell);
        }
        self.cell_dirty.push(new_cell);
        self.cursor_needs_render = true;
    }

    pub fn cursor_cell(&self) -> (u16, u16) {
        let cx = (self.pointer_x / GLYPH_W as i32) as u16;
        let cy = (self.pointer_y / GLYPH_H as i32) as u16;
        (cx.min(self.cols.saturating_sub(1)), cy.min(self.rows.saturating_sub(1)))
    }

    #[allow(dead_code)]
    // rationale: cursor-dirty helper for future incremental cursor rendering.
    pub fn dirty_cursor_cell(&mut self) {
        let (cx, cy) = self.cursor_cell();
        self.cell_dirty.push((cx, cy));
        self.cursor_needs_render = true;
    }

    pub fn render_cursor(&mut self) {
        let (cx, cy) = self.cursor_cell();
        let idx = cy as usize * self.cols as usize + cx as usize;
        if idx < self.cell_grid.len() {
            let cell = self.cell_grid[idx];
            let _bg = (cell >> 29) & 0xFF;
            self.cell_grid[idx] = (cell & !((0x1F_FFFF) | (0xFFu64 << 21)))
                | (0x2588u64)
                | (15u64 << 21);
        }
    }

    fn window_at(&self, cx: u16, cy: u16) -> Option<WinInfo> {
        for win in self.windows.iter().rev() {
            if cx >= win.x && cx < win.x.saturating_add(win.w)
                && cy >= win.y && cy < win.y.saturating_add(win.h)
            {
                return Some(WinInfo { id: win.id, x: win.x, y: win.y, w: win.w, h: win.h });
            }
        }
        None
    }

    fn is_resize_handle(&self, win: WinInfo, cx: u16, cy: u16) -> bool {
        cx == win.x.saturating_add(win.w).saturating_sub(1)
            || cy == win.y.saturating_add(win.h).saturating_sub(1)
    }

    fn focus_window(&mut self, id: WindowId) {
        if self.focused == Some(id) { return; }
        if let Some(prev_id) = self.focused {
            if let Some(prev) = self.windows.iter().find(|w| w.id == prev_id) {
                let (px, py, pw, ph) = (prev.x, prev.y, prev.w, prev.h);
                for cy in py..py.saturating_add(ph) {
                    for cx in px..px.saturating_add(pw) {
                        self.cell_dirty.push((cx, cy));
                    }
                }
            }
        }
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let win = self.windows.remove(pos);
            let new_id = win.id;
            let (x, y, w, h) = (win.x, win.y, win.w, win.h);
            if win.modal {
                self.windows.push(win);
            } else {
                let modal_pos = self.windows.iter().position(|w| w.modal).unwrap_or(self.windows.len());
                self.windows.insert(modal_pos, win);
            }
            self.focused = Some(new_id);
            for cy in y..y.saturating_add(h) {
                for cx in x..x.saturating_add(w) {
                    self.cell_dirty.push((cx, cy));
                }
            }
        }
    }

    fn apply_drag(&mut self, current_cell: (u16, u16)) {
        let ds = match self.drag_state {
            Some(ds) => ds,
            None => return,
        };
        let pos = match self.windows.iter().position(|w| w.id == ds.window_id) {
            Some(p) => p,
            None => { self.drag_state = None; return; }
        };
        let delta_x = current_cell.0 as i32 - ds.start_cell_x as i32;
        let delta_y = current_cell.1 as i32 - ds.start_cell_y as i32;

        let (old_x, old_y, old_w, old_h) = {
            let win = &self.windows[pos];
            (win.x, win.y, win.w, win.h)
        };

        match ds.mode {
            DragMode::Move => {
                let new_x = (ds.start_win_x as i32 + delta_x)
                    .max(0)
                    .min(self.cols as i32 - ds.start_win_w as i32) as u16;
                let new_y = (ds.start_win_y as i32 + delta_y)
                    .max(1)
                    .min(self.rows as i32 - ds.start_win_h as i32) as u16;
                self.windows[pos].x = new_x;
                self.windows[pos].y = new_y;
            }
            DragMode::Resize => {
                let new_w = ((ds.start_win_w as i32 + delta_x).max(5))
                    .min(self.cols as i32 - self.windows[pos].x as i32) as u16;
                let new_h = ((ds.start_win_h as i32 + delta_y).max(5))
                    .min(self.rows as i32 - self.windows[pos].y as i32) as u16;
                self.windows[pos].w = new_w;
                self.windows[pos].h = new_h;

                let input_ep = self.windows[pos].input_endpoint;
                if input_ep != 0 && new_w > 2 && new_h > 2 {
                    let is_modal = self.windows[pos].modal;
                    let (iw_off, ih_off): (u16, u16) = if is_modal { (0, 0) } else { (4, 3) };
                    let interior_w = new_w.saturating_sub(iw_off);
                    let interior_h = new_h.saturating_sub(ih_off);
                    {
                        let hdr = self.windows[pos].mapping.as_ptr() as *mut WindowShm;
                        unsafe {
                            core::ptr::write_volatile(&mut (*hdr).width as *mut u32, interior_w as u32);
                            core::ptr::write_volatile(&mut (*hdr).height as *mut u32, interior_h as u32);
                        }
                    }
                    let msg = libcluu::types::Message::new(
                        libcluu::ipc::COMP_WIN_CONFIGURE_LABEL,
                        [ds.window_id as usize, interior_w as usize, interior_h as usize, 0, 0, 0],
                        3,
                    );
                    let _ = libcluu::ipc::send(input_ep, &msg, libcluu::types::IpcFlags::empty());
                }
            }
        }

        let (new_x, new_y, new_w, new_h) = {
            let win = &self.windows[pos];
            (win.x, win.y, win.w, win.h)
        };
        let min_x = old_x.min(new_x);
        let min_y = old_y.min(new_y);
        let max_x = (old_x.saturating_add(old_w)).max(new_x.saturating_add(new_w));
        let max_y = (old_y.saturating_add(old_h)).max(new_y.saturating_add(new_h));
        for cy in min_y..max_y {
            for cx in min_x..max_x {
                self.cell_dirty.push((cx, cy));
            }
        }
    }

    /// Handle `COMP_WIN_SET_PIXEL_REGION_LABEL`.
    ///
    /// Maps the client-provided frame token into the compositor's address
    /// space and stores the region on the window. Sending `cell_w=0,
    /// cell_h=0` clears any existing pixel region.
    pub fn handle_win_set_pixel_region(
        &mut self,
        window_id: WindowId,
        cell_x: u16,
        cell_y: u16,
        cell_w: u16,
        cell_h: u16,
        pixel_token: u64,
    ) -> libcluu::Result<()> {
        let pos = self.windows.iter().position(|w| w.id == window_id);
        let Some(pos) = pos else {
            return Err(libcluu::Error::NotFound);
        };

        if let Some(old) = self.windows[pos].pixel_region.take() {
            let old_bytes = old.pixel_w as usize * old.pixel_h as usize * 4;
            let old_pages = (old_bytes + 0xFFF) / 0x1000;
            let _ = libcluu::syscall::space_unmap(
                libcluu::boot::space_token(),
                old.mapping.as_ptr() as usize,
                old_pages,
            );
            if old.shm_token != 0 {
                unsafe {
                    let _ = libcluu::syscall::invoke(
                        old.shm_token as usize,
                        libcluu::syscall::InvokeOp::FrameFree,
                        0, 0, 0, 0,
                    );
                }
            }
            for cy in old.cell_y..old.cell_y.saturating_add(old.cell_h) {
                for cx in old.cell_x..old.cell_x.saturating_add(old.cell_w) {
                    self.cell_dirty.push((cx, cy));
                }
            }
        }

        if cell_w == 0 || cell_h == 0 {
            return Ok(());
        }

        let pixel_w = cell_w as u32 * GLYPH_W;
        let pixel_h = cell_h as u32 * GLYPH_H;
        let total_pixels = pixel_w as usize * pixel_h as usize;
        let total_bytes = total_pixels * 4;
        let rounded = (total_bytes + 0xFFF) & !0xFFF;
        let num_pages = rounded / 0x1000;

        // VA must not collide with text-cell SHM (0xC100_0000+).
        let pixel_va: usize = 0xC800_0000 + (window_id as usize) * 0x40_0000;

        let flags = 0x07 | libcluu::syscall::MAP_FRAME_TOKEN;
        libcluu::syscall::space_map_range(
            libcluu::boot::space_token(),
            pixel_va,
            pixel_token as usize,
            flags,
            num_pages,
            0,
        )?;

        let mapping = ShmMapping::new(pixel_va, rounded)
            .ok_or(libcluu::Error::InvalidArgument)?;

        self.windows[pos].pixel_region = Some(WindowPixelRegion {
            cell_x,
            cell_y,
            cell_w,
            cell_h,
            pixel_w,
            pixel_h,
            mapping,
            shm_token: pixel_token,
        });

        for cy in cell_y..cell_y.saturating_add(cell_h) {
            for cx in cell_x..cell_x.saturating_add(cell_w) {
                self.cell_dirty.push((cx, cy));
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
struct WinInfo {
    id: WindowId,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}
