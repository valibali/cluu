/*
  Simple DirectMedia Layer
  Copyright (C) 1997-2024 Sam Lantinga <slouken@libsdl.org>

  This software is provided 'as-is', without any express or implied
  warranty.  In no event will the authors be held liable for any damages
  arising from the use of this software.

  Permission is granted to anyone to use this software for any purpose,
  including commercial applications, and to alter it and redistribute it
  freely, subject to the following restrictions:

  1. The origin of this software must not be misrepresented; you must not
     claim that you wrote the original software. If you use this software
     in a product, an acknowledgment in the product documentation would be
     appreciated but is not required.
  2. Altered source versions must be plainly marked as such, and must not be
     misrepresented as being the original software.
  3. This notice may not be removed or altered from any source distribution.
*/
#include "../../SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_CLUU

/* CLUU framebuffer — maps compositor PixelRegion or direct display pixels.
 *
 * CreateWindowFramebuffer:
 *   1. Allocate a frame token via invoke(space_cap, FrameAllocate, bytes, ...).
 *   2. Map the frame token into our address space at a fixed VA.
 *   3. Return the mapped pointer as the SDL framebuffer.
 *
 * UpdateWindowFramebuffer:
 *   1. Copy the SDL framebuffer pixels into the frame token memory
 *      (they're already there — the SDL software renderer writes directly
 *      to the mapped frame token memory).
 *   2. Windowed mode sends full-cell DAMAGE to the compositor. Direct
 *      fullscreen sends BUFFER_COMMIT to displayd with SDL damage rects.
 *
 * The software renderer is the only honest renderer — no accelerated
 * renderer is advertised. SDL_RenderCreate with SDL_RENDERER_ACCELERATED
 * falls back to software automatically. */

#include "SDL_cluuvideo.h"
#include "../SDL_sysvideo.h"
#include "../SDL_pixels_c.h"

extern void cluu_debug(const char *msg);

/* ── CreateWindowFramebuffer ─────────────────────────────────────────── */

int CLUU_CreateWindowFramebuffer(_THIS, SDL_Window *window,
    Uint32 *format, void **pixels, int *pitch)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);
    CLUU_Message msg;
    unsigned int frame_bytes, frame_pages;
    unsigned long combined;
    long frame_token;
    long ret;

    if (!wd) {
        return SDL_SetError("CLUU: no window data for framebuffer");
    }

    if (wd->direct_phase == CLUU_DIRECT_MAPPED) {
        if (cluu_framebuffer_layout(wd->surf_w, wd->surf_h, wd->surf_pitch,
                &frame_bytes, &frame_pages) < 0 ||
            frame_bytes != wd->direct_bytes || frame_pages != wd->direct_pages) {
            return SDL_SetError("CLUU: invalid direct framebuffer layout");
        }
        *format = SDL_PIXELFORMAT_RGB888;
        *pitch = (int)wd->surf_pitch;
        *pixels = wd->direct_va;
        if (!wd->framebuffer_created) {
            SDL_memset(wd->direct_va, 0, wd->direct_bytes);
            wd->framebuffer_created = 1;
        }
        return 0;
    }
    if (wd->direct_phase != CLUU_DIRECT_NONE) {
        return SDL_SetError("CLUU: direct framebuffer lease is not writable");
    }

    CLUU_DestroyWindowFramebuffer(_this, window);

    /* XRGB8888 — the compositor PixelRegion format. */
    *format = SDL_PIXELFORMAT_RGB888;
    *pitch = (int)wd->pixel_pitch;

    /* Allocate a frame token for pixel transfer.
     * The frame token is a kernel-managed contiguous memory region that
     * compositor can map into its own address space to read our pixels. */
    if (cluu_framebuffer_layout(wd->pixel_w, wd->pixel_h, wd->pixel_pitch,
            &frame_bytes, &frame_pages) < 0) {
        return SDL_SetError("CLUU: invalid framebuffer layout");
    }
    frame_token = cluu_invoke(dev->space_cap,
        CLUU_INVOKE_FRAME_ALLOCATE,
        (unsigned long)frame_pages * CLUU_PAGE_SIZE, 0, 0, 0);
    if (frame_token < 0) {
        return SDL_SetError("CLUU: frame allocate failed (err=%ld)", frame_token);
    }
    wd->frame_token = (unsigned long long)frame_token;
    wd->frame_bytes = frame_bytes;

    /* Map the frame token into our address space.
     * space_map_range packs (num_pages << 32) | data_len into arg4.
     * When MAP_FRAME_TOKEN is set in flags, source_ptr is the frame token. */
    wd->frame_pages = frame_pages;
    combined = ((unsigned long)frame_pages << 32) | (unsigned long)frame_bytes;

    ret = cluu_invoke(dev->space_cap,
        CLUU_INVOKE_SPACE_MAP_RANGE,
        CLUU_FB_VA,
        (unsigned long)frame_token,
        CLUU_FLAGS_USER_RW | CLUU_MAP_FRAME_TOKEN,
        combined);
    if (ret < 0) {
        /* Map failed — free the frame token. */
        cluu_invoke((unsigned long)frame_token, CLUU_INVOKE_FRAME_FREE, 0, 0, 0, 0);
        wd->frame_token = 0;
        return SDL_SetError("CLUU: frame map failed (err=%ld)", ret);
    }

    wd->frame_va = (void *)(unsigned long)CLUU_FB_VA;

    /* Zero the framebuffer. */
    SDL_memset(wd->frame_va, 0, frame_bytes);

    cluu_msg_init(&msg, CLUU_COMP_WIN_SET_PIXEL_REGION_LABEL,
        (unsigned long)wd->win_id,
        0, 0,
        (unsigned long)wd->cell_w,
        (unsigned long)wd->cell_h,
        (unsigned long)wd->frame_token,
        6);
    if (cluu_ipc_send(dev->comp_ep, &msg, CLUU_MSG_SIZE) < 0) {
        CLUU_DestroyWindowFramebuffer(_this, window);
        return SDL_SetError("CLUU: PixelRegion attach failed");
    }
    wd->compositor_owns_frame = 1;
    cluu_debug("sdl2-cluu: pixel region");

    /* Return the mapped pointer — SDL's software renderer writes here. */
    *pixels = wd->frame_va;

    return 0;
}

/* ── UpdateWindowFramebuffer ─────────────────────────────────────────── */

int CLUU_UpdateWindowFramebuffer(_THIS, SDL_Window *window,
    const SDL_Rect *rects, int numrects)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);
    CLUU_Message msg;
    char damage_payload[CLUU_MAX_DAMAGE_RECTS * 16];
    int damage_count;
    int i;
    long ret;

    if (!wd || wd->destroyed) {
        return SDL_SetError("CLUU: window destroyed");
    }
    if (wd->direct_phase == CLUU_DIRECT_MAPPED) {
        if (dev->displayd_ep == 0) {
            return SDL_SetError("CLUU: no displayd lease for update");
        }
        if (wd->direct_va == NULL || !wd->framebuffer_created) {
            return SDL_SetError("CLUU: direct framebuffer unavailable");
        }
    } else if (wd->direct_phase != CLUU_DIRECT_NONE) {
        return SDL_SetError("CLUU: direct framebuffer lease is not writable");
    } else if (dev->comp_ep == 0 || wd->win_id == 0 || wd->frame_token == 0 ||
        !wd->compositor_owns_frame) {
        return SDL_SetError("CLUU: no compositor PixelRegion for update");
    }

    if (wd->direct_phase == CLUU_DIRECT_NONE) {
        cluu_msg_init(&msg, CLUU_COMP_WIN_DAMAGE_LABEL,
            (unsigned long)wd->win_id,
            0, 0,
            (unsigned long)wd->cell_w,
            (unsigned long)wd->cell_h,
            0,
            5);
        if (cluu_ipc_send(dev->comp_ep, &msg, CLUU_MSG_SIZE) < 0) {
            return SDL_SetError("CLUU: compositor damage failed");
        }
        return 0;
    }

    /* Build damage list from SDL update rects.
     * Each rect is 16 bytes: x:u32, y:u32, w:u32, h:u32 (all LE).
     * Clip to surface bounds and skip zero-dimension rects.
     * If more than CLUU_MAX_DAMAGE_RECTS rects are passed, use the
     * bounding box of all rects. */
    if (numrects <= 0) {
        /* No rects — commit full surface as damage. */
        unsigned int *d = (unsigned int *)damage_payload;
        d[0] = 0; d[1] = 0; d[2] = wd->surf_w; d[3] = wd->surf_h;
        damage_count = 1;
    } else if (numrects > CLUU_MAX_DAMAGE_RECTS) {
        Sint64 min_x = wd->surf_w, min_y = wd->surf_h;
        Sint64 max_x = 0, max_y = 0;
        int found = 0;

        for (i = 0; i < numrects; i++) {
            Sint64 x = rects[i].x;
            Sint64 y = rects[i].y;
            Sint64 right;
            Sint64 bottom;

            if (rects[i].w <= 0 || rects[i].h <= 0) continue;
            right = x + (Sint64)rects[i].w;
            bottom = y + (Sint64)rects[i].h;
            if (x < 0) x = 0;
            if (y < 0) y = 0;
            if (right > wd->surf_w) right = wd->surf_w;
            if (bottom > wd->surf_h) bottom = wd->surf_h;
            if (right <= x || bottom <= y) continue;
            if (x < min_x) min_x = x;
            if (y < min_y) min_y = y;
            if (right > max_x) max_x = right;
            if (bottom > max_y) max_y = bottom;
            found = 1;
        }
        {
            unsigned int *d = (unsigned int *)damage_payload;
            if (!found) {
                d[2] = wd->surf_w;
                d[3] = wd->surf_h;
                d[0] = 0;
                d[1] = 0;
            } else {
                d[0] = (unsigned int)min_x;
                d[1] = (unsigned int)min_y;
                d[2] = (unsigned int)(max_x - min_x);
                d[3] = (unsigned int)(max_y - min_y);
            }
        }
        damage_count = 1;
    } else {
        /* Use the rects directly, clipped to surface bounds. */
        damage_count = 0;
        for (i = 0; i < numrects && damage_count < CLUU_MAX_DAMAGE_RECTS; i++) {
            Sint64 x = rects[i].x, y = rects[i].y;
            Sint64 right, bottom;
            unsigned int *d;

            if (rects[i].w <= 0 || rects[i].h <= 0) continue;
            right = x + (Sint64)rects[i].w;
            bottom = y + (Sint64)rects[i].h;
            if (x < 0) x = 0;
            if (y < 0) y = 0;
            if (right > wd->surf_w) right = wd->surf_w;
            if (bottom > wd->surf_h) bottom = wd->surf_h;
            if (right <= x || bottom <= y) continue;

            d = (unsigned int *)(damage_payload + damage_count * 16);
            d[0] = (unsigned int)x;
            d[1] = (unsigned int)y;
            d[2] = (unsigned int)(right - x);
            d[3] = (unsigned int)(bottom - y);
            damage_count++;
        }
        if (damage_count == 0) {
            /* All rects were empty — use full surface. */
            unsigned int *d = (unsigned int *)damage_payload;
            d[0] = 0; d[1] = 0; d[2] = wd->surf_w; d[3] = wd->surf_h;
            damage_count = 1;
        }
    }

    if (wd->direct_phase == CLUU_DIRECT_MAPPED) {
        cluu_msg_init(&msg, CLUU_DISPLAY_BUFFER_COMMIT_LABEL,
            (unsigned long)(damage_count * 16),
            (unsigned long)wd->lease_id,
            0,
            (unsigned long)wd->lease_generation,
            0, 0, 4);
        ret = cluu_call_with_payload(dev->displayd_ep, &msg,
            damage_payload, damage_count * 16, &msg);
        if (ret < 0 || msg.tag.label != CLUU_DISPLAY_BUFFER_COMMIT_LABEL ||
            msg.words[0] != 0) {
            return SDL_SetError("CLUU: direct framebuffer damage failed");
        }
        return 0;
    }

    return 0;
}

/* ── DestroyWindowFramebuffer ────────────────────────────────────────── */

void CLUU_DestroyWindowFramebuffer(_THIS, SDL_Window *window)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);

    if (!wd) return;

    if (wd->direct_phase != CLUU_DIRECT_NONE &&
        wd->direct_phase != CLUU_DIRECT_RELEASED) {
        cluu_release_direct_lease(dev, wd);
        return;
    }

    /* Unmap the frame token from our address space. */
    if (wd->frame_va != NULL && wd->frame_pages > 0) {
        cluu_invoke(dev->space_cap,
            CLUU_INVOKE_SPACE_UNMAP,
            CLUU_FB_VA,
            wd->frame_pages, 0, 0);
        wd->frame_va = NULL;
    }

    /* Free only tokens whose ownership was not transferred to compositor. */
    if (wd->frame_token != 0 && !wd->compositor_owns_frame) {
        cluu_invoke((unsigned long)wd->frame_token,
            CLUU_INVOKE_FRAME_FREE, 0, 0, 0, 0);
    }

    wd->frame_token = 0;
    wd->frame_pages = 0;
    wd->frame_bytes = 0;
    wd->compositor_owns_frame = 0;
}

#endif /* SDL_VIDEO_DRIVER_CLUU */

/* vi: set ts=4 sw=4 expandtab: */
