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

#ifndef SDL_cluuaudio_h_
#define SDL_cluuaudio_h_

#include "../SDL_sysaudio.h"

/*
 * CLUU SDL2 audio backend — audiod stream lifecycle + virtio-snd PCM transport.
 *
 * The backend talks to two services:
 *   1. audiod (via PARAM_AUDIOD_EP) for stream lifecycle: open, close, pause,
 *      resume, drain, status. audiod is the authority broker for streams.
 *   2. virtio-snd (via registry "snddev:main") for actual PCM output, since
 *      audiod's mixer path is stubbed in T17. The backend opens its own
 *      AudioSessionClient session with virtio-snd and submits S16 stereo
 *      periods via grant.
 *
 * Between the SDL callback (producer) and the virtio-snd submit (consumer),
 * a local SPSC FrameRing (matching audiod/src/ring.rs) provides bounded
 * buffering. WaitDevice drains the ring to virtio-snd and blocks on
 * completions when the ring is full — never polling, never dropping.
 *
 * Output format is fixed: stereo S16 at 44100 Hz, 512-sample periods
 * (2048 bytes). SDL's AudioStream converter handles application formats.
 *
 * # Teardown lock ordering
 *
 *   1. SDL_AtomicSet(&device->shutdown, 1)  — stop callback loop
 *   2. Audio thread exits SDL_RunAudio
 *   3. CloseDevice: drain ring → flush to virtio-snd → wait completions
 *   4. CloseDevice: AUDIOD_STREAM_CLOSE → AUDIO_CLOSE to virtio-snd
 *   5. CloseDevice: free ring, scratch, hidden
 *
 * No locks are held across IPC calls. The mixer_lock is held only during
 * the callback (by SDL_RunAudio), never during WaitDevice/PlayDevice.
 * CloseDevice runs after the audio thread has exited, so there is no
 * contention on hidden fields.
 */

/* ── ProcessInfo (at PROCESS_INFO_ADDR, #[repr(C)]) ─────────────────── */

#define CLUU_BOOT_INFO_ADDR      0x7fe00000u
#define CLUU_PROCESS_INFO_ADDR   (CLUU_BOOT_INFO_ADDR + 0x100)

#define CLUU_TOKEN_SPACE    5
#define CLUU_TOKEN_IPC      6
#define CLUU_TOKEN_REGISTRY 8

#define CLUU_PARAM_AUDIOD_EP    20

typedef struct {
    unsigned long exit_token;
    unsigned long exit_cookie;
    unsigned long pid;
    unsigned long tokens[17];
    unsigned long long params[32];
} CLUU_ProcessInfo;

/* ── IPC Message (#[repr(C)], 56 bytes) ─────────────────────────────── */

typedef struct {
    unsigned int  label;
    unsigned char words;
    unsigned char extra;
    unsigned short _pad;
} CLUU_MessageTag;

typedef struct {
    CLUU_MessageTag tag;
    unsigned long words[6];
} CLUU_Message;

#define CLUU_MSG_SIZE 56

/* ── Syscall numbers ────────────────────────────────────────────────── */

#define CLUU_SYS_SEND    0
#define CLUU_SYS_RECV    1
#define CLUU_SYS_CALL    2
#define CLUU_SYS_INVOKE  5

/* ── InvokeOp numbers ────────────────────────────────────────────────── */

#define CLUU_INVOKE_ENDPOINT_CREATE  40
#define CLUU_INVOKE_SPACE_GRANT      14
#define CLUU_INVOKE_SPACE_MAP_RANGE  15

/* ── Audiod IPC labels (audiod/src/session.rs) ───────────────────────── */

#define CLUU_AUDIOD_STREAM_OPEN          0x700u
#define CLUU_AUDIOD_STREAM_CLOSE         0x701u
#define CLUU_AUDIOD_STREAM_PAUSE         0x702u
#define CLUU_AUDIOD_STREAM_RESUME        0x703u
#define CLUU_AUDIOD_STREAM_DRAIN         0x704u
#define CLUU_AUDIOD_STREAM_STATUS        0x706u

/* ── virtio-snd IPC labels (libcluu/src/ipc.rs) ──────────────────────── */

#define CLUU_AUDIO_OPEN_SESSION          0x600u
#define CLUU_AUDIO_SUBMIT_PCM            0x601u
#define CLUU_AUDIO_COMPLETE              0x602u
#define CLUU_AUDIO_CLOSE                 0x603u

/* ── PCM format constants (libcluu/src/audio_client.rs) ──────────────── */

#define CLUU_PCM_FMT_S16                 5
#define CLUU_PCM_RATE_44100              6

/* ── Registry protocol labels ────────────────────────────────────────── */

#define CLUU_REGISTRY_SUBSCRIBE_LABEL      0x103u
#define CLUU_REGISTRY_GRANT_DELIVER_LABEL  0x106u

/* ── FrameRing constants (audiod/src/ring.rs) ────────────────────────── */

#define CLUU_FRAME_BYTES                 4   /* stereo S16: 2 x i16 */
#define CLUU_FRAME_RING_MAGIC            0x41554446u  /* "AUDF" */
#define CLUU_FRAME_RING_HEADER_BYTES     32

/* ── Audio output constants ──────────────────────────────────────────── */

#define CLUU_AUDIO_OUTPUT_RATE           44100
#define CLUU_AUDIO_OUTPUT_CHANNELS       2
#define CLUU_AUDIO_PERIOD_FRAMES         512   /* 2048 bytes / 4 bytes */
#define CLUU_AUDIO_PERIOD_BYTES          (CLUU_AUDIO_PERIOD_FRAMES * CLUU_FRAME_BYTES)
#define CLUU_AUDIO_RING_CAPACITY         (CLUU_AUDIO_PERIOD_FRAMES * 8)  /* 8 periods */
#define CLUU_AUDIO_RING_BYTES            (CLUU_FRAME_RING_HEADER_BYTES + CLUU_AUDIO_RING_CAPACITY * CLUU_FRAME_BYTES)
#define CLUU_AUDIO_VSND_SLOTS            4
#define CLUU_AUDIO_SCRATCH_VA            0x70000000u
#define CLUU_AUDIO_PAGE_BYTES            4096
#define CLUU_AUDIO_COMPLETION_TIMEOUT_MS 500   /* bounded wakeup */

/* ── Inline syscall (matches kernel ABI: rax=nr, rdi..r9=args) ──────── */

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

static __inline__ int cluu_ipc_send(unsigned long ep, const void *buf, unsigned long len)
{
    long ret = cluu_syscall(CLUU_SYS_SEND, ep, (unsigned long)buf, len, 0, 0, 0);
    return ret < 0 ? -1 : 0;
}

static __inline__ long cluu_ipc_call(unsigned long ep,
    const void *msg, unsigned long msg_len,
    void *reply, unsigned long reply_len)
{
    return cluu_syscall(CLUU_SYS_CALL, ep,
        (unsigned long)msg, msg_len,
        (unsigned long)reply, reply_len, 0);
}

static __inline__ long cluu_ipc_recv_any(
    const unsigned long *tokens, unsigned long num_tokens,
    void *buf, unsigned long buf_len, unsigned long timeout_ms)
{
    unsigned long sender_tid = 0;
    return cluu_syscall(CLUU_SYS_RECV,
        (unsigned long)tokens, num_tokens,
        (unsigned long)buf, buf_len, timeout_ms,
        (unsigned long)&sender_tid);
}

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

/* Send a Message header + payload as a single IPC buffer. */
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

/* ── FrameRing header (matches audiod/src/ring.rs FrameRingHeader) ──── */

typedef struct {
    volatile unsigned int magic;
    volatile unsigned int capacity;
    volatile unsigned int write_idx;
    volatile unsigned int read_idx;
    volatile unsigned int total_written;
    volatile unsigned int total_read;
    volatile unsigned int xrun_count;
    volatile unsigned int reserved;
} CLUU_FrameRingHeader;

/* ── Private audio data ──────────────────────────────────────────────── */

#define _THIS SDL_AudioDevice *_this

typedef struct SDL_PrivateAudioData SDL_PrivateAudioData;

struct SDL_PrivateAudioData
{
    /* Audiod stream lifecycle */
    unsigned long audiod_ep;
    unsigned int  audiod_stream_id;
    unsigned int  audiod_session_id;

    /* virtio-snd session */
    unsigned long snddev_ep;
    unsigned long completion_ep;
    unsigned int  snd_session_id;
    unsigned long snd_driver_space_token;
    unsigned long snd_grant_target_va;

    /* Capability tokens */
    unsigned long ipc_cap;
    unsigned long space_cap;
    unsigned long registry_ep;
    unsigned long control_ep;

    /* virtio-snd ring slots */
    unsigned int  vsnd_next_slot;
    unsigned int  vsnd_inflight;
    unsigned int  vsnd_next_period_id;

    /* Local FrameRing */
    unsigned char *ring_buf;
    unsigned int  ring_capacity;
    unsigned int  ring_xruns;

    /* Scratch buffer for GetDeviceBuf (one period) */
    unsigned char *scratch;
    unsigned int  scratch_len;
};

#endif /* SDL_cluuaudio_h_ */
/* vi: set ts=4 sw=4 expandtab: */
