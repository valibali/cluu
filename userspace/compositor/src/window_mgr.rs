//! Window lifecycle, focus management, VT switching, and input forwarding.
//!
//! All methods are `impl Compositor` blocks; the type itself lives in `state`.

use crate::state::{Compositor, Window, WindowId, WindowShm, WIN_SHM_MAGIC, WIN_SHM_VERSION};
use libcluu::Result;

impl Compositor {
    /// Allocate a window per the request. Returns
    /// `(id, frame_token, granted_w, granted_h)` on success.
    ///
    /// Granted dims are clamped to the screen minus row 0 (status bar).
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
    ) -> Result<(WindowId, u64, u32, u32)> {
        let granted_w = (req_w as u16).min(self.cols);
        let granted_h = (req_h as u16).min(self.rows.saturating_sub(1));
        // Minimum 3×3: 1-cell chrome on each side + at least 1 interior cell.
        if granted_w < 3 || granted_h < 3 {
            return Err(libcluu::Error::InvalidArgument);
        }

        let cells_bytes = granted_w as usize * granted_h as usize * 8;
        let header_bytes = core::mem::size_of::<WindowShm>();
        let total_bytes = header_bytes + cells_bytes;
        let (token, allocated) = crate::shm::alloc_frame(total_bytes)?;

        let id = self.next_id;
        self.next_id += 1;

        // Per-window VA slot, well above APP_FB_BASE. Each id reserves a
        // 4 MiB stride so neighbouring windows never collide regardless of
        // their pixel dimensions. 256 MiB region total before we run out.
        let va_base: usize = 0xC100_0000;
        let va = va_base + (id as usize) * 0x40_0000;
        crate::shm::map_frame_rw(va, token, allocated)?;

        unsafe {
            let hdr = va as *mut WindowShm;
            (*hdr).magic = WIN_SHM_MAGIC;
            (*hdr).version = WIN_SHM_VERSION;
            (*hdr).width = granted_w as u32;
            (*hdr).height = granted_h as u32;
            (*hdr).cursor_x = 0;
            (*hdr).cursor_y = 0;
            (*hdr).cursor_visible = 0;
            (*hdr).generation = 0;
            // Zero cell area
            let cells_ptr = (va + header_bytes) as *mut u8;
            core::ptr::write_bytes(cells_ptr, 0, cells_bytes);
        }

        // Cascade window placement. Status bar reserves row 0, so y >= 1.
        let offset = (id as u16) * 2;
        let max_x = self.cols.saturating_sub(granted_w);
        let max_y = self.rows.saturating_sub(granted_h);
        let x = offset.min(max_x);
        let y = (1 + offset).min(max_y.max(1));

        let mut title_owned = alloc::string::String::new();
        title_owned.push_str(title);
        if title_owned.len() > 31 {
            title_owned.truncate(31);
        }

        self.windows.push(Window {
            id,
            owner_pid,
            title: title_owned,
            x,
            y,
            w: granted_w,
            h: granted_h,
            shm_va: va as *mut u8,
            shm_token: token,
            shm_size: allocated,
            last_gen: 0,
            input_endpoint,
        });
        self.focused = Some(id);
        // Mark all the window's cells dirty so the (eventual) compose pass
        // emits chrome + interior.
        for cy in y..y.saturating_add(granted_h) {
            for cx in x..x.saturating_add(granted_w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        Ok((id, token, granted_w as u32, granted_h as u32))
    }
}

impl Compositor {
    /// App says "I redrew (x,y,w,h) inside my window's interior". Mark
    /// the corresponding total-grid cells dirty.
    ///
    /// Chrome is 1 cell on each side, so interior starts at local (1,1).
    pub fn handle_win_damage(&mut self, id: WindowId, x: u32, y: u32, w: u32, h: u32) {
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        let inner_w = win.w.saturating_sub(2); // 1 chrome col each side
        let inner_h = win.h.saturating_sub(2); // 1 chrome row each side
        let cx0 = (x as u16).min(inner_w);
        let cy0 = (y as u16).min(inner_h);
        let cx1 = ((x as u16).saturating_add(w as u16)).min(inner_w);
        let cy1 = ((y as u16).saturating_add(h as u16)).min(inner_h);
        for iy in cy0..cy1 {
            for ix in cx0..cx1 {
                let gx = win.x + 1 + ix;
                let gy = win.y + 1 + iy;
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
    pub fn focus_next(&mut self) {
        if self.windows.is_empty() { return; }
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
    pub fn focus_prev(&mut self) {
        if self.windows.is_empty() { return; }
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
    }

    /// Mark every cell on screen dirty (used after focus changes so chrome
    /// repaints with correct focused/unfocused colours).
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
    pub fn handle_vt_activate(&mut self) {
        self.active = true;
        self.repaint_all();
    }

    /// VT switch: compositor's VT became inactive — suppress fb writes.
    pub fn handle_vt_deactivate(&mut self) {
        self.active = false;
    }
}

impl Compositor {
    /// Forward a raw kbd event to the focused window's input endpoint.
    /// `ascii`/`mods`/`scancode`/`extended` come straight from the
    /// `KbdEvent` variant of `protocol::Incoming`.
    pub fn forward_input_event(&self, ascii: u8, mods: u8, scancode: u8, extended: u8) {
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
                0usize, // kind = 0 → ordinary input
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
    /// Spawn a new compdemo container via procmgr. The new compdemo
    /// auto-registers a window with the compositor on startup.
    /// Uses the same payload format as vtmgr's spawn_vt_container:
    /// NUL-terminated image name, no param overrides.
    pub fn spawn_demo(&self) {
        let ep = match libcluu::registry::lookup_service("procmgr:spawn") {
            Some(ep) => ep,
            None => {
                let _ = libcluu::debug_print("compositor: spawn_demo: no procmgr:spawn");
                return;
            }
        };
        // Payload: "compdemo\0" (NUL-terminated image name, no param overrides).
        // Wire format: words[0]=payload_len, words[3]=name_nul_term_len, words[4]=param_count.
        let payload = b"compdemo\0";
        let msg = libcluu::types::Message::new(
            libcluu::ipc::PROCMGR_CONTAINER_RUN_LABEL,
            [payload.len(), 0, 0, payload.len(), 0, 0],
            5,
        );
        let _ = libcluu::ipc::send_msg_with_payload(ep, &msg, payload);
        let _ = libcluu::debug_print("compositor: spawn_demo: requested compdemo");
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
