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

/* CLUU framebuffer — maps a displayd surface buffer for pixel output.
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
 *   2. Send BUFFER_COMMIT to displayd with the frame token and damage rects
 *      derived from the SDL update rects. The damage is clipped to the
 *      surface bounds and capped at CLUU_MAX_DAMAGE_RECTS (8). If more
 *      rects are passed, the bounding box is used.
 *
 * The software renderer is the only honest renderer — no accelerated
 * renderer is advertised. SDL_RenderCreate with SDL_RENDERER_ACCELERATED
 * falls back to software automatically. */

#include "SDL_cluuvideo.h"
#include "../SDL_sysvideo.h"
#include "../SDL_pixels_c.h"

/* Fixed VA for mapping the displayd frame token. This must not conflict
 * with other mappings. The sdl2-shim used 0xD200_0000; we use the same. */
#define CLUU_FB_VA 0xD2000000u

/* ── CreateWindowFramebuffer ─────────────────────────────────────────── */

int CLUU_CreateWindowFramebuffer(_THIS, SDL_Window *window,
    Uint32 *format, void **pixels, int *pitch)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);
    unsigned int frame_bytes, frame_pages;
    unsigned long combined;
    long frame_token;
    long ret;

    if (!wd) {
        return SDL_SetError("CLUU: no window data for framebuffer");
    }

    /* Free any existing framebuffer. */
    CLUU_DestroyWindowFramebuffer(_this, window);

    /* XRGB8888 — the only format displayd supports. */
    *format = SDL_PIXELFORMAT_RGB888;
    *pitch = (int)wd->surf_pitch;

    /* Allocate a frame token for pixel transfer.
     * The frame token is a kernel-managed contiguous memory region that
     * displayd can map into its own address space to read our pixels. */
    frame_bytes = wd->surf_w * wd->surf_h * 4;
    frame_token = cluu_invoke(dev->space_cap,
        CLUU_INVOKE_FRAME_ALLOCATE,
        frame_bytes, 0, 0, 0);
    if (frame_token < 0) {
        return SDL_SetError("CLUU: frame allocate failed (err=%ld)", frame_token);
    }
    wd->frame_token = (unsigned long long)frame_token;
    wd->frame_bytes = frame_bytes;

    /* Map the frame token into our address space.
     * space_map_range packs (num_pages << 32) | data_len into arg4.
     * When MAP_FRAME_TOKEN is set in flags, source_ptr is the frame token. */
    frame_pages = (frame_bytes + 4095) / 4096;
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
    if (wd->surface_token == 0 || wd->frame_token == 0 || dev->displayd_ep == 0) {
        return SDL_SetError("CLUU: no displayd surface for update");
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
        /* Bounding box of all rects. */
        int min_x = rects[0].x, min_y = rects[0].y;
        int max_x = rects[0].x + rects[0].w, max_y = rects[0].y + rects[0].h;
        for (i = 1; i < numrects; i++) {
            if (rects[i].x < min_x) min_x = rects[i].x;
            if (rects[i].y < min_y) min_y = rects[i].y;
            if (rects[i].x + rects[i].w > max_x) max_x = rects[i].x + rects[i].w;
            if (rects[i].y + rects[i].h > max_y) max_y = rects[i].y + rects[i].h;
        }
        if (min_x < 0) min_x = 0;
        if (min_y < 0) min_y = 0;
        if (max_x > (int)wd->surf_w) max_x = (int)wd->surf_w;
        if (max_y > (int)wd->surf_h) max_y = (int)wd->surf_h;
        {
            unsigned int *d = (unsigned int *)damage_payload;
            d[0] = (unsigned int)min_x;
            d[1] = (unsigned int)min_y;
            d[2] = (unsigned int)(max_x - min_x);
            d[3] = (unsigned int)(max_y - min_y);
            if (d[2] == 0 || d[3] == 0) {
                d[2] = wd->surf_w;
                d[3] = wd->surf_h;
                d[0] = 0;
                d[1] = 0;
            }
        }
        damage_count = 1;
    } else {
        /* Use the rects directly, clipped to surface bounds. */
        damage_count = 0;
        for (i = 0; i < numrects && damage_count < CLUU_MAX_DAMAGE_RECTS; i++) {
            int x = rects[i].x, y = rects[i].y;
            int w = rects[i].w, h = rects[i].h;
            unsigned int *d;

            /* Clip to surface bounds. */
            if (x < 0) { w += x; x = 0; }
            if (y < 0) { h += y; y = 0; }
            if (x + w > (int)wd->surf_w) w = (int)wd->surf_w - x;
            if (y + h > (int)wd->surf_h) h = (int)wd->surf_h - y;
            if (w <= 0 || h <= 0) continue;  /* skip empty rects */

            d = (unsigned int *)(damage_payload + damage_count * 16);
            d[0] = (unsigned int)x;
            d[1] = (unsigned int)y;
            d[2] = (unsigned int)w;
            d[3] = (unsigned int)h;
            damage_count++;
        }
        if (damage_count == 0) {
            /* All rects were empty — use full surface. */
            unsigned int *d = (unsigned int *)damage_payload;
            d[0] = 0; d[1] = 0; d[2] = wd->surf_w; d[3] = wd->surf_h;
            damage_count = 1;
        }
    }

    /* Send BUFFER_COMMIT to displayd.
     * words[1] = surface_token, words[2] = buffer_index (0),
     * words[3] = seq (0 — displayd frame-token path ignores seq),
     * words[4] = client_frame_token.
     * Payload = damage rects (16 bytes each). */
    cluu_msg_init(&msg, CLUU_DISPLAY_BUFFER_COMMIT_LABEL,
        (unsigned long)(damage_count * 16),  /* words[0] = payload length */
        wd->surface_token,    /* words[1] */
        0,                    /* words[2] = buffer_index */
        0,                    /* words[3] = seq */
        wd->frame_token,      /* words[4] = client frame token */
        0, 5);

    ret = cluu_call_with_payload(dev->displayd_ep, &msg,
        damage_payload, damage_count * 16, &msg);
    if (ret < 0) {
        return SDL_SetError("CLUU: displayd commit IPC failed");
    }

    /* Check error code in reply words[0]. */
    if (msg.words[0] != 0) {
        return SDL_SetError("CLUU: displayd commit error (code=%lu)", msg.words[0]);
    }

    return 0;
}

/* ── DestroyWindowFramebuffer ────────────────────────────────────────── */

void CLUU_DestroyWindowFramebuffer(_THIS, SDL_Window *window)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);

    if (!wd) return;

    /* Unmap the frame token from our address space. */
    if (wd->frame_va != NULL && wd->frame_pages > 0) {
        cluu_invoke(dev->space_cap,
            CLUU_INVOKE_SPACE_UNMAP,
            CLUU_FB_VA,
            wd->frame_pages, 0, 0);
        wd->frame_va = NULL;
    }

    /* Free the frame token. */
    if (wd->frame_token != 0) {
        cluu_invoke((unsigned long)wd->frame_token,
            CLUU_INVOKE_FRAME_FREE, 0, 0, 0, 0);
        wd->frame_token = 0;
    }

    wd->frame_pages = 0;
    wd->frame_bytes = 0;
}

#endif /* SDL_VIDEO_DRIVER_CLUU */

/* vi: set ts=4 sw=4 expandtab: */
