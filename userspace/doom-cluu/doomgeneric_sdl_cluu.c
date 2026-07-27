/* doomgeneric_sdl_cluu.c — CLUU local patch of upstream doomgeneric_sdl.c.
 *
 * Local changes from upstream (external/doomgeneric/doomgeneric/doomgeneric_sdl.c):
 *   1. Drop <unistd.h> (unused on CLUU).
 *   2. Make window/renderer/texture static (file-local).
 *   3. DG_Init: call SDL_SetHint to activate CLUU video+audio backends
 *      (no env vars in no_std), then SDL_Init(SDL_INIT_VIDEO|AUDIO).
 *   4. DG_Init: support -fullscreen flag via M_CheckParm.
 *   5. DG_Init: request SDL_RENDERER_SOFTWARE instead of SDL_RENDERER_ACCELERATED
 *      (CLUU has no GPU; software renderer is the only honest path).
 *   6. handleKeyInput: bare exit(1) on SDL_QUIT (no puts/atexit — no stdio
 *      in DOOM panic path).
 *   7. main: throttle with DG_SleepMs(1000/35).  Kept because the displayd
 *      commit IPC is blocking but fast — without a cap DOOM busy-loops and
 *      starves other processes on the single-threaded CLUU runtime.  This
 *      is a fixed frame sleep, NOT render+sleep accumulation: DG_SleepMs
 *      runs after doomgeneric_Tick (which includes render+present), so the
 *      total frame time = tick_time + sleep_time.  If tick_time exceeds
 *      1/35s, sleep is effectively zero (DG_SleepMs(0) is a no-op).  Remove
 *      only after displayd provides vsync feedback or audiod backpressure
 *      is proven to pace the loop (T22).
 *   8. main: cluu_debug diagnostic markers at 1s/5s (CLUU serial trace).
 *
 * Total local diff from upstream: ~25 code lines (well within ≤50).
 */

#include "doomkeys.h"
#include "m_argv.h"
#include "doomgeneric.h"

#include <stdio.h>
#include <stdlib.h>
#include <stdbool.h>
#include <SDL.h>

extern void cluu_debug(const char *msg);

static SDL_Window* window = NULL;
static SDL_Renderer* renderer = NULL;
static SDL_Texture* texture;

#define KEYQUEUE_SIZE 16

static unsigned short s_KeyQueue[KEYQUEUE_SIZE];
static unsigned int s_KeyQueueWriteIndex = 0;
static unsigned int s_KeyQueueReadIndex = 0;

static unsigned char convertToDoomKey(unsigned int key){
  switch (key)
    {
    case SDLK_RETURN:
      key = KEY_ENTER;
      break;
    case SDLK_ESCAPE:
      key = KEY_ESCAPE;
      break;
    case SDLK_LEFT:
      key = KEY_LEFTARROW;
      break;
    case SDLK_RIGHT:
      key = KEY_RIGHTARROW;
      break;
    case SDLK_UP:
      key = KEY_UPARROW;
      break;
    case SDLK_DOWN:
      key = KEY_DOWNARROW;
      break;
    case SDLK_LCTRL:
    case SDLK_RCTRL:
      key = KEY_FIRE;
      break;
    case SDLK_SPACE:
      key = KEY_USE;
      break;
    case SDLK_LSHIFT:
    case SDLK_RSHIFT:
      key = KEY_RSHIFT;
      break;
    case SDLK_LALT:
    case SDLK_RALT:
      key = KEY_LALT;
      break;
    case SDLK_F2:
      key = KEY_F2;
      break;
    case SDLK_F3:
      key = KEY_F3;
      break;
    case SDLK_F4:
      key = KEY_F4;
      break;
    case SDLK_F5:
      key = KEY_F5;
      break;
    case SDLK_F6:
      key = KEY_F6;
      break;
    case SDLK_F7:
      key = KEY_F7;
      break;
    case SDLK_F8:
      key = KEY_F8;
      break;
    case SDLK_F9:
      key = KEY_F9;
      break;
    case SDLK_F10:
      key = KEY_F10;
      break;
    case SDLK_F11:
      key = KEY_F11;
      break;
    case SDLK_EQUALS:
    case SDLK_PLUS:
      key = KEY_EQUALS;
      break;
    case SDLK_MINUS:
      key = KEY_MINUS;
      break;
    default:
      key = tolower(key);
      break;
    }

  return key;
}

static void addKeyToQueue(int pressed, unsigned int keyCode){
  unsigned char key = convertToDoomKey(keyCode);

  unsigned short keyData = (pressed << 8) | key;

  s_KeyQueue[s_KeyQueueWriteIndex] = keyData;
  s_KeyQueueWriteIndex++;
  s_KeyQueueWriteIndex %= KEYQUEUE_SIZE;
}
static void handleKeyInput(){
  SDL_Event e;
  while (SDL_PollEvent(&e)){
    if (e.type == SDL_QUIT){
      exit(1);
    }
    if (e.type == SDL_KEYDOWN) {
      addKeyToQueue(1, e.key.keysym.sym);
    } else if (e.type == SDL_KEYUP) {
      addKeyToQueue(0, e.key.keysym.sym);
    }
  }
}


void DG_Init(){
  /* CLUU: activate CLUU video+audio backends via hints (no env in no_std). */
  SDL_SetHint(SDL_HINT_VIDEODRIVER, "cluu");
  SDL_SetHint(SDL_HINT_AUDIODRIVER, "cluu");
  SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO);

  Uint32 win_flags = SDL_WINDOW_SHOWN;
  if (M_CheckParm("-fullscreen") > 0) {
      win_flags |= SDL_WINDOW_FULLSCREEN;
  }

  window = SDL_CreateWindow("DOOM",
                            SDL_WINDOWPOS_UNDEFINED,
                            SDL_WINDOWPOS_UNDEFINED,
                            DOOMGENERIC_RESX,
                            DOOMGENERIC_RESY,
                            win_flags
                            );

  /* CLUU: software renderer — no GPU on CLUU. */
  renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);
  SDL_RenderClear(renderer);
  SDL_RenderPresent(renderer);

  texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_RGB888, SDL_TEXTUREACCESS_TARGET, DOOMGENERIC_RESX, DOOMGENERIC_RESY);
}

void DG_DrawFrame()
{
  SDL_UpdateTexture(texture, NULL, DG_ScreenBuffer, DOOMGENERIC_RESX * sizeof(uint32_t));

  SDL_RenderClear(renderer);
  SDL_RenderCopy(renderer, texture, NULL, NULL);
  SDL_RenderPresent(renderer);

  handleKeyInput();
}

void DG_SleepMs(uint32_t ms)
{
  SDL_Delay(ms);
}

uint32_t DG_GetTicksMs()
{
  return SDL_GetTicks();
}

int DG_GetKey(int* pressed, unsigned char* doomKey)
{
  if (s_KeyQueueReadIndex == s_KeyQueueWriteIndex){
    return 0;
  }else{
    unsigned short keyData = s_KeyQueue[s_KeyQueueReadIndex];
    s_KeyQueueReadIndex++;
    s_KeyQueueReadIndex %= KEYQUEUE_SIZE;

    *pressed = keyData >> 8;
    *doomKey = keyData & 0xFF;

    return 1;
  }

  return 0;
}

void DG_SetWindowTitle(const char * title)
{
  if (window != NULL){
    SDL_SetWindowTitle(window, title);
  }
}

int main(int argc, char **argv)
{
    cluu_debug("doom-cluu: DG_Init starting");
    doomgeneric_Create(argc, argv);
    cluu_debug("doom-cluu: game loop starting");

    for (int i = 0; ; i++)
    {
        doomgeneric_Tick();
        DG_SleepMs(1000/35);

        if (i == 35) {
            cluu_debug("doom-cluu: 1 second of game loop completed");
        }
        if (i == 175) {
            cluu_debug("doom-cluu: 5 seconds of game loop completed");
        }
    }

    return 0;
}
