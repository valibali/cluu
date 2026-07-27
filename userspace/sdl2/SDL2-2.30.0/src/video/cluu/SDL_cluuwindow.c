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

/* CLUU window operations — displayd surface creation + compositor window
 * registration for keyboard input.
 *
 * CreateSDLWindow:
 *   1. Allocate CLUU_WindowData.
 *   2. Create a displayd surface (SurfaceCreate) for pixel output.
 *   3. Set geometry (centered for windowed, top-left for fullscreen).
 *   4. Register a compositor window (WinRegister) for keyboard input.
 *
 * Fullscreen: falls back to composite — no VT theft, no scanout promotion.
 * The surface is positioned at (0,0) and the compositor composites it. */

#include "SDL_cluuvideo.h"
#include "../SDL_sysvideo.h"
#include "../../events/SDL_events_c.h"

/* ── CreateSDLWindow ─────────────────────────────────────────────────── */

int CLUU_CreateSDLWindow(_THIS, SDL_Window *window)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd;
    CLUU_Message req, reply;
    unsigned int surf_w, surf_h, surf_pitch;
    long ret;

    wd = (CLUU_WindowData *)SDL_calloc(1, sizeof(CLUU_WindowData));
    if (!wd) {
        return SDL_OutOfMemory();
    }
    window->driverdata = wd;

    /* Surface dimensions from the SDL window request. */
    surf_w = (unsigned int)window->w;
    surf_h = (unsigned int)window->h;
    if (surf_w == 0 || surf_h == 0) {
        surf_w = dev->screen_w;
        surf_h = dev->screen_h;
    }
    surf_pitch = surf_w * 4;  /* XRGB8888 = 4 bytes/pixel */
    wd->surf_w = surf_w;
    wd->surf_h = surf_h;
    wd->surf_pitch = surf_pitch;
    wd->fullscreen = (window->flags & SDL_WINDOW_FULLSCREEN) ? 1 : 0;
    wd->visible = 1;

    /* Create displayd surface. */
    if (dev->displayd_ep != 0) {
        cluu_msg_init(&req, CLUU_DISPLAY_SURFACE_CREATE_LABEL,
            0,  /* words[0] = 0 (no payload) */
            surf_w, surf_h, surf_pitch,
            0, 0, 4);
        ret = cluu_ipc_call(dev->displayd_ep,
            &req, CLUU_MSG_SIZE, &reply, CLUU_MSG_SIZE);
        if (ret < 0) {
            SDL_free(wd);
            window->driverdata = NULL;
            return SDL_SetError("CLUU: displayd surface create IPC failed");
        }
        wd->surface_token = reply.words[0];
        if (wd->surface_token == 0) {
            /* Error code in words[4] (error variant index). */
            SDL_free(wd);
            window->driverdata = NULL;
            return SDL_SetError("CLUU: displayd refused surface create (err=%lu)",
                reply.words[4]);
        }

        /* Set geometry: centered for windowed, top-left for fullscreen. */
        {
            long geo_x, geo_y;
            char geo_payload[5];

            if (wd->fullscreen) {
                geo_x = 0;
                geo_y = 0;
            } else {
                geo_x = (long)((dev->screen_w - surf_w) / 2);
                geo_y = (long)((dev->screen_h - surf_h) / 2);
                if (geo_x < 0) geo_x = 0;
                if (geo_y < 0) geo_y = 0;
            }

            /* Payload: z_order (i32 LE) + visible (u8). */
            {
                int z = 1;
                geo_payload[0] = (char)(z & 0xFF);
                geo_payload[1] = (char)((z >> 8) & 0xFF);
                geo_payload[2] = (char)((z >> 16) & 0xFF);
                geo_payload[3] = (char)((z >> 24) & 0xFF);
                geo_payload[4] = 1;  /* visible = true */
            }

            cluu_msg_init(&req, CLUU_DISPLAY_SET_GEOMETRY_LABEL,
                0,  /* words[0] = 0 (payload len set by send_msg_with_payload) */
                wd->surface_token,  /* words[1] = surface token */
                (unsigned long)geo_x,  /* words[2] = x */
                (unsigned long)geo_y,  /* words[3] = y */
                0, 0, 4);
            /* For payload messages, words[0] must be the payload length. */
            req.words[0] = 5;  /* payload is 5 bytes */
            cluu_send_msg_with_payload(dev->displayd_ep, &req, geo_payload, 5);
        }

        dev->surface_count++;
    }

    /* Register a compositor window for keyboard input.
     * We create a minimal 1x1 cell window — pixel output goes through
     * displayd, not the compositor. The compositor window exists only
     * to receive COMP_INPUT_FORWARD messages. */
    if (dev->comp_ep != 0 && dev->input_ep != 0) {
        char title[] = "SDL";
        cluu_msg_init(&req, CLUU_COMP_WIN_REGISTER_LABEL,
            sizeof(title) - 1,  /* words[0] = title_len */
            1,  /* words[1] = req_w (1 cell) */
            1,  /* words[2] = req_h (1 cell) */
            dev->input_ep,  /* words[3] = our input endpoint */
            0,  /* words[4] = flags */
            0, 4);

        ret = cluu_call_with_payload(dev->comp_ep,
            &req, title, sizeof(title) - 1, &reply);
        if (ret == 0 && reply.tag.label == CLUU_COMP_WIN_REGISTER_REPLY) {
            wd->win_id = reply.words[0];
            wd->shm_token = reply.words[1];
        }
        /* Non-fatal if compositor registration fails — pixel output
         * still works, just no keyboard events. */
    }

    return 0;
}

/* ── DestroyWindow ───────────────────────────────────────────────────── */

void CLUU_DestroyWindow(_THIS, SDL_Window *window)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);
    CLUU_Message msg;

    if (!wd) return;

    wd->destroyed = 1;

    /* Destroy framebuffer first (frees frame token + unmap). */
    CLUU_DestroyWindowFramebuffer(_this, window);

    /* Destroy displayd surface. */
    if (wd->surface_token != 0 && dev->displayd_ep != 0) {
        cluu_msg_init(&msg, CLUU_DISPLAY_SURFACE_DESTROY_LABEL,
            0, wd->surface_token, 0, 0, 0, 0, 2);
        cluu_ipc_call(dev->displayd_ep, &msg, CLUU_MSG_SIZE, &msg, CLUU_MSG_SIZE);
        wd->surface_token = 0;
        if (dev->surface_count > 0) {
            dev->surface_count--;
        }
    }

    /* Destroy compositor window. */
    if (wd->win_id != 0 && dev->comp_ep != 0) {
        cluu_msg_init(&msg, CLUU_COMP_WIN_DESTROY_LABEL,
            wd->win_id, 0, 0, 0, 0, 0, 1);
        cluu_ipc_send(dev->comp_ep, &msg, CLUU_MSG_SIZE);
        wd->win_id = 0;
    }

    SDL_free(wd);
    window->driverdata = NULL;
}

/* ── ShowWindow / HideWindow / RaiseWindow ──────────────────────────── */

void CLUU_ShowWindow(_THIS, SDL_Window *window)
{
    CLUU_WindowData *wd = cluu_window_data(window);
    if (wd) wd->visible = 1;
}

void CLUU_HideWindow(_THIS, SDL_Window *window)
{
    CLUU_WindowData *wd = cluu_window_data(window);
    if (wd) wd->visible = 0;
}

void CLUU_RaiseWindow(_THIS, SDL_Window *window)
{
    /* No-op — displayd composites by z-order, not focus. */
    (void)_this;
    (void)window;
}

/* ── SetWindowFullscreen ─────────────────────────────────────────────── */

void CLUU_SetWindowFullscreen(_THIS, SDL_Window *window, SDL_VideoDisplay *display, SDL_bool fullscreen)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd = cluu_window_data(window);
    CLUU_Message msg;
    char geo_payload[5];

    (void)display;

    if (!wd || wd->surface_token == 0 || dev->displayd_ep == 0) {
        return;  /* no surface — nothing to do */
    }

    /* Fullscreen fallback: composite, not VT theft.
     * Move the surface to (0,0) for fullscreen, or re-center for windowed.
     * The compositor composites it at the surface position — no scanout
     * promotion is attempted. */
    wd->fullscreen = fullscreen ? 1 : 0;

    {
        long geo_x, geo_y;
        int z = 1;
        if (wd->fullscreen) {
            geo_x = 0;
            geo_y = 0;
        } else {
            geo_x = (long)((dev->screen_w - wd->surf_w) / 2);
            geo_y = (long)((dev->screen_h - wd->surf_h) / 2);
            if (geo_x < 0) geo_x = 0;
            if (geo_y < 0) geo_y = 0;
        }

        geo_payload[0] = (char)(z & 0xFF);
        geo_payload[1] = (char)((z >> 8) & 0xFF);
        geo_payload[2] = (char)((z >> 16) & 0xFF);
        geo_payload[3] = (char)((z >> 24) & 0xFF);
        geo_payload[4] = 1;  /* visible */

        cluu_msg_init(&msg, CLUU_DISPLAY_SET_GEOMETRY_LABEL,
            5,  /* words[0] = payload length */
            wd->surface_token,
            (unsigned long)geo_x,
            (unsigned long)geo_y,
            0, 0, 4);
        cluu_send_msg_with_payload(dev->displayd_ep, &msg, geo_payload, 5);
    }
}

#endif /* SDL_VIDEO_DRIVER_CLUU */

/* vi: set ts=4 sw=4 expandtab: */
