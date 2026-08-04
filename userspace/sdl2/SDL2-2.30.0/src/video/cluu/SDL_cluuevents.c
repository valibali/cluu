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

/* CLUU event pump — polls our IPC endpoint for compositor input forward
 * messages and converts them to SDL keyboard events.
 *
 * The compositor forwards keyboard events as COMP_INPUT_FORWARD_LABEL
 * messages with:
 *   words[1] = ascii byte
 *   words[2] = modifiers (bit 0=shift, 1=ctrl, 2=alt)
 *   words[3] = PS/2 set-2 scancode
 *   words[4] = extended code (1=up, 2=down, 3=left, 4=right, 0=normal)
 *   words[5] = kind (0=keydown, 2=keyup, 99=close-request/quit)
 *
 * We convert PS/2 scancodes to SDL scancodes (USB HID) and call
 * SDL_SendKeyboardKey. Close requests generate SDL_SendQuit. */

#include "SDL_cluuvideo.h"
#include "../SDL_sysvideo.h"
#include "../../events/SDL_events_c.h"
#include "../../events/SDL_keyboard_c.h"
#include "../../events/SDL_windowevents_c.h"

/* ── PS/2 set-2 scancode → SDL scancode (USB HID) mapping ────────────── */

static SDL_Scancode cluu_ps2_to_sdl_scancode(unsigned char ps2, unsigned char extended)
{
    /* Extended codes (arrow keys, etc.) */
    if (extended != 0) {
        switch (extended) {
            case 1: return SDL_SCANCODE_UP;
            case 2: return SDL_SCANCODE_DOWN;
            case 3: return SDL_SCANCODE_LEFT;
            case 4: return SDL_SCANCODE_RIGHT;
            default: return SDL_SCANCODE_UNKNOWN;
        }
    }

    /* PS/2 set-2 make codes → SDL USB HID scancodes. */
    switch (ps2 & 0x7F) {
        case 0x01: return SDL_SCANCODE_ESCAPE;
        case 0x02: return SDL_SCANCODE_1;
        case 0x03: return SDL_SCANCODE_2;
        case 0x04: return SDL_SCANCODE_3;
        case 0x05: return SDL_SCANCODE_4;
        case 0x06: return SDL_SCANCODE_5;
        case 0x07: return SDL_SCANCODE_6;
        case 0x08: return SDL_SCANCODE_7;
        case 0x09: return SDL_SCANCODE_8;
        case 0x0A: return SDL_SCANCODE_9;
        case 0x0B: return SDL_SCANCODE_0;
        case 0x0C: return SDL_SCANCODE_MINUS;
        case 0x0D: return SDL_SCANCODE_EQUALS;
        case 0x0E: return SDL_SCANCODE_BACKSPACE;
        case 0x0F: return SDL_SCANCODE_TAB;
        case 0x10: return SDL_SCANCODE_Q;
        case 0x11: return SDL_SCANCODE_W;
        case 0x12: return SDL_SCANCODE_E;
        case 0x13: return SDL_SCANCODE_R;
        case 0x14: return SDL_SCANCODE_T;
        case 0x15: return SDL_SCANCODE_Y;
        case 0x16: return SDL_SCANCODE_U;
        case 0x17: return SDL_SCANCODE_I;
        case 0x18: return SDL_SCANCODE_O;
        case 0x19: return SDL_SCANCODE_P;
        case 0x1A: return SDL_SCANCODE_LEFTBRACKET;
        case 0x1B: return SDL_SCANCODE_RIGHTBRACKET;
        case 0x1C: return SDL_SCANCODE_RETURN;
        case 0x1D: return SDL_SCANCODE_LCTRL;
        case 0x1E: return SDL_SCANCODE_A;
        case 0x1F: return SDL_SCANCODE_S;
        case 0x20: return SDL_SCANCODE_D;
        case 0x21: return SDL_SCANCODE_F;
        case 0x22: return SDL_SCANCODE_G;
        case 0x23: return SDL_SCANCODE_H;
        case 0x24: return SDL_SCANCODE_J;
        case 0x25: return SDL_SCANCODE_K;
        case 0x26: return SDL_SCANCODE_L;
        case 0x27: return SDL_SCANCODE_SEMICOLON;
        case 0x28: return SDL_SCANCODE_APOSTROPHE;
        case 0x29: return SDL_SCANCODE_GRAVE;
        case 0x2A: return SDL_SCANCODE_LSHIFT;
        case 0x2B: return SDL_SCANCODE_BACKSLASH;
        case 0x2C: return SDL_SCANCODE_Z;
        case 0x2D: return SDL_SCANCODE_X;
        case 0x2E: return SDL_SCANCODE_C;
        case 0x2F: return SDL_SCANCODE_V;
        case 0x30: return SDL_SCANCODE_B;
        case 0x31: return SDL_SCANCODE_N;
        case 0x32: return SDL_SCANCODE_M;
        case 0x33: return SDL_SCANCODE_COMMA;
        case 0x34: return SDL_SCANCODE_PERIOD;
        case 0x35: return SDL_SCANCODE_SLASH;
        case 0x36: return SDL_SCANCODE_RSHIFT;
        case 0x38: return SDL_SCANCODE_LALT;
        case 0x39: return SDL_SCANCODE_SPACE;
        case 0x3A: return SDL_SCANCODE_CAPSLOCK;
        case 0x3B: return SDL_SCANCODE_F1;
        case 0x3C: return SDL_SCANCODE_F2;
        case 0x3D: return SDL_SCANCODE_F3;
        case 0x3E: return SDL_SCANCODE_F4;
        case 0x3F: return SDL_SCANCODE_F5;
        case 0x40: return SDL_SCANCODE_F6;
        case 0x41: return SDL_SCANCODE_F7;
        case 0x42: return SDL_SCANCODE_F8;
        case 0x43: return SDL_SCANCODE_F9;
        case 0x44: return SDL_SCANCODE_F10;
        case 0x45: return SDL_SCANCODE_NUMLOCKCLEAR;
        case 0x46: return SDL_SCANCODE_SCROLLLOCK;
        case 0x47: return SDL_SCANCODE_KP_7;
        case 0x48: return SDL_SCANCODE_KP_8;
        case 0x49: return SDL_SCANCODE_KP_9;
        case 0x4A: return SDL_SCANCODE_MINUS;
        case 0x4B: return SDL_SCANCODE_KP_4;
        case 0x4C: return SDL_SCANCODE_KP_5;
        case 0x4D: return SDL_SCANCODE_KP_6;
        case 0x4E: return SDL_SCANCODE_KP_PLUS;
        case 0x4F: return SDL_SCANCODE_KP_1;
        case 0x50: return SDL_SCANCODE_KP_2;
        case 0x51: return SDL_SCANCODE_KP_3;
        case 0x52: return SDL_SCANCODE_KP_0;
        case 0x53: return SDL_SCANCODE_KP_PERIOD;
        case 0x57: return SDL_SCANCODE_F11;
        case 0x58: return SDL_SCANCODE_F12;
        default:   return SDL_SCANCODE_UNKNOWN;
    }
}

/* ── PumpEvents ──────────────────────────────────────────────────────── */

void CLUU_PumpEvents(_THIS)
{
    CLUU_DeviceData *dev = cluu_device_data(_this);
    char recv_buf[256];
    unsigned long tokens[2];
    int num_tokens = 0;
    int i;

    if (dev->input_ep == 0) {
        return;  /* no input endpoint — nothing to pump */
    }

    tokens[0] = dev->input_ep;
    num_tokens = 1;

    /* Also pump the registry control endpoint for grant delivery. */
    if (dev->control_ep != 0) {
        tokens[1] = dev->control_ep;
        num_tokens = 2;
    }

    /* Drain all pending messages (non-blocking: timeout_ms = 0). */
    for (i = 0; i < 32; i++) {  /* cap at 32 events per pump */
        long ret;
        CLUU_Message msg;

        ret = cluu_ipc_recv_any(tokens, num_tokens, recv_buf, sizeof(recv_buf), 0);
        if (ret < 0) {
            break;  /* WouldBlock or error — done. */
        }
        if (ret < CLUU_MSG_SIZE) {
            continue;  /* truncated — skip. */
        }

        SDL_memcpy(&msg, recv_buf, CLUU_MSG_SIZE);

        /* Windowed input is compositor-forwarded; direct fullscreen receives
         * the same key wire layout under the raw KBD_EVENT label. */
        if (msg.tag.label == CLUU_COMP_INPUT_FORWARD_LABEL ||
            msg.tag.label == CLUU_KBD_EVENT_LABEL) {
            unsigned char ascii    = (unsigned char)(msg.words[1] & 0xFF);
            unsigned char mods     = (unsigned char)(msg.words[2] & 0xFF);
            unsigned char scancode = (unsigned char)(msg.words[3] & 0xFF);
            unsigned char extended = (unsigned char)(msg.words[4] & 0xFF);
            unsigned int  kind     = (unsigned int)msg.words[5];

            if ((msg.tag.label == CLUU_KBD_EVENT_LABEL && kind == 0 &&
                 (mods & ((1u << 1) | (1u << 2))) == ((1u << 1) | (1u << 2)) &&
                 scancode == 0x2D && extended == 0) || kind == 99) {
                SDL_SendQuit();
                continue;
            }

            /* Convert PS/2 scancode to SDL scancode. */
            {
                SDL_Scancode sdl_sc = cluu_ps2_to_sdl_scancode(scancode, extended);
                if (sdl_sc != SDL_SCANCODE_UNKNOWN) {
                    Uint8 state;
                    if (kind == 0) {
                        state = SDL_PRESSED;
                    } else if (kind == 2) {
                        state = SDL_RELEASED;
                    } else {
                        continue;  /* unknown kind */
                    }
                    SDL_SendKeyboardKey(state, sdl_sc);
                }
                /* If scancode is unknown but ascii is printable, we could
                 * send a text input event. For now, the scancode path
                 * covers all keyboard test requirements. */
                (void)ascii;
                (void)mods;
            }
        }

        /* Registry grant delivery — ignore (we already subscribed during
         * VideoInit; late grants are harmless). */
        if (msg.tag.label == CLUU_REGISTRY_GRANT_DELIVER_LABEL) {
            /* Grant arrived after initial subscribe — store the token
             * as the compositor endpoint if we don't have one yet. */
            if (dev->comp_ep == 0) {
                dev->comp_ep = msg.words[1];
            }
        }
    }
}

#endif /* SDL_VIDEO_DRIVER_CLUU */

/* vi: set ts=4 sw=4 expandtab: */
