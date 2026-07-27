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

#ifndef SDL_cluuvideo_h_
#define SDL_cluuvideo_h_

#include "../SDL_sysvideo.h"

/*
 * CLUU SDL2 video backend — displayd surface protocol + compositor input.
 *
 * Five logical files (spec §3.6):
 *   SDL_cluuvideo.h        — this file: shared declarations, IPC helpers
 *   SDL_cluuvideo.c        — bootstrap, VideoInit/VideoQuit, display modes
 *   SDL_cluuwindow.c       — CreateSDLWindow/DestroyWindow, fullscreen fallback
 *   SDL_cluuframebuffer.c  — framebuffer create/update/destroy → displayd commit
 *   SDL_cluuevents.c       — PumpEvents → SDL_SendKeyboardKey from compositor forward
 *
 * The C backend talks to displayd and the compositor via raw CLUU kernel
 * syscalls (inline asm). No Rust libcluu linkage is needed at the C level —
 * the ProcessInfo block and IPC message layout are #[repr(C)] and stable.
 *
 * Software renderer only. No SDL_RENDERER_ACCELERATED is advertised.
 * Fullscreen falls back to composite (no VT theft, no scanout promotion).
 */

/* ── ProcessInfo (at PROCESS_INFO_ADDR, #[repr(C)]) ─────────────────── */

#define CLUU_BOOT_INFO_ADDR      0x7fe00000u
#define CLUU_PROCESS_INFO_ADDR   (CLUU_BOOT_INFO_ADDR + 0x100)

#define CLUU_TOKEN_STDIN    0
#define CLUU_TOKEN_STDOUT   1
#define CLUU_TOKEN_STDERR   2
#define CLUU_TOKEN_STDLOG   3
#define CLUU_TOKEN_SELF     4
#define CLUU_TOKEN_SPACE    5
#define CLUU_TOKEN_IPC      6
#define CLUU_TOKEN_CLOCK    7
#define CLUU_TOKEN_REGISTRY 8

#define CLUU_PARAM_SESSION_VFS_EP 18
#define CLUU_PARAM_DISPLAYD_EP    19

typedef struct {
    unsigned long exit_token;       /* slot 0 */
    unsigned long exit_cookie;      /* slot 1 */
    unsigned long pid;              /* slot 2 */
    unsigned long tokens[17];       /* slots 3..19 */
    unsigned long long params[32];  /* slots 20..51 */
} CLUU_ProcessInfo;

/* ── IPC Message (#[repr(C)], 56 bytes) ─────────────────────────────── */

typedef struct {
    unsigned int  label;   /* u32 */
    unsigned char words;   /* u8  — word count */
    unsigned char extra;   /* u8  */
    unsigned short _pad;   /* u16 */
} CLUU_MessageTag;

typedef struct {
    CLUU_MessageTag tag;
    unsigned long words[6];
} CLUU_Message;

#define CLUU_MSG_SIZE 56  /* sizeof(CLUU_Message) */

/* ── Syscall numbers ────────────────────────────────────────────────── */

#define CLUU_SYS_SEND    0
#define CLUU_SYS_RECV    1
#define CLUU_SYS_CALL    2
#define CLUU_SYS_REPLY   3
#define CLUU_SYS_YIELD   4
#define CLUU_SYS_INVOKE  5

/* ── InvokeOp numbers ────────────────────────────────────────────────── */

#define CLUU_INVOKE_ENDPOINT_CREATE  40
#define CLUU_INVOKE_SPACE_MAP_RANGE  15
#define CLUU_INVOKE_SPACE_UNMAP      13
#define CLUU_INVOKE_FRAME_ALLOCATE   70
#define CLUU_INVOKE_FRAME_FREE       71

/* ── Space map flags ─────────────────────────────────────────────────── */

#define CLUU_MAP_FRAME_TOKEN  0x400u
#define CLUU_FLAGS_USER_RW    0x07u

/* ── Display protocol labels (cluu_wire::display) ────────────────────── */

#define CLUU_DISPLAY_OUTPUT_INFO_LABEL     300u
#define CLUU_DISPLAY_SURFACE_CREATE_LABEL  301u
#define CLUU_DISPLAY_BUFFER_ACQUIRE_LABEL  302u
#define CLUU_DISPLAY_BUFFER_COMMIT_LABEL   303u
#define CLUU_DISPLAY_BUFFER_RELEASE_LABEL  304u
#define CLUU_DISPLAY_SET_GEOMETRY_LABEL    305u
#define CLUU_DISPLAY_SET_VISIBLE_LABEL     306u
#define CLUU_DISPLAY_SURFACE_DESTROY_LABEL 307u

/* ── Compositor protocol labels (libcluu::ipc) ───────────────────────── */

#define CLUU_COMP_WIN_REGISTER_LABEL      90u
#define CLUU_COMP_WIN_REGISTER_REPLY      91u
#define CLUU_COMP_WIN_DESTROY_LABEL       93u
#define CLUU_COMP_INPUT_FORWARD_LABEL     96u
#define CLUU_COMP_FRAME_READY_LABEL       100u

/* ── Registry protocol labels ────────────────────────────────────────── */

#define CLUU_REGISTRY_SUBSCRIBE_LABEL      0x103u
#define CLUU_REGISTRY_GRANT_DELIVER_LABEL  0x106u

/* ── Displayd error codes (cluu_wire::display::Error) ────────────────── */

#define CLUU_DISP_ERR_NO_FREE_BUFFER    1
#define CLUU_DISP_ERR_DOUBLE_COMMIT     2
#define CLUU_DISP_ERR_STALE_SEQUENCE    3
#define CLUU_DISP_ERR_FOREIGN_SURFACE   4
#define CLUU_DISP_ERR_UNACQUIRED_BUFFER 5
#define CLUU_DISP_ERR_INVALID_RECT      6
#define CLUU_DISP_ERR_PITCH_OVERFLOW    7
#define CLUU_DISP_ERR_BUFFER_OVERFLOW   8
#define CLUU_DISP_ERR_INVALID_CAPABILITY 9

/* ── Per-window driver data ──────────────────────────────────────────── */

#define CLUU_MAX_DAMAGE_RECTS 8

typedef struct {
    /* displayd surface */
    unsigned long long surface_token;   /* 0 = no surface */
    unsigned int       surf_w;
    unsigned int       surf_h;
    unsigned int       surf_pitch;

    /* frame token for pixel transfer */
    unsigned long long frame_token;
    void              *frame_va;        /* mapped virtual address */
    unsigned int       frame_pages;
    unsigned int       frame_bytes;

    /* compositor window */
    unsigned long long win_id;          /* 0 = no compositor window */
    unsigned long      shm_token;       /* compositor SHM token (unused for pixels) */

    /* state flags */
    unsigned char      fullscreen;
    unsigned char      visible;
    unsigned char      destroyed;
} CLUU_WindowData;

/* ── Per-device driver data ──────────────────────────────────────────── */

typedef struct {
    /* endpoints */
    unsigned long displayd_ep;     /* from PARAM_DISPLAYD_EP */
    unsigned long comp_ep;         /* from registry subscribe "compositor:client" */
    unsigned long input_ep;        /* our own endpoint for receiving events */
    unsigned long registry_ep;     /* from tokens[TOKEN_REGISTRY] */
    unsigned long control_ep;      /* for registry grant delivery */

    /* capability tokens */
    unsigned long ipc_cap;         /* tokens[TOKEN_IPC] */
    unsigned long space_cap;       /* tokens[TOKEN_SPACE] */
    unsigned long clock_cap;       /* tokens[TOKEN_CLOCK] */

    /* display dimensions from displayd */
    unsigned int screen_w;
    unsigned int screen_h;
    unsigned int screen_pitch;

    /* surface leak counter (for 100 init/quit test) */
    int surface_count;
} CLUU_DeviceData;

/* ── Inline syscall (matches kernel ABI: rax=nr, rdi..r9=args) ──────── */

static __inline__ long cluu_syscall6(unsigned long nr,
    unsigned long a1, unsigned long a2, unsigned long a3,
    unsigned long a4, unsigned long a5, unsigned long a6)
{
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(a4), "r"(a5), "r"(a6)
        : "rcx", "r11", "r12", "r13", "r14", "r15", "memory"
    );
    /* a4 is r10, a5 is r8, a6 is r9 — but the "r" constraint lets the
     * compiler pick. We need to force r10/r8/r9 for the kernel ABI. */
    return ret;
}

/* The generic "r" constraint above does not guarantee r10/r8/r9.
 * Use explicit register constraints for correctness. */
static __inline__ long cluu_syscall(unsigned long nr,
    unsigned long a1, unsigned long a2, unsigned long a3,
    unsigned long a4, unsigned long a5, unsigned long a6)
{
    long ret;
    register unsigned long r10 __asm__("r10") = a4;
    register unsigned long r8  __asm__("r8")  = a5;
    register unsigned long r9  __asm__("r9")  = a6;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "r12", "r13", "r14", "r15", "memory"
    );
    return ret;
}

/* ── IPC helper wrappers ─────────────────────────────────────────────── */

/* Send a byte buffer to an endpoint. Returns 0 on success, -1 on error. */
static __inline__ int cluu_ipc_send(unsigned long ep, const void *buf, unsigned long len)
{
    long ret = cluu_syscall(CLUU_SYS_SEND, ep, (unsigned long)buf, len, 0, 0, 0);
    return ret < 0 ? -1 : 0;
}

/* Call (send + receive). Returns reply byte count, or -1 on error. */
static __inline__ long cluu_ipc_call(unsigned long ep,
    const void *msg, unsigned long msg_len,
    void *reply, unsigned long reply_len)
{
    long ret = cluu_syscall(CLUU_SYS_CALL, ep,
        (unsigned long)msg, msg_len,
        (unsigned long)reply, reply_len, 0);
    return ret;
}

/* Receive from any endpoint in the tokens array.
 * Returns (index << 32 | msg_len) or negative error. */
static __inline__ long cluu_ipc_recv_any(
    const unsigned long *tokens, unsigned long num_tokens,
    void *buf, unsigned long buf_len, unsigned long timeout_ms)
{
    unsigned long sender_tid = 0;
    long ret = cluu_syscall(CLUU_SYS_RECV,
        (unsigned long)tokens, num_tokens,
        (unsigned long)buf, buf_len, timeout_ms,
        (unsigned long)&sender_tid);
    return ret;
}

/* Invoke an operation on a token. Returns value or negative error. */
static __inline__ long cluu_invoke(unsigned long token, unsigned long op,
    unsigned long a1, unsigned long a2, unsigned long a3, unsigned long a4)
{
    return cluu_syscall(CLUU_SYS_INVOKE, token, op, a1, a2, a3, a4);
}

/* ── Message construction helpers ────────────────────────────────────── */

static __inline__ void cluu_msg_init(CLUU_Message *m, unsigned int label,
    unsigned long w0, unsigned long w1, unsigned long w2,
    unsigned long w3, unsigned long w4, unsigned long w5,
    unsigned char word_count)
{
    m->tag.label = label;
    m->tag.words = word_count;
    m->tag.extra = 0;
    m->tag._pad  = 0;
    m->words[0] = w0;
    m->words[1] = w1;
    m->words[2] = w2;
    m->words[3] = w3;
    m->words[4] = w4;
    m->words[5] = w5;
}

/* Send a Message header + payload as a single IPC buffer.
 * The kernel receives header bytes followed by payload bytes. */
static __inline__ int cluu_send_msg_with_payload(unsigned long ep,
    const CLUU_Message *msg, const void *payload, unsigned long payload_len)
{
    char buf[256];
    unsigned long total = CLUU_MSG_SIZE + payload_len;
    if (total > sizeof(buf)) {
        return -1;
    }
    SDL_memcpy(buf, msg, CLUU_MSG_SIZE);
    if (payload && payload_len) {
        SDL_memcpy(buf + CLUU_MSG_SIZE, payload, payload_len);
    }
    return cluu_ipc_send(ep, buf, total);
}

/* Call with payload: send header+payload, receive reply into reply_msg. */
static __inline__ int cluu_call_with_payload(unsigned long ep,
    const CLUU_Message *msg, const void *payload, unsigned long payload_len,
    CLUU_Message *reply)
{
    char sendbuf[256];
    unsigned long total = CLUU_MSG_SIZE + payload_len;
    if (total > sizeof(sendbuf)) {
        return -1;
    }
    SDL_memcpy(sendbuf, msg, CLUU_MSG_SIZE);
    if (payload && payload_len) {
        SDL_memcpy(sendbuf + CLUU_MSG_SIZE, payload, payload_len);
    }
    long ret = cluu_ipc_call(ep, sendbuf, total, reply, CLUU_MSG_SIZE);
    return ret < 0 ? -1 : 0;
}

/* ── ProcessInfo access ──────────────────────────────────────────────── */

static __inline__ CLUU_ProcessInfo *cluu_process_info(void)
{
    return (CLUU_ProcessInfo *)(unsigned long)CLUU_PROCESS_INFO_ADDR;
}

static __inline__ unsigned long cluu_token(int index)
{
    return cluu_process_info()->tokens[index];
}

static __inline__ unsigned long long cluu_param(int index)
{
    return cluu_process_info()->params[index];
}

/* ── VideoInit / VideoQuit ───────────────────────────────────────────── */

extern int CLUU_VideoInit(_THIS);
extern void CLUU_VideoQuit(_THIS);

/* ── Window operations ───────────────────────────────────────────────── */

extern int CLUU_CreateSDLWindow(_THIS, SDL_Window *window);
extern void CLUU_DestroyWindow(_THIS, SDL_Window *window);
extern void CLUU_ShowWindow(_THIS, SDL_Window *window);
extern void CLUU_HideWindow(_THIS, SDL_Window *window);
extern void CLUU_RaiseWindow(_THIS, SDL_Window *window);
extern void CLUU_SetWindowFullscreen(_THIS, SDL_Window *window, SDL_VideoDisplay *display, SDL_bool fullscreen);

/* ── Framebuffer operations ──────────────────────────────────────────── */

extern int CLUU_CreateWindowFramebuffer(_THIS, SDL_Window *window, Uint32 *format, void **pixels, int *pitch);
extern int CLUU_UpdateWindowFramebuffer(_THIS, SDL_Window *window, const SDL_Rect *rects, int numrects);
extern void CLUU_DestroyWindowFramebuffer(_THIS, SDL_Window *window);

/* ── Events ──────────────────────────────────────────────────────────── */

extern void CLUU_PumpEvents(_THIS);

static __inline__ CLUU_DeviceData *cluu_device_data(_THIS)
{
    return (CLUU_DeviceData *)_this->driverdata;
}

static __inline__ CLUU_WindowData *cluu_window_data(SDL_Window *window)
{
    return (CLUU_WindowData *)window->driverdata;
}

#endif /* SDL_cluuvideo_h_ */

/* vi: set ts=4 sw=4 expandtab: */
