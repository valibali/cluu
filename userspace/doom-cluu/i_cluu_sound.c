/* i_cluu_sound.c — DOOM in-process SFX mixer with SDL2 audio output.
 *
 * The 8-channel mixer is unchanged from the original cluu_sound path.
 * Output now goes through SDL2 audio (SDL_QueueAudio + SDL_AudioStream)
 * instead of the former direct cluu_submit_audio path.  The CLUU SDL2
 * audio backend (SDL_cluuaudio.c) is thin and bounded — it opens an
 * audiod stream + virtio-snd session, uses a local SPSC ring, and blocks
 * on completions (never polls, never drops).  SDL_AudioStream handles
 * format conversion from DOOM's S16 mono 11025 Hz to the device's
 * S16 stereo 44100 Hz. */

#include <string.h>
#include <stdlib.h>

#include "doomtype.h"
#include "doomfeatures.h"
#include "i_sound.h"
#include "i_system.h"
#include "m_misc.h"
#include "w_wad.h"
#include "z_zone.h"

#include <SDL.h>

#define NUM_CHANNELS 8
#define DOOM_SAMPLERATE 11025
#define PCM_PERIOD_BYTES 4096

typedef struct {
    byte *data;
    int length;
    int position;
    int playing;
    int vol;
    int sep;
} cluu_channel_t;

static cluu_channel_t channels[NUM_CHANNELS];
static int sound_initialized = 0;

typedef struct {
    int lumpnum;
    byte *data;
    int length;
} cached_sfx_t;

#define MAX_CACHED_SFX 128
static cached_sfx_t cached_sfx[MAX_CACHED_SFX];
static int num_cached_sfx = 0;

static cached_sfx_t *find_cached(int lumpnum)
{
    int i;
    for (i = 0; i < num_cached_sfx; ++i) {
        if (cached_sfx[i].lumpnum == lumpnum) return &cached_sfx[i];
    }
    return NULL;
}

extern void cluu_debug(const char *msg);

/* Config variables referenced by i_sound.c (originally defined in
 * i_sdlsound.c, which is not compiled in the CLUU port). */
int use_libsamplerate = 0;
float libsamplerate_scale = 1.0f;

/* SDL2 audio output state. */
static SDL_AudioDeviceID sdl_audio_dev = 0;
static SDL_AudioStream *audio_stream = NULL;
static SDL_AudioSpec sdl_audio_have;

static boolean sfx_prefix = true;

static void GetSfxLumpName(sfxinfo_t *sfx, char *buf, size_t buf_len)
{
    char *name;
    size_t i, di;

    if (sfx->link != NULL) {
        sfx = sfx->link;
    }

    name = sfx->name;
    if (name == NULL) {
        name = "";
    }

    if (sfx_prefix) {
        buf[0] = 'D';
        buf[1] = 'S';
        di = 2;
    } else {
        di = 0;
    }

    for (i = 0; di < buf_len - 1 && name[i] != '\0'; ++i, ++di) {
        buf[di] = name[i];
    }
    buf[di] = '\0';
}

static boolean CacheSFX(sfxinfo_t *sfxinfo)
{
    int lumpnum;
    unsigned int lumplen;
    unsigned int length;
    byte *data;
    cached_sfx_t *slot;

    lumpnum = sfxinfo->lumpnum;

    if (find_cached(lumpnum)) return true;

    if (num_cached_sfx >= MAX_CACHED_SFX) return false;

    data = W_CacheLumpNum(lumpnum, PU_STATIC);
    lumplen = W_LumpLength(lumpnum);

    if (lumplen < 8 || data[0] != 0x03 || data[1] != 0x00) {
        return false;
    }

    length = (data[7] << 24) | (data[6] << 16) | (data[5] << 8) | data[4];

    if (length > lumplen - 8 || length <= 48) {
        return false;
    }

    data += 16;
    length -= 32;

    slot = &cached_sfx[num_cached_sfx++];
    slot->lumpnum = lumpnum;
    slot->data = data;
    slot->length = (int)length;
    sfxinfo->driver_data = slot;

    return true;
}

static int I_Cluu_GetSfxLumpNum(sfxinfo_t *sfx)
{
    char namebuf[9];
    GetSfxLumpName(sfx, namebuf, sizeof(namebuf));
    return W_GetNumForName(namebuf);
}

static boolean I_Cluu_InitSound(boolean use_sfx_prefix)
{
    int i;
    SDL_AudioSpec want;

    sfx_prefix = use_sfx_prefix;
    for (i = 0; i < NUM_CHANNELS; ++i) {
        channels[i].playing = 0;
        channels[i].position = 0;
        channels[i].data = NULL;
        channels[i].length = 0;
        channels[i].vol = 127;
        channels[i].sep = 128;
    }

    /* Open SDL2 audio device.  Request DOOM's native format (S16, 11025,
     * mono); the CLUU backend forces S16 stereo 44100, so SDL inserts
     * an AudioStream converter.  SDL_AUDIO_ALLOW_ANY_CHANGE accepts the
     * device's actual format. */
    SDL_memset(&want, 0, sizeof(want));
    want.freq = DOOM_SAMPLERATE;
    want.format = AUDIO_S16LSB;
    want.channels = 1;
    want.samples = 512;
    want.callback = NULL;  /* Use SDL_QueueAudio */

    sdl_audio_dev = SDL_OpenAudioDevice(NULL, 0, &want, &sdl_audio_have,
                                         SDL_AUDIO_ALLOW_FREQUENCY_CHANGE |
                                         SDL_AUDIO_ALLOW_CHANNELS_CHANGE);
    if (sdl_audio_dev == 0) {
        cluu_debug("doom-cluu: SDL_OpenAudioDevice failed");
        return false;
    }

    /* Create conversion stream: DOOM format → device format. */
    audio_stream = SDL_NewAudioStream(
        AUDIO_S16LSB, 1, DOOM_SAMPLERATE,
        sdl_audio_have.format, sdl_audio_have.channels, sdl_audio_have.freq);
    if (!audio_stream) {
        cluu_debug("doom-cluu: SDL_NewAudioStream failed");
        SDL_CloseAudioDevice(sdl_audio_dev);
        sdl_audio_dev = 0;
        return false;
    }

    SDL_PauseAudioDevice(sdl_audio_dev, 0);  /* Start playback. */
    sound_initialized = 1;
    cluu_debug("doom-cluu: I_Cluu_InitSound ok (SDL2 audio)");
    return true;
}

static void I_Cluu_ShutdownSound(void)
{
    sound_initialized = 0;
    if (sdl_audio_dev > 0) {
        SDL_CloseAudioDevice(sdl_audio_dev);
        sdl_audio_dev = 0;
    }
    if (audio_stream) {
        SDL_FreeAudioStream(audio_stream);
        audio_stream = NULL;
    }
}

static void I_Cluu_UpdateSoundParams(int handle, int vol, int sep)
{
    if (handle < 0 || handle >= NUM_CHANNELS) return;
    channels[handle].vol = vol;
    channels[handle].sep = sep;
}

static int I_Cluu_StartSound(sfxinfo_t *sfxinfo, int channel, int vol, int sep)
{
    cached_sfx_t *slot;

    if (channel < 0 || channel >= NUM_CHANNELS) return -1;

    if (sfxinfo->driver_data == NULL) {
        if (!CacheSFX(sfxinfo)) {
            cluu_debug("doom-cluu: CacheSFX failed");
            return -1;
        }
    }

    slot = (cached_sfx_t *)sfxinfo->driver_data;
    channels[channel].data = slot->data;
    channels[channel].length = slot->length;
    channels[channel].position = 0;
    channels[channel].playing = 1;
    channels[channel].vol = vol;
    channels[channel].sep = sep;

    return channel;
}

static void I_Cluu_StopSound(int handle)
{
    if (handle < 0 || handle >= NUM_CHANNELS) return;
    channels[handle].playing = 0;
    channels[handle].position = 0;
}

static boolean I_Cluu_SoundIsPlaying(int handle)
{
    if (handle < 0 || handle >= NUM_CHANNELS) return false;
    return channels[handle].playing;
}

static int sfx_submitted = 0;
static int update_called = 0;

#define MAX_TICK_SAMPLES 2048
static short tick_buf[MAX_TICK_SAMPLES];
static unsigned int last_ticks = 0;

extern unsigned int DG_GetTicksMs(void);

static void I_Cluu_UpdateSound(void)
{
    int i, ch;
    unsigned int now, elapsed_ms;
    int tick_samples;

    if (!sound_initialized) return;

    if (!update_called) {
        update_called = 1;
        last_ticks = DG_GetTicksMs();
        cluu_debug("doom-cluu: I_Cluu_UpdateSound first call");
    }

    now = DG_GetTicksMs();
    elapsed_ms = now - last_ticks;
    last_ticks = now;
    if (elapsed_ms > 200) elapsed_ms = 200;

    tick_samples = (int)((unsigned long long)elapsed_ms * DOOM_SAMPLERATE / 1000);
    if (tick_samples > MAX_TICK_SAMPLES) tick_samples = MAX_TICK_SAMPLES;
    if (tick_samples == 0) return;

    for (i = 0; i < tick_samples; ++i) {
        int mixed = 0;
        for (ch = 0; ch < NUM_CHANNELS; ++ch) {
            if (!channels[ch].playing) continue;
            if (channels[ch].position >= channels[ch].length) {
                channels[ch].playing = 0;
                continue;
            }
            int sample = channels[ch].data[channels[ch].position] - 128;
            sample = (sample * channels[ch].vol) / 127;
            mixed += sample;
            channels[ch].position++;
        }
        mixed <<= 8;
        if (mixed > 32767) mixed = 32767;
        if (mixed < -32768) mixed = -32768;
        tick_buf[i] = (short)mixed;
    }

    /* Feed mixed PCM through SDL_AudioStream (format conversion) then
     * queue to the device.  Non-blocking: if the queue is full, excess
     * data is dropped (underrun degrades to silence, not hang). */
    SDL_AudioStreamPut(audio_stream, tick_buf, tick_samples * 2);

    int avail = SDL_AudioStreamAvailable(audio_stream);
    while (avail > 0) {
        Uint8 conv_buf[4096];
        int got = SDL_AudioStreamGet(audio_stream, conv_buf, (int)sizeof(conv_buf));
        if (got <= 0) break;
        SDL_QueueAudio(sdl_audio_dev, conv_buf, (Uint32)got);
        avail = SDL_AudioStreamAvailable(audio_stream);
    }

    if (!sfx_submitted) {
        sfx_submitted = 1;
        cluu_debug("doom-cluu: first PCM period queued via SDL2");
    }
}

static void I_Cluu_PrecacheSounds(sfxinfo_t *sounds, int num_sounds)
{
    int i;
    char namebuf[9];

    for (i = 0; i < num_sounds; ++i) {
        GetSfxLumpName(&sounds[i], namebuf, sizeof(namebuf));
        int lump = W_CheckNumForName(namebuf);
        if (lump >= 0) {
            CacheSFX(&sounds[i]);
        }
    }
}

static snddevice_t cluu_sound_devices[] = { SNDDEVICE_SB };

sound_module_t DG_sound_module =
{
    cluu_sound_devices,
    1,
    I_Cluu_InitSound,
    I_Cluu_ShutdownSound,
    I_Cluu_GetSfxLumpNum,
    I_Cluu_UpdateSound,
    I_Cluu_UpdateSoundParams,
    I_Cluu_StartSound,
    I_Cluu_StopSound,
    I_Cluu_SoundIsPlaying,
    I_Cluu_PrecacheSounds,
};

static boolean I_Cluu_InitMusic(void) { return true; }
static void I_Cluu_ShutdownMusic(void) {}
static void I_Cluu_SetMusicVolume(int vol) {}
static void I_Cluu_PauseMusic(void) {}
static void I_Cluu_ResumeMusic(void) {}
static void *I_Cluu_RegisterSong(void *data, int len) { return NULL; }
static void I_Cluu_UnRegisterSong(void *handle) {}
static void I_Cluu_PlaySong(void *handle, boolean looping) {}
static void I_Cluu_StopSong(void) {}
static boolean I_Cluu_MusicIsPlaying(void) { return false; }
static void I_Cluu_PollMusic(void) {}

static snddevice_t cluu_music_devices[] = { SNDDEVICE_SB };

music_module_t DG_music_module =
{
    cluu_music_devices,
    1,
    I_Cluu_InitMusic,
    I_Cluu_ShutdownMusic,
    I_Cluu_SetMusicVolume,
    I_Cluu_PauseMusic,
    I_Cluu_ResumeMusic,
    I_Cluu_RegisterSong,
    I_Cluu_UnRegisterSong,
    I_Cluu_PlaySong,
    I_Cluu_StopSong,
    I_Cluu_MusicIsPlaying,
    I_Cluu_PollMusic,
};
