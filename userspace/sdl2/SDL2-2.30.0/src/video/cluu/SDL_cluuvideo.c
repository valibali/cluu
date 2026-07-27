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

/* CLUU video backend — displayd surface protocol + compositor input.
 *
 * This is the real SDL2 video backend for CLUU (spec §3.6). It creates
 * windows on displayd via the surface protocol, renders through the
 * built-in software renderer (SDL_RENDERER_SOFTWARE only — no accelerated
 * renderer is advertised), and pumps keyboard/mouse events from the
 * compositor's input-forward channel.
 *
 * The C code uses raw CLUU kernel syscalls (inline assembly in the header)
 * to talk to displayd and the compositor. No Rust libcluu linkage is needed
 * at the C level — ProcessInfo and Message are #[repr(C)] and stable. */

#include "SDL_cluuvideo.h"
#include "../SDL_sysvideo.h"
#include "../../events/SDL_events_c.h"
#include "SDL_hints.h"

#define CLUUVID_DRIVER_NAME "cluu"

/* ── Forward declarations ────────────────────────────────────────────── */

static int CLUU_Available(void);
static SDL_VideoDevice *CLUU_CreateDevice(void);
static void CLUU_DeleteDevice(SDL_VideoDevice *device);

/* ── Bootstrap ────────────────────────────────────────────────────────── */

VideoBootStrap CLUU_bootstrap = {
    CLUUVID_DRIVER_NAME, "CLUU displayd video driver",
    CLUU_CreateDevice,
    NULL /* no ShowMessageBox */
};

/* ── Availability check ──────────────────────────────────────────────── */

static int CLUU_Available(void)
{
    /* The CLUU backend is activated only when the SDL_VIDEODRIVER hint
     * is explicitly "cluu". This avoids touching ProcessInfo memory on
     * non-CLUU hosts (where 0x7fe00100 is unmapped) and lets the dummy
     * backend coexist as fallback. On CLUU, the application or init
     * system sets SDL_VIDEODRIVER=cluu before SDL_Init. */
    const char *envr = SDL_GetHint(SDL_HINT_VIDEODRIVER);
    if (envr && SDL_strcmp(envr, CLUUVID_DRIVER_NAME) == 0) {
        return 1;
    }
    return 0;
}

/* ── Device creation / destruction ───────────────────────────────────── */

static void CLUU_DeleteDevice(SDL_VideoDevice *device)
{
    if (device->driverdata) {
        SDL_free(device->driverdata);
    }
    SDL_free(device);
}

static SDL_VideoDevice *CLUU_CreateDevice(void)
{
    SDL_VideoDevice *device;
    CLUU_DeviceData *data;

    if (!CLUU_Available()) {
        return NULL;
    }

    device = (SDL_VideoDevice *)SDL_calloc(1, sizeof(SDL_VideoDevice));
    if (!device) {
        SDL_OutOfMemory();
        return NULL;
    }

    data = (CLUU_DeviceData *)SDL_calloc(1, sizeof(CLUU_DeviceData));
    if (!data) {
        SDL_free(device);
        SDL_OutOfMemory();
        return NULL;
    }
    device->driverdata = data;

    /* Set function pointers */
    device->VideoInit = CLUU_VideoInit;
    device->VideoQuit = CLUU_VideoQuit;
    device->PumpEvents = CLUU_PumpEvents;

    device->CreateSDLWindow = CLUU_CreateSDLWindow;
    device->DestroyWindow = CLUU_DestroyWindow;
    device->ShowWindow = CLUU_ShowWindow;
    device->HideWindow = CLUU_HideWindow;
    device->RaiseWindow = CLUU_RaiseWindow;
    device->SetWindowFullscreen = CLUU_SetWindowFullscreen;

    device->CreateWindowFramebuffer = CLUU_CreateWindowFramebuffer;
    device->UpdateWindowFramebuffer = CLUU_UpdateWindowFramebuffer;
    device->DestroyWindowFramebuffer = CLUU_DestroyWindowFramebuffer;

    device->free = CLUU_DeleteDevice;

    return device;
}

/* ── Registry subscribe (C implementation) ───────────────────────────── */

/* Encode service:endpoint names as registry payload:
 *   [service_len:u16 LE][endpoint_len:u16 LE][service_bytes][endpoint_bytes] */
static int cluu_registry_encode_names(const char *service, const char *endpoint,
    char *out, int out_size)
{
    int slen = (int)SDL_strlen(service);
    int elen = (int)SDL_strlen(endpoint);
    int total = 4 + slen + elen;
    if (total > out_size) return -1;
    out[0] = (char)(slen & 0xFF);
    out[1] = (char)((slen >> 8) & 0xFF);
    out[2] = (char)(elen & 0xFF);
    out[3] = (char)((elen >> 8) & 0xFF);
    SDL_memcpy(out + 4, service, slen);
    SDL_memcpy(out + 4 + slen, endpoint, elen);
    return total;
}

/* Subscribe to a service endpoint via the registry. Returns the granted
 * endpoint token, or 0 on failure. */
static unsigned long cluu_registry_subscribe(CLUU_DeviceData *data,
    const char *service, const char *endpoint)
{
    char payload[128];
    int payload_len;
    CLUU_Message msg;
    CLUU_Message reply;
    char recv_buf[256];
    long ret;
    int i;

    if (data->registry_ep == 0 || data->control_ep == 0) {
        return 0;
    }

    payload_len = cluu_registry_encode_names(service, endpoint, payload, sizeof(payload));
    if (payload_len < 0) return 0;

    /* Build subscribe message: label=SUBSCRIBE, words[0]=payload_len,
     * words[1]=control_endpoint. */
    cluu_msg_init(&msg, CLUU_REGISTRY_SUBSCRIBE_LABEL,
        (unsigned long)payload_len, data->control_ep, 0, 0, 0, 0, 2);

    /* Send header + payload to registry endpoint. */
    if (cluu_send_msg_with_payload(data->registry_ep, &msg, payload, payload_len) < 0) {
        return 0;
    }

    /* Wait for grant delivery on control endpoint (up to ~2s).
     * We poll with short timeouts to avoid hanging forever if the
     * producer is dead. */
    for (i = 0; i < 200; i++) {
        unsigned long tokens[1];
        tokens[0] = data->control_ep;
        ret = cluu_ipc_recv_any(tokens, 1, recv_buf, sizeof(recv_buf), 10 /* 10ms */);
        if (ret < 0) {
            continue;  /* timeout or error — retry */
        }
        /* Received a message — parse it. */
        if (ret >= CLUU_MSG_SIZE) {
            SDL_memcpy(&reply, recv_buf, CLUU_MSG_SIZE);
            if (reply.tag.label == CLUU_REGISTRY_GRANT_DELIVER_LABEL) {
                /* words[1] = granted endpoint token. */
                return reply.words[1];
            }
            /* Other messages (grant requests, etc.) — ignore. */
        }
    }
    return 0;  /* timed out */
}

/* ── VideoInit ───────────────────────────────────────────────────────── */

int CLUU_VideoInit(_THIS)
{
    CLUU_DeviceData *data = cluu_device_data(_this);
    CLUU_Message req, reply;
    SDL_DisplayMode mode;
    long ret;

    /* Read capability tokens from ProcessInfo. */
    data->ipc_cap     = cluu_token(CLUU_TOKEN_IPC);
    data->space_cap   = cluu_token(CLUU_TOKEN_SPACE);
    data->clock_cap   = cluu_token(CLUU_TOKEN_CLOCK);
    data->registry_ep = cluu_token(CLUU_TOKEN_REGISTRY);

    /* displayd endpoint from PARAM_DISPLAYD_EP. */
    data->displayd_ep = (unsigned long)cluu_param(CLUU_PARAM_DISPLAYD_EP);

    if (data->ipc_cap == 0 || data->space_cap == 0) {
        return SDL_SetError("CLUU: missing capability tokens (ipc=%lu space=%lu)",
            data->ipc_cap, data->space_cap);
    }

    /* Create our own endpoint for receiving compositor input events. */
    data->input_ep = (unsigned long)cluu_invoke(data->ipc_cap,
        CLUU_INVOKE_ENDPOINT_CREATE, 0, 0, 0, 0);
    if ((long)data->input_ep < 0) {
        data->input_ep = 0;
        return SDL_SetError("CLUU: failed to create input endpoint");
    }

    /* Create control endpoint for registry grant delivery. */
    data->control_ep = (unsigned long)cluu_invoke(data->ipc_cap,
        CLUU_INVOKE_ENDPOINT_CREATE, 0, 0, 0, 0);
    if ((long)data->control_ep < 0) {
        data->control_ep = 0;
        return SDL_SetError("CLUU: failed to create registry control endpoint");
    }

    /* Fall back to registry lookup if PARAM_DISPLAYD_EP is unset. */
    if (data->displayd_ep == 0 && data->registry_ep != 0) {
        data->displayd_ep = cluu_registry_subscribe(data, "displayd", "main");
    }

    if (data->displayd_ep == 0) {
        data->screen_w = 640;
        data->screen_h = 400;
        data->screen_pitch = 640 * 4;
    } else {
        cluu_msg_init(&req, CLUU_DISPLAY_OUTPUT_INFO_LABEL,
            0, 0, 0, 0, 0, 0, 0);
        ret = cluu_ipc_call(data->displayd_ep,
            &req, CLUU_MSG_SIZE, &reply, CLUU_MSG_SIZE);
        if (ret < 0) {
            return SDL_SetError("CLUU: displayd output info query failed");
        }
        data->screen_w = (unsigned int)reply.words[0];
        data->screen_h = (unsigned int)reply.words[1];
        data->screen_pitch = (unsigned int)reply.words[2];
        if (data->screen_w == 0 || data->screen_h == 0) {
            return SDL_SetError("CLUU: displayd returned zero dimensions");
        }
    }

    /* Subscribe to compositor:client for keyboard input. */
    if (data->registry_ep != 0) {
        data->comp_ep = cluu_registry_subscribe(data, "compositor", "client");
        /* comp_ep == 0 is non-fatal — keyboard events won't arrive but
         * pixel output still works. */
    }

    /* Register the display mode. */
    SDL_zero(mode);
    mode.format = SDL_PIXELFORMAT_RGB888;
    mode.w = (int)data->screen_w;
    mode.h = (int)data->screen_h;
    mode.refresh_rate = 60;
    mode.driverdata = NULL;
    if (SDL_AddBasicVideoDisplay(&mode) < 0) {
        return -1;
    }
    SDL_AddDisplayMode(&_this->displays[0], &mode);

    data->surface_count = 0;

    return 0;
}

/* ── VideoQuit ───────────────────────────────────────────────────────── */

void CLUU_VideoQuit(_THIS)
{
    CLUU_DeviceData *data = cluu_device_data(_this);
    (void)data;
    /* Per-window cleanup happens in DestroyWindow/DestroyWindowFramebuffer.
     * Nothing global to clean up here — endpoints are process-scoped and
     * reclaimed on exit. */
}

#endif /* SDL_VIDEO_DRIVER_CLUU */

/* vi: set ts=4 sw=4 expandtab: */
