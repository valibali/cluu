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

/* CLUU window operations — direct fullscreen lease or compositor window.
 *
 * CreateSDLWindow:
 *   1. Allocate CLUU_WindowData.
 * Fullscreen is startup-only and uses displayd's direct framebuffer lease.
 * Windowed mode renders through a compositor PixelRegion. */

#include "SDL_cluuvideo.h"
#include "../SDL_sysvideo.h"
#include "../../events/SDL_events_c.h"

extern void cluu_debug(const char *msg);

static int CLUU_AcquireDirectLease(CLUU_DeviceData *dev,
    CLUU_WindowData *wd, SDL_Window *window)
{
    CLUU_Message req, reply;
    unsigned int bytes, pages;
    unsigned long fallback_pages;
    unsigned long mapped_pages;
    long ret;

    if (dev->displayd_ep == 0 || dev->input_ep == 0) {
        return SDL_SetError("CLUU: direct fullscreen requires displayd and input endpoint");
    }
    if (cluu_framebuffer_layout(dev->screen_w, dev->screen_h,
            dev->screen_pitch, NULL, NULL) < 0) {
        return SDL_SetError("CLUU: invalid display geometry for direct fullscreen");
    }
    cluu_framebuffer_mapping_pages(dev->screen_h, dev->screen_pitch,
        &fallback_pages);

    cluu_msg_init(&req, CLUU_DISPLAY_LEASE_ACQUIRE_LABEL,
        dev->space_cap, CLUU_FB_VA, dev->input_ep, 0, 0, 0, 3);
    ret = cluu_ipc_call(dev->displayd_ep,
        &req, CLUU_MSG_SIZE, &reply, CLUU_MSG_SIZE);
    if (ret < CLUU_MSG_SIZE ||
        reply.tag.label != CLUU_DISPLAY_LEASE_ACQUIRE_LABEL ||
        reply.words[5] != 0 || reply.words[0] == 0 || reply.words[1] == 0) {
        return SDL_SetError("CLUU: direct fullscreen lease acquisition failed");
    }
    wd->lease_id = reply.words[0];
    wd->lease_generation = reply.words[1];
    wd->direct_va = (void *)(unsigned long)CLUU_FB_VA;
    wd->direct_pages = fallback_pages;
    wd->direct_phase = CLUU_DIRECT_MAPPED;

    if (reply.words[2] > (~(unsigned int)0) ||
        reply.words[3] > (~(unsigned int)0) ||
        reply.words[4] > (~(unsigned int)0)) {
        cluu_release_direct_lease(dev, wd);
        return SDL_SetError("CLUU: malformed direct framebuffer grant");
    }

    if (cluu_framebuffer_mapping_pages((unsigned int)reply.words[3],
            (unsigned int)reply.words[4], &mapped_pages) < 0) {
        cluu_release_direct_lease(dev, wd);
        return SDL_SetError("CLUU: direct framebuffer mapping size overflow");
    }
    wd->direct_pages = mapped_pages;
    wd->direct_phase = CLUU_DIRECT_MAPPED;

    if (cluu_framebuffer_layout((unsigned int)reply.words[2],
            (unsigned int)reply.words[3], (unsigned int)reply.words[4],
            &bytes, &pages) < 0) {
        cluu_release_direct_lease(dev, wd);
        return SDL_SetError("CLUU: invalid direct framebuffer grant");
    }

    wd->surf_w = (unsigned int)reply.words[2];
    wd->surf_h = (unsigned int)reply.words[3];
    wd->surf_pitch = (unsigned int)reply.words[4];
    wd->direct_pages = pages;
    wd->direct_bytes = bytes;
    wd->direct_phase = CLUU_DIRECT_MAPPED;
    wd->visible = 1;
    window->w = (int)wd->surf_w;
    window->h = (int)wd->surf_h;
    cluu_debug("sdl2-cluu: direct FB");
    return 0;
}

/* ── CreateSDLWindow ─────────────────────────────────────────────────── */

int CLUU_CreateSDLWindow(_THIS, SDL_Window *window)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    CLUU_WindowData *wd;
    CLUU_Message req, reply;
    unsigned int pixel_w, pixel_h, pixel_pitch;
    unsigned int cell_w, cell_h;
    int requested_w, requested_h;
    long ret;

    wd = (CLUU_WindowData *)SDL_calloc(1, sizeof(CLUU_WindowData));
    if (!wd) {
        return SDL_OutOfMemory();
    }
    window->driverdata = wd;

    wd->fullscreen = (window->flags & SDL_WINDOW_FULLSCREEN) ? 1 : 0;
    if (wd->fullscreen) {
        if (CLUU_AcquireDirectLease(dev, wd, window) < 0) {
            if (wd->direct_phase != CLUU_DIRECT_NONE &&
                wd->direct_phase != CLUU_DIRECT_RELEASED) {
                if (cluu_release_direct_lease(dev, wd) < 0) {
                    dev->failed_direct_cleanup = wd;
                    window->driverdata = NULL;
                    return -1;
                }
            }
            SDL_free(wd);
            window->driverdata = NULL;
            return -1;
        }
        return 0;
    }
    if (window->w < 0 || window->h < 0) {
        SDL_free(wd);
        window->driverdata = NULL;
        return SDL_SetError("CLUU: invalid window dimensions");
    }
    requested_w = window->w;
    requested_h = window->h;
    pixel_w = window->w == 0 ? dev->screen_w : (unsigned int)window->w;
    pixel_h = window->h == 0 ? dev->screen_h : (unsigned int)window->h;
    cell_w = pixel_w / CLUU_COMP_GLYPH_W;
    if ((pixel_w % CLUU_COMP_GLYPH_W) != 0) {
        cell_w++;
    }
    cell_h = pixel_h / CLUU_COMP_GLYPH_H;
    if ((pixel_h % CLUU_COMP_GLYPH_H) != 0) {
        cell_h++;
    }
    if (cell_w < 3 || cell_h < 3 || cell_w > 0xffffu || cell_h > 0xffffu ||
        cell_w > (~(unsigned int)0) / CLUU_COMP_GLYPH_W ||
        cell_h > (~(unsigned int)0) / CLUU_COMP_GLYPH_H) {
        SDL_free(wd);
        window->driverdata = NULL;
        return SDL_SetError("CLUU: invalid compositor cell geometry");
    }
    pixel_w = cell_w * CLUU_COMP_GLYPH_W;
    pixel_h = cell_h * CLUU_COMP_GLYPH_H;
    if (pixel_w > (~(unsigned int)0) / 4u) {
        SDL_free(wd);
        window->driverdata = NULL;
        return SDL_SetError("CLUU: PixelRegion pitch overflow");
    }
    pixel_pitch = pixel_w * 4u;
    if (cluu_framebuffer_layout(pixel_w, pixel_h, pixel_pitch, NULL, NULL) < 0) {
        SDL_free(wd);
        window->driverdata = NULL;
        return SDL_SetError("CLUU: invalid PixelRegion geometry");
    }
    if (dev->comp_ep == 0 || dev->input_ep == 0) {
        SDL_free(wd);
        window->driverdata = NULL;
        return SDL_SetError("CLUU: windowed mode requires compositor and input endpoint");
    }

    {
        char title[] = "SDL";
        unsigned long win_flags = 0;
        cluu_msg_init(&req, CLUU_COMP_WIN_REGISTER_LABEL,
            sizeof(title) - 1,
            cell_w,
            cell_h,
            dev->input_ep,
            win_flags,
            0,
            5);

        SDL_memset(&reply, 0, sizeof(reply));
        ret = cluu_call_with_payload(dev->comp_ep,
            &req, title, sizeof(title) - 1, &reply);
        if (ret < 0 || reply.tag.label != CLUU_COMP_WIN_REGISTER_REPLY ||
            reply.words[0] == 0 || reply.words[1] == 0 || reply.words[4] != 0 ||
            reply.words[2] != cell_w || reply.words[3] != cell_h) {
            if (ret == 0 && reply.tag.label == CLUU_COMP_WIN_REGISTER_REPLY &&
                reply.words[0] != 0) {
                CLUU_Message destroy;
                cluu_msg_init(&destroy, CLUU_COMP_WIN_DESTROY_LABEL,
                    reply.words[0], 0, 0, 0, 0, 0, 1);
                cluu_ipc_send(dev->comp_ep, &destroy, CLUU_MSG_SIZE);
            }
            SDL_free(wd);
            window->driverdata = NULL;
            return SDL_SetError("CLUU: compositor window registration failed");
        }
        wd->win_id = reply.words[0];
        wd->shm_token = reply.words[1];
    }

    wd->pixel_w = pixel_w;
    wd->pixel_h = pixel_h;
    wd->pixel_pitch = pixel_pitch;
    wd->cell_w = cell_w;
    wd->cell_h = cell_h;
    wd->visible = 1;
    if (requested_w == 0) {
        window->w = (int)pixel_w;
    }
    if (requested_h == 0) {
        window->h = (int)pixel_h;
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
    if (wd->direct_phase != CLUU_DIRECT_NONE &&
        wd->direct_phase != CLUU_DIRECT_RELEASED) {
        dev->failed_direct_cleanup = wd;
        window->driverdata = NULL;
        return;
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
    /* No-op — compositor manages z-order through focus. */
    (void)_this;
    (void)window;
}

/* ── SetWindowFullscreen ─────────────────────────────────────────────── */

SDL_bool CLUU_CanSetWindowFullscreen(_THIS, SDL_Window *window,
    SDL_VideoDisplay *display, SDL_bool fullscreen)
{
    CLUU_WindowData *wd = cluu_window_data(window);

    (void)_this;
    (void)display;

    /* Startup mode is immutable. Direct lease transitions require coordinated
     * framebuffer and input ownership changes that SDL cannot do atomically. */
    return wd && ((fullscreen ? 1 : 0) == wd->fullscreen) ? SDL_TRUE : SDL_FALSE;
}

void CLUU_SetWindowFullscreen(_THIS, SDL_Window *window,
    SDL_VideoDisplay *display, SDL_bool fullscreen)
{
    (void)_this;
    (void)window;
    (void)display;
    (void)fullscreen;
}

#endif /* SDL_VIDEO_DRIVER_CLUU */

/* vi: set ts=4 sw=4 expandtab: */
