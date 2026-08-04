/*
  Simple DirectMedia Layer
  Copyright (C) 1997-2024 Sam Lantinga <slouken@libsdl.org>

  This software is provided 'as-is', without any express or implied
  warranty.  In no event will the authors be held liable for any damages
  arising from the use of this software.

  Permission is granted to anyone to use this software for any purpose,
  including commercial applications, and to alter it freely, subject to
  the following restrictions:

  1. The origin of this software must not be misrepresented; you must not
     claim that you wrote the original software.
  2. Altered source versions must be plainly marked as such.
  3. This notice may not be removed from any source distribution.
*/
#include "../../SDL_internal.h"

#ifdef SDL_AUDIO_DRIVER_CLUU

/* CLUU audio backend — audiod SHM ring protocol.
 *
 * Talks only to audiod (registry "audiod:main"). Opens a stream, receives
 * a frame token for a SHM SPSC FrameRing, maps it, and pushes PCM frames.
 * audiod handles mixing, resampling, and virtio-snd submission.
 *
 * The FrameRing layout matches audiod/src/ring.rs. The ring is SPSC:
 * SDL is the sole producer (PlayDevice), audiod is the sole consumer.
 *
 * Output format is fixed: stereo S16 at audiod's native rate. SDL's
 * AudioStream converter handles application format conversion. */

#include "SDL_timer.h"
#include "SDL_audio.h"
#include "SDL_hints.h"
#include "../SDL_audio_c.h"
#include "SDL_cluuaudio.h"

extern void debug_print(const char *msg);

/* ── FrameRing operations (matches audiod/src/ring.rs) ───────────────── */

static unsigned int cluu_ring_available_read(unsigned char *buf)
{
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    unsigned int write = __atomic_load_n(&h->write_idx, __ATOMIC_ACQUIRE) % h->capacity;
    unsigned int read  = __atomic_load_n(&h->read_idx, __ATOMIC_ACQUIRE) % h->capacity;
    if (write >= read) {
        return write - read;
    }
    return h->capacity - read + write;
}

static unsigned int cluu_ring_available_write(unsigned char *buf)
{
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    return (h->capacity - 1) - cluu_ring_available_read(buf);
}

static unsigned int cluu_ring_push(unsigned char *buf,
                                    const void *src, unsigned int n_frames)
{
    if (n_frames == 0) {
        return 0;
    }
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    unsigned int capacity = h->capacity;
    unsigned char *data = buf + CLUU_FRAME_RING_HEADER_BYTES;
    unsigned int avail = cluu_ring_available_write(buf);
    unsigned int to_write = n_frames < avail ? n_frames : avail;
    if (to_write == 0) {
        __atomic_fetch_add(&h->xrun_count, 1, __ATOMIC_RELAXED);
        return 0;
    }
    unsigned int write = __atomic_load_n(&h->write_idx, __ATOMIC_ACQUIRE) % capacity;
    const unsigned char *s = (const unsigned char *)src;
    for (unsigned int i = 0; i < to_write; i++) {
        unsigned int idx = (write + i) % capacity;
        unsigned int offset = idx * CLUU_FRAME_BYTES;
        SDL_memcpy(data + offset, s + i * CLUU_FRAME_BYTES, CLUU_FRAME_BYTES);
    }
    __atomic_thread_fence(__ATOMIC_RELEASE);
    unsigned int new_write = (write + to_write) % capacity;
    __atomic_store_n(&h->write_idx, new_write, __ATOMIC_RELEASE);
    __atomic_fetch_add(&h->total_written, to_write, __ATOMIC_RELAXED);
    return to_write;
}

static unsigned int cluu_ring_total_read(unsigned char *buf)
{
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    return __atomic_load_n(&h->total_read, __ATOMIC_ACQUIRE);
}

/* ── Registry subscribe (shared with video backend) ──────────────────── */

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

static unsigned long cluu_registry_subscribe(SDL_PrivateAudioData *h,
    const char *service, const char *endpoint)
{
    char payload[128];
    int payload_len;
    CLUU_Message msg;
    CLUU_Message reply;
    char recv_buf[256];
    long ret;
    int i;

    if (h->registry_ep == 0 || h->control_ep == 0) {
        return 0;
    }

    payload_len = cluu_registry_encode_names(service, endpoint, payload, sizeof(payload));
    if (payload_len < 0) return 0;

    cluu_msg_init(&msg, CLUU_REGISTRY_SUBSCRIBE_LABEL,
        (unsigned long)payload_len, h->control_ep, 0, 0, 0, 0, 2);

    if (cluu_send_msg_with_payload(h->registry_ep, &msg, payload, payload_len) < 0) {
        return 0;
    }

    for (i = 0; i < 200; i++) {
        unsigned long tokens[1];
        tokens[0] = h->control_ep;
        ret = cluu_ipc_recv_any(tokens, 1, recv_buf, sizeof(recv_buf), 10);
        if (ret < 0) {
            continue;
        }
        if (ret >= (long)CLUU_MSG_SIZE) {
            SDL_memcpy(&reply, recv_buf, CLUU_MSG_SIZE);
            if (reply.tag.label == CLUU_REGISTRY_GRANT_DELIVER_LABEL &&
                reply.tag.words >= 2 && reply.words[1] != 0) {
                return reply.words[1];
            }
        }
    }
    return 0;
}

/* ── audiod stream lifecycle ─────────────────────────────────────────── */

static int cluu_audiod_open_stream(SDL_PrivateAudioData *h,
    unsigned int rate, unsigned char channels)
{
    CLUU_Message req, reply;
    long ret;

    /* AUDIOD_STREAM_OPEN words: [session_id=0, rate, channels, period_bytes, format] */
    cluu_msg_init(&req, CLUU_AUDIOD_STREAM_OPEN,
        0, rate, channels, CLUU_AUDIO_PERIOD_BYTES, CLUU_PCM_FMT_S16, 0, 5);

    ret = cluu_ipc_call_timeout(h->audiod_ep, &req, CLUU_MSG_SIZE, &reply, CLUU_MSG_SIZE, 5000);
    if (ret < 0) {
        return -1;
    }
    /* reply: [status=0, stream_id, session_id, frame_token, ring_bytes, period_bytes] */
    if (reply.tag.label != CLUU_AUDIOD_STREAM_OPEN || reply.words[0] != 0) {
        return -1;
    }
    h->audiod_stream_id = (unsigned int)reply.words[1];
    h->audiod_session_id = (unsigned int)reply.words[2];
    h->ring_frame_token = reply.words[3];
    h->ring_bytes = reply.words[4];
    h->period_bytes = reply.words[5];
    return 0;
}

static void cluu_audiod_send_simple(SDL_PrivateAudioData *h, unsigned int label)
{
    CLUU_Message msg;
    cluu_msg_init(&msg, label,
        h->audiod_session_id, h->audiod_stream_id,
        0, 0, 0, 0, 2);
    cluu_ipc_send(h->audiod_ep, &msg, CLUU_MSG_SIZE);
}

/* ── SDL audio backend interface ─────────────────────────────────────── */

static SDL_bool CLUU_AudioAvailable(void)
{
    const char *envr = SDL_GetHint(SDL_HINT_AUDIODRIVER);
    if (envr && SDL_strcmp(envr, "cluu") == 0) {
        return SDL_TRUE;
    }
    return SDL_FALSE;
}

static int CLUU_OpenDevice(_THIS, const char *devname)
{
    SDL_PrivateAudioData *h;
    unsigned long ring_va;
    unsigned long num_pages;

    (void)devname;

    h = (SDL_PrivateAudioData *)SDL_calloc(1, sizeof(SDL_PrivateAudioData));
    if (!h) {
        return SDL_OutOfMemory();
    }
    _this->hidden = h;

    h->ipc_cap     = cluu_token(CLUU_TOKEN_IPC);
    h->space_cap   = cluu_token(CLUU_TOKEN_SPACE);
    h->registry_ep = cluu_token(CLUU_TOKEN_REGISTRY);

    if (h->ipc_cap == 0 || h->space_cap == 0) {
        return SDL_SetError("CLUU audio: missing capability tokens");
    }

    h->control_ep = (unsigned long)cluu_invoke(h->ipc_cap,
        CLUU_INVOKE_ENDPOINT_CREATE, 0, 0, 0, 0);
    if ((long)h->control_ep < 0) {
        h->control_ep = 0;
        return SDL_SetError("CLUU audio: failed to create control endpoint");
    }

    h->audiod_ep = (unsigned long)cluu_param(CLUU_PARAM_AUDIOD_EP);
    if (h->audiod_ep == 0 && h->registry_ep != 0) {
        h->audiod_ep = cluu_registry_subscribe(h, "audiod", "main");
    }
    if (h->audiod_ep == 0) {
        return SDL_SetError("CLUU audio: audiod endpoint not available");
    }

    /* SDL handles all format conversion via AudioStream. audiod receives
     * fixed S16 stereo 44100 — the ring is always [i16; 2] at 44100. */
    if (cluu_audiod_open_stream(h, CLUU_AUDIO_OUTPUT_RATE, CLUU_AUDIO_OUTPUT_CHANNELS) < 0) {
        return SDL_SetError("CLUU audio: audiod stream open failed");
    }
    debug_print("sdl2-cluu: audiod stream reply received");

    num_pages = (h->ring_bytes + CLUU_AUDIO_PAGE_BYTES - 1) / CLUU_AUDIO_PAGE_BYTES;
    debug_print("sdl2-cluu: mapping audio ring");
    ring_va = (unsigned long)cluu_invoke(h->space_cap, CLUU_INVOKE_SPACE_MAP_AUTO,
            h->ring_frame_token, 0x03, num_pages, 0);
    debug_print("sdl2-cluu: audio ring map returned");
    if ((long)ring_va < 0) {
        return SDL_SetError("CLUU audio: failed to map SHM ring");
    }
    h->ring_buf = (unsigned char *)ring_va;

    h->scratch = (unsigned char *)SDL_malloc(h->period_bytes);
    if (!h->scratch) {
        return SDL_OutOfMemory();
    }
    debug_print("sdl2-cluu: audio scratch allocated");
    SDL_memset(h->scratch, 0, h->period_bytes);

    /* Device format: fixed S16 stereo 44100. SDL's AudioStream converts
     * from the app's requested format to this. */
    _this->spec.format = AUDIO_S16LSB;
    _this->spec.freq = CLUU_AUDIO_OUTPUT_RATE;
    _this->spec.channels = CLUU_AUDIO_OUTPUT_CHANNELS;
    _this->spec.samples = (h->period_bytes / CLUU_FRAME_BYTES);
    SDL_CalculateAudioSpec(&_this->spec);
    debug_print("sdl2-cluu: audio device open complete");

    return 0;
}

static Uint8 *CLUU_GetDeviceBuf(_THIS)
{
    return _this->hidden->scratch;
}

static void CLUU_PlayDevice(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;
    unsigned int period_frames = h->period_bytes / CLUU_FRAME_BYTES;
    unsigned int pushed = cluu_ring_push(h->ring_buf, h->scratch, period_frames);
    if (pushed < period_frames) {
        h->ring_xruns++;
    }
}

static void CLUU_WaitDevice(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;
    unsigned int period_frames = h->period_bytes / CLUU_FRAME_BYTES;

    /* Block until the ring has space for one period.
     * Poll the consumer's total_read counter (audiod updates it).
     * Sleep 2ms between checks to avoid busy-spinning. */
    while (cluu_ring_available_write(h->ring_buf) < period_frames) {
        SDL_Delay(2);
    }
}

static void CLUU_CloseDevice(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;
    if (!h) {
        return;
    }

    if (h->audiod_ep != 0 && h->audiod_stream_id != 0) {
        cluu_audiod_send_simple(h, CLUU_AUDIOD_STREAM_DRAIN);
        cluu_audiod_send_simple(h, CLUU_AUDIOD_STREAM_CLOSE);
    }

    if (h->ring_buf) {
        unsigned long num_pages = (h->ring_bytes + CLUU_AUDIO_PAGE_BYTES - 1) / CLUU_AUDIO_PAGE_BYTES;
        cluu_invoke(h->space_cap, CLUU_INVOKE_SPACE_UNMAP,
            (unsigned long)h->ring_buf, num_pages, 0, 0);
    }
    SDL_free(h->scratch);
    SDL_free(h);
    _this->hidden = NULL;
}

static void CLUU_AudioDeinitialize(void)
{
}

static SDL_bool CLUU_AudioInit(SDL_AudioDriverImpl *impl)
{
    impl->OpenDevice = CLUU_OpenDevice;
    impl->WaitDevice = CLUU_WaitDevice;
    impl->PlayDevice = CLUU_PlayDevice;
    impl->GetDeviceBuf = CLUU_GetDeviceBuf;
    impl->CloseDevice = CLUU_CloseDevice;
    impl->Deinitialize = CLUU_AudioDeinitialize;

    impl->OnlyHasDefaultOutputDevice = SDL_TRUE;
    impl->OnlyHasDefaultCaptureDevice = SDL_FALSE;
    impl->HasCaptureSupport = SDL_FALSE;

    return SDL_TRUE;
}

AudioBootStrap CLUU_bootstrap = {
    "cluu", "CLUU audiod audio driver", CLUU_AudioInit, SDL_TRUE
};

#endif /* SDL_AUDIO_DRIVER_CLUU */
/* vi: set ts=4 sw=4 expandtab: */
