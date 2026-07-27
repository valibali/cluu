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

#ifdef SDL_AUDIO_DRIVER_CLUU

/* CLUU audio backend — audiod stream lifecycle + virtio-snd PCM transport.
 *
 * See SDL_cluuaudio.h for the architecture overview and teardown lock
 * ordering. The backend uses a local SPSC FrameRing (matching
 * audiod/src/ring.rs) between the SDL callback and the virtio-snd submit
 * path. WaitDevice drains the ring and blocks on completions — never
 * polling, never dropping. Output is fixed S16 stereo 44100 Hz; SDL's
 * AudioStream converter handles application formats. */

#include "SDL_timer.h"
#include "SDL_audio.h"
#include "SDL_hints.h"
#include "../SDL_audio_c.h"
#include "SDL_cluuaudio.h"

/* ── FrameRing (local implementation matching audiod/src/ring.rs) ────── */

static void cluu_ring_init(unsigned char *buf, unsigned int capacity)
{
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    h->magic = CLUU_FRAME_RING_MAGIC;
    h->capacity = capacity;
    h->write_idx = 0;
    h->read_idx = 0;
    h->total_written = 0;
    h->total_read = 0;
    h->xrun_count = 0;
    h->reserved = 0;
    __atomic_thread_fence(__ATOMIC_RELEASE);
}

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

static unsigned int cluu_ring_available_write(unsigned char *buf, unsigned int capacity)
{
    /* One slot reserved to distinguish full from empty. */
    return (capacity - 1) - cluu_ring_available_read(buf);
}

/* Push up to n_frames stereo S16 frames into the ring. Returns frames
 * actually written. On overrun (ring full), increments xrun counter. */
static unsigned int cluu_ring_push(unsigned char *buf, unsigned int capacity,
                                    const void *src, unsigned int n_frames)
{
    if (n_frames == 0) {
        return 0;
    }
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    unsigned char *data = buf + CLUU_FRAME_RING_HEADER_BYTES;
    unsigned int avail = cluu_ring_available_write(buf, capacity);
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

/* Pop up to n_frames stereo S16 frames from the ring. Returns frames
 * actually read. On empty ring, returns 0 (underrun — caller feeds silence). */
static unsigned int cluu_ring_pop(unsigned char *buf, unsigned int capacity,
                                   void *dst, unsigned int n_frames)
{
    if (n_frames == 0) {
        return 0;
    }
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    unsigned char *data = buf + CLUU_FRAME_RING_HEADER_BYTES;
    unsigned int avail = cluu_ring_available_read(buf);
    unsigned int to_read = n_frames < avail ? n_frames : avail;
    if (to_read == 0) {
        return 0;
    }
    unsigned int read = __atomic_load_n(&h->read_idx, __ATOMIC_ACQUIRE) % capacity;
    unsigned char *d = (unsigned char *)dst;
    for (unsigned int i = 0; i < to_read; i++) {
        unsigned int idx = (read + i) % capacity;
        unsigned int offset = idx * CLUU_FRAME_BYTES;
        SDL_memcpy(d + i * CLUU_FRAME_BYTES, data + offset, CLUU_FRAME_BYTES);
    }
    __atomic_thread_fence(__ATOMIC_RELEASE);
    unsigned int new_read = (read + to_read) % capacity;
    __atomic_store_n(&h->read_idx, new_read, __ATOMIC_RELEASE);
    __atomic_fetch_add(&h->total_read, to_read, __ATOMIC_RELAXED);
    return to_read;
}

static unsigned int cluu_ring_xrun_count(unsigned char *buf)
{
    CLUU_FrameRingHeader *h = (CLUU_FrameRingHeader *)buf;
    return __atomic_load_n(&h->xrun_count, __ATOMIC_RELAXED);
}

/* ── Registry subscribe (C implementation, matches cluuvideo.c) ──────── */

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
            if (reply.tag.label == CLUU_REGISTRY_GRANT_DELIVER_LABEL) {
                return reply.words[1];
            }
        }
    }
    return 0;
}

/* ── Audiod stream lifecycle helpers ─────────────────────────────────── */

static int cluu_audiod_open_stream(SDL_PrivateAudioData *h)
{
    CLUU_Message req, reply;
    long ret;

    /* words[0] = session_id (0 = let audiod assign),
     * words[1] = rate (44100), words[2] = channels (2) */
    cluu_msg_init(&req, CLUU_AUDIOD_STREAM_OPEN,
        0, CLUU_AUDIO_OUTPUT_RATE, CLUU_AUDIO_OUTPUT_CHANNELS,
        0, 0, 0, 3);

    ret = cluu_ipc_call(h->audiod_ep, &req, CLUU_MSG_SIZE, &reply, CLUU_MSG_SIZE);
    if (ret < 0) {
        return -1;
    }
    /* reply: [status=0, stream_id, session_id, ...] */
    if (reply.tag.label != CLUU_AUDIOD_STREAM_OPEN || reply.words[0] != 0) {
        return -1;
    }
    h->audiod_stream_id = (unsigned int)reply.words[1];
    h->audiod_session_id = (unsigned int)reply.words[2];
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

/* ── virtio-snd session helpers ──────────────────────────────────────── */

static int cluu_vsnd_open_session(SDL_PrivateAudioData *h)
{
    CLUU_Message req, reply;
    long ret;

    /* Create completion endpoint. */
    h->completion_ep = (unsigned long)cluu_invoke(h->ipc_cap,
        CLUU_INVOKE_ENDPOINT_CREATE, 0, 0, 0, 0);
    if ((long)h->completion_ep < 0) {
        h->completion_ep = 0;
        return -1;
    }

    /* AUDIO_OPEN_SESSION: words = [completion_ep, format, rate, channels] */
    cluu_msg_init(&req, CLUU_AUDIO_OPEN_SESSION,
        h->completion_ep, CLUU_PCM_FMT_S16, CLUU_PCM_RATE_44100,
        CLUU_AUDIO_OUTPUT_CHANNELS, 0, 0, 4);

    ret = cluu_ipc_call(h->snddev_ep, &req, CLUU_MSG_SIZE, &reply, CLUU_MSG_SIZE);
    if (ret < 0) {
        return -1;
    }
    /* reply: [status=0, session_id, driver_space_token, grant_target_va] */
    if (reply.tag.label != CLUU_AUDIO_OPEN_SESSION || reply.words[0] != 0) {
        return -1;
    }
    h->snd_session_id = (unsigned int)reply.words[1];
    h->snd_driver_space_token = reply.words[2];
    h->snd_grant_target_va = reply.words[3];
    return 0;
}

static int cluu_vsnd_map_and_grant(SDL_PrivateAudioData *h)
{
    long ret;
    unsigned int i;

    /* Map CLUU_AUDIO_VSND_SLOTS pages at AUDIO_SCRATCH_VA for PCM buffers.
     * flags 0x03 = user read + write. */
    ret = cluu_invoke(h->space_cap, CLUU_INVOKE_SPACE_MAP_RANGE,
        CLUU_AUDIO_SCRATCH_VA, 0, 0x03, CLUU_AUDIO_VSND_SLOTS);
    if (ret < 0) {
        return -1;
    }

    /* Grant each page to virtio-snd at the driver's grant target VA. */
    for (i = 0; i < CLUU_AUDIO_VSND_SLOTS; i++) {
        unsigned long src_va = CLUU_AUDIO_SCRATCH_VA + i * CLUU_AUDIO_PAGE_BYTES;
        unsigned long dst_va = h->snd_grant_target_va + i * CLUU_AUDIO_PAGE_BYTES;
        ret = cluu_invoke(h->space_cap, CLUU_INVOKE_SPACE_GRANT,
            h->snd_driver_space_token, src_va, dst_va, 0);
        if (ret < 0) {
            return -1;
        }
    }
    return 0;
}

static void cluu_vsnd_submit_period(SDL_PrivateAudioData *h, unsigned int slot,
                                     unsigned int len)
{
    CLUU_Message msg;
    unsigned int pid = h->vsnd_next_period_id;
    h->vsnd_next_period_id++;

    /* AUDIO_SUBMIT_PCM: words = [session_id, period_id, len, page_index] */
    cluu_msg_init(&msg, CLUU_AUDIO_SUBMIT_PCM,
        h->snd_session_id, pid, len, slot, 0, 0, 4);
    cluu_ipc_send(h->snddev_ep, &msg, CLUU_MSG_SIZE);
    h->vsnd_inflight++;
}

static int cluu_vsnd_drain_completions(SDL_PrivateAudioData *h)
{
    unsigned long tokens[1];
    char buf[128];
    CLUU_Message *m;
    long ret;
    int count = 0;

    tokens[0] = h->completion_ep;
    for (;;) {
        ret = cluu_ipc_recv_any(tokens, 1, buf, sizeof(buf), 0);
        if (ret < 0) {
            break;  /* timeout or error — no more completions */
        }
        if (ret >= (long)CLUU_MSG_SIZE) {
            m = (CLUU_Message *)buf;
            if (m->tag.label == CLUU_AUDIO_COMPLETE) {
                if (h->vsnd_inflight > 0) {
                    h->vsnd_inflight--;
                }
                count++;
            }
        }
    }
    return count;
}

static int cluu_vsnd_wait_completion(SDL_PrivateAudioData *h, unsigned long timeout_ms)
{
    unsigned long tokens[1];
    char buf[128];
    CLUU_Message *m;
    long ret;

    tokens[0] = h->completion_ep;
    ret = cluu_ipc_recv_any(tokens, 1, buf, sizeof(buf), timeout_ms);
    if (ret < 0) {
        return -1;  /* timeout */
    }
    if (ret >= (long)CLUU_MSG_SIZE) {
        m = (CLUU_Message *)buf;
        if (m->tag.label == CLUU_AUDIO_COMPLETE) {
            if (h->vsnd_inflight > 0) {
                h->vsnd_inflight--;
            }
            return 1;
        }
    }
    return 0;
}

static void cluu_vsnd_close(SDL_PrivateAudioData *h)
{
    CLUU_Message msg;
    cluu_msg_init(&msg, CLUU_AUDIO_CLOSE,
        h->snd_session_id, 0, 0, 0, 0, 0, 1);
    cluu_ipc_send(h->snddev_ep, &msg, CLUU_MSG_SIZE);
}

/* ── Submit pending periods from ring to virtio-snd ──────────────────── */

static void cluu_submit_pending(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;
    unsigned char slot_buf[CLUU_AUDIO_PERIOD_BYTES];

    while (cluu_ring_available_read(h->ring_buf) >= CLUU_AUDIO_PERIOD_FRAMES &&
           h->vsnd_inflight < CLUU_AUDIO_VSND_SLOTS) {
        unsigned int got = cluu_ring_pop(h->ring_buf, h->ring_capacity,
                                          slot_buf, CLUU_AUDIO_PERIOD_FRAMES);
        if (got < CLUU_AUDIO_PERIOD_FRAMES) {
            /* Partial — pad with silence and submit (EOF partial period). */
            if (got > 0) {
                SDL_memset(slot_buf + got * CLUU_FRAME_BYTES,
                           _this->spec.silence,
                           (CLUU_AUDIO_PERIOD_FRAMES - got) * CLUU_FRAME_BYTES);
                unsigned long dst = CLUU_AUDIO_SCRATCH_VA +
                    h->vsnd_next_slot * CLUU_AUDIO_PAGE_BYTES;
                SDL_memcpy((void *)dst, slot_buf, CLUU_AUDIO_PERIOD_BYTES);
                cluu_vsnd_submit_period(h, h->vsnd_next_slot, CLUU_AUDIO_PERIOD_BYTES);
                h->vsnd_next_slot = (h->vsnd_next_slot + 1) % CLUU_AUDIO_VSND_SLOTS;
            }
            break;
        }
        /* Copy period to virtio-snd scratch slot and submit. */
        unsigned long dst = CLUU_AUDIO_SCRATCH_VA +
            h->vsnd_next_slot * CLUU_AUDIO_PAGE_BYTES;
        SDL_memcpy((void *)dst, slot_buf, CLUU_AUDIO_PERIOD_BYTES);
        cluu_vsnd_submit_period(h, h->vsnd_next_slot, CLUU_AUDIO_PERIOD_BYTES);
        h->vsnd_next_slot = (h->vsnd_next_slot + 1) % CLUU_AUDIO_VSND_SLOTS;
    }
}

/* ── SDL audio backend interface ─────────────────────────────────────── */

static SDL_bool CLUU_AudioAvailable(void)
{
    /* Activated only when SDL_AUDIODRIVER=cluu is set explicitly.
     * Avoids touching ProcessInfo memory on non-CLUU hosts. */
    const char *envr = SDL_GetHint(SDL_HINT_AUDIODRIVER);
    if (envr && SDL_strcmp(envr, "cluu") == 0) {
        return SDL_TRUE;
    }
    return SDL_FALSE;
}

static int CLUU_OpenDevice(_THIS, const char *devname)
{
    SDL_PrivateAudioData *h;
    unsigned long long audiod_ep_param;

    (void)devname;

    /* Allocate private data. */
    h = (SDL_PrivateAudioData *)SDL_calloc(1, sizeof(SDL_PrivateAudioData));
    if (!h) {
        return SDL_OutOfMemory();
    }
    _this->hidden = h;

    /* Read capability tokens. */
    h->ipc_cap     = cluu_token(CLUU_TOKEN_IPC);
    h->space_cap   = cluu_token(CLUU_TOKEN_SPACE);
    h->registry_ep = cluu_token(CLUU_TOKEN_REGISTRY);

    if (h->ipc_cap == 0 || h->space_cap == 0) {
        return SDL_SetError("CLUU audio: missing capability tokens");
    }

    /* Create control endpoint for registry grant delivery. */
    h->control_ep = (unsigned long)cluu_invoke(h->ipc_cap,
        CLUU_INVOKE_ENDPOINT_CREATE, 0, 0, 0, 0);
    if ((long)h->control_ep < 0) {
        h->control_ep = 0;
        return SDL_SetError("CLUU audio: failed to create control endpoint");
    }

    /* Get audiod endpoint from PARAM_AUDIOD_EP. */
    audiod_ep_param = cluu_param(CLUU_PARAM_AUDIOD_EP);
    h->audiod_ep = (unsigned long)audiod_ep_param;
    if (h->audiod_ep == 0) {
        return SDL_SetError("CLUU audio: audiod endpoint not configured");
    }

    /* Subscribe to snddev:main for virtio-snd PCM transport. */
    h->snddev_ep = cluu_registry_subscribe(h, "snddev", "main");
    if (h->snddev_ep == 0) {
        return SDL_SetError("CLUU audio: failed to subscribe to snddev:main");
    }

    /* Open virtio-snd session (S16, 44100 Hz, stereo). */
    if (cluu_vsnd_open_session(h) < 0) {
        return SDL_SetError("CLUU audio: virtio-snd session open failed");
    }

    /* Map and grant PCM scratch pages to virtio-snd. */
    if (cluu_vsnd_map_and_grant(h) < 0) {
        return SDL_SetError("CLUU audio: failed to map/grant PCM pages");
    }

    /* Open audiod stream (lifecycle management). */
    if (cluu_audiod_open_stream(h) < 0) {
        return SDL_SetError("CLUU audio: audiod stream open failed");
    }

    /* Allocate local FrameRing (8 periods of buffering). */
    h->ring_capacity = CLUU_AUDIO_RING_CAPACITY;
    h->ring_buf = (unsigned char *)SDL_malloc(CLUU_AUDIO_RING_BYTES);
    if (!h->ring_buf) {
        return SDL_OutOfMemory();
    }
    cluu_ring_init(h->ring_buf, h->ring_capacity);

    /* Allocate scratch buffer (one period) for GetDeviceBuf. */
    h->scratch_len = CLUU_AUDIO_PERIOD_BYTES;
    h->scratch = (unsigned char *)SDL_malloc(h->scratch_len);
    if (!h->scratch) {
        return SDL_OutOfMemory();
    }
    SDL_memset(h->scratch, 0, h->scratch_len);

    /* Fixed output format: S16 stereo 44100 Hz, 512-sample periods.
     * SDL's AudioStream converter handles application formats. */
    _this->spec.format = AUDIO_S16LSB;
    _this->spec.freq = CLUU_AUDIO_OUTPUT_RATE;
    _this->spec.channels = CLUU_AUDIO_OUTPUT_CHANNELS;
    _this->spec.samples = CLUU_AUDIO_PERIOD_FRAMES;
    SDL_CalculateAudioSpec(&_this->spec);

    return 0;
}

static Uint8 *CLUU_GetDeviceBuf(_THIS)
{
    /* Return the scratch buffer. SDL fills it via callback or AudioStreamGet.
     * PlayDevice commits it to the ring. Non-blocking, always succeeds. */
    return _this->hidden->scratch;
}

static void CLUU_PlayDevice(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;

    /* Push the scratch buffer (one period) into the local FrameRing.
     * If the ring is full, the push returns 0 and increments xrun counter.
     * The xrun will be resolved in WaitDevice (which drains the ring). */
    unsigned int pushed = cluu_ring_push(h->ring_buf, h->ring_capacity,
                                          h->scratch,
                                          CLUU_AUDIO_PERIOD_FRAMES);
    if (pushed < CLUU_AUDIO_PERIOD_FRAMES) {
        h->ring_xruns++;
    }
}

static void CLUU_WaitDevice(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;

    /* Phase 1: Submit as many pending periods as possible from ring to
     * virtio-snd. This drains the ring, making space for the next period. */
    cluu_submit_pending(_this);

    /* Phase 2: Drain completions (non-blocking) — frees virtio-snd slots. */
    cluu_vsnd_drain_completions(h);

    /* Phase 3: If the ring cannot accept the next period (available_write
     * < period_frames), block until a virtio-snd completion frees space.
     * This is the "block on low-water/completion" path — never polling,
     * never dropping. The 500ms timeout bounds wakeup if audiod or
     * virtio-snd dies. */
    while (cluu_ring_available_write(h->ring_buf, h->ring_capacity) <
           CLUU_AUDIO_PERIOD_FRAMES) {

        if (h->vsnd_inflight > 0) {
            /* Wait for a completion (blocking, bounded by timeout). */
            int got = cluu_vsnd_wait_completion(h, CLUU_AUDIO_COMPLETION_TIMEOUT_MS);
            if (got < 0) {
                /* Timeout — no completion within 500ms.
                 * If we still have inflight periods, the driver is alive
                 * but slow. If inflight is zero and ring is full, something
                 * is wrong — signal disconnection. */
                if (h->vsnd_inflight == 0) {
                    SDL_OpenedAudioDeviceDisconnected(_this);
                    return;
                }
                /* else: retry — driver is still processing. */
            }
            /* Try to submit more after the completion. */
            cluu_submit_pending(_this);
        } else {
            /* No inflight periods but ring is still too full to accept
             * the next period. This means we have data in the ring but
             * all virtio-snd slots are occupied (shouldn't happen since
             * we just drained completions). Wait briefly and retry. */
            cluu_submit_pending(_this);
            if (cluu_ring_available_write(h->ring_buf, h->ring_capacity) <
                CLUU_AUDIO_PERIOD_FRAMES) {
                /* Still full — sleep briefly to avoid busy-loop, then
                 * signal disconnect if this persists. */
                SDL_Delay(10);
            }
        }
    }
}

static void CLUU_CloseDevice(_THIS)
{
    SDL_PrivateAudioData *h = _this->hidden;
    if (!h) {
        return;
    }

    /* Teardown lock ordering (see SDL_cluuaudio.h):
     *   1. SDL has already set shutdown=1 and the audio thread has exited.
     *   2. Drain: submit remaining ring data to virtio-snd.
     *   3. Wait for all in-flight periods to complete (bounded). */
    if (h->ring_buf) {
        cluu_submit_pending(_this);
        /* Wait for inflight periods to complete (up to 2s total). */
        int wait_cycles = 0;
        while (h->vsnd_inflight > 0 && wait_cycles < 20) {
            cluu_vsnd_wait_completion(h, 100);
            wait_cycles++;
        }
    }

    /* 4. Close audiod stream (fire-and-forget). */
    if (h->audiod_ep != 0 && h->audiod_stream_id != 0) {
        cluu_audiod_send_simple(h, CLUU_AUDIOD_STREAM_CLOSE);
    }

    /* 5. Close virtio-snd session (fire-and-forget). */
    if (h->snddev_ep != 0 && h->snd_session_id != 0) {
        cluu_vsnd_close(h);
    }

    /* 6. Free resources. */
    SDL_free(h->ring_buf);
    SDL_free(h->scratch);
    SDL_free(h);
    _this->hidden = NULL;
}

static void CLUU_AudioDeinitialize(void)
{
    /* Nothing global to clean up — all resources are per-device. */
}

static SDL_bool CLUU_AudioInit(SDL_AudioDriverImpl *impl)
{
    /* Set the function pointers */
    impl->OpenDevice = CLUU_OpenDevice;
    impl->WaitDevice = CLUU_WaitDevice;
    impl->PlayDevice = CLUU_PlayDevice;
    impl->GetDeviceBuf = CLUU_GetDeviceBuf;
    impl->CloseDevice = CLUU_CloseDevice;
    impl->Deinitialize = CLUU_AudioDeinitialize;

    /* Default output only — no capture, no full-duplex. */
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
