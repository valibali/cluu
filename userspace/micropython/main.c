#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
#include "py/compile.h"
#include "py/runtime.h"
#include "py/repl.h"
#include "py/gc.h"
#include "py/mperrno.h"
#include "py/stackctrl.h"
#include "py/mphal.h"
#include "py/mpthread.h"
#include "shared/runtime/pyexec.h"

#if MICROPY_VFS_POSIX
#include "extmod/vfs.h"
#include "extmod/vfs_posix.h"
#endif

// debug_print on exit gives the harness a stable success/failure marker
// (mp's stdout goes to TTY/console/framebuffer, not COM2).
extern void debug_print(const char *msg);

#define HEAP_SIZE (8 * 1024 * 1024)  // 8MB GC heap

static char heap[HEAP_SIZE];

static int setup_repl_tty_mode(struct termios *saved) {
    if (tcgetattr(STDIN_FILENO, saved) != 0) {
        return -1;
    }
    struct termios raw = *saved;
    raw.c_lflag &= (tcflag_t) ~(ICANON | ECHO);
#ifdef VMIN
    raw.c_cc[VMIN] = 1;
#endif
#ifdef VTIME
    raw.c_cc[VTIME] = 0;
#endif
    if (tcsetattr(STDIN_FILENO, TCSANOW, &raw) != 0) {
        return -1;
    }
    return 0;
}

static void restore_repl_tty_mode(const struct termios *saved) {
    (void)tcsetattr(STDIN_FILENO, TCSANOW, saved);
}

static int do_str(const char *str) {
    nlr_buf_t nlr;
    if (nlr_push(&nlr) == 0) {
        mp_lexer_t *lex = mp_lexer_new_from_str_len(MP_QSTR__lt_stdin_gt_, str, strlen(str), false);
        mp_parse_tree_t parse_tree = mp_parse(lex, MP_PARSE_FILE_INPUT);
        mp_obj_t module_fun = mp_compile(&parse_tree, MP_QSTR__lt_stdin_gt_, false);
        mp_call_function_0(module_fun);
        nlr_pop();

        mp_obj_t marker = mp_load_global(qstr_from_str("_serial_marker"));
        if (marker != MP_OBJ_NULL && mp_obj_is_str(marker)) {
            const char *s = mp_obj_str_get_str(marker);
            debug_print(s);
        }
        return 0;
    } else {
        if (mp_obj_is_exception_instance(nlr.ret_val)) {
            mp_obj_exception_t *exc = MP_OBJ_TO_PTR(nlr.ret_val);
            if (mp_obj_get_type(nlr.ret_val) == &mp_type_SystemExit) {
                if (exc->args == NULL || exc->args->len == 0) {
                    return 0;
                }
                return (int)mp_obj_get_int(exc->args->items[0]);
            }
        }
        mp_obj_print_exception(&mp_plat_print, MP_OBJ_FROM_PTR(nlr.ret_val));
        return 1;
    }
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    // Thread-local MicroPython state must exist before stack helpers run.
    // mp_stack_ctrl_init() dereferences mp_thread_get_state().
    mp_thread_init();

    // Stack limit
    mp_stack_ctrl_init();
    mp_stack_set_limit(40000);

    // GC heap
    gc_init(heap, heap + sizeof(heap));

    // Init runtime
    mp_init();

    #if MICROPY_VFS_POSIX
    {
        // Mount POSIX VFS at root
        mp_obj_t args[2] = {
            MP_OBJ_TYPE_GET_SLOT(&mp_type_vfs_posix, make_new)(&mp_type_vfs_posix, 0, 0, NULL),
            MP_OBJ_NEW_QSTR(MP_QSTR__slash_),
        };
        mp_vfs_mount(2, args, (mp_map_t *)&mp_const_empty_map);
        MP_STATE_VM(vfs_cur) = MP_STATE_VM(vfs_mount_table);
    }
    #endif

    int ret = 0;
    struct termios saved_termios;
    int tty_mode_active = 0;

    // If argv has -c "command", execute it and exit
    if (argc >= 3 && strcmp(argv[1], "-c") == 0) {
        ret = do_str(argv[2]);
    } else if (argc >= 2 && argv[1][0] != '\0') {
        // Execute script file from argv[1]
        FILE *f = fopen(argv[1], "r");
        if (f == NULL) {
            fprintf(stderr, "cannot open %s\n", argv[1]);
            ret = 1;
        } else {
            fseek(f, 0, SEEK_END);
            long fsize = ftell(f);
            fseek(f, 0, SEEK_SET);
            char *src = malloc(fsize + 1);
            if (src != NULL) {
                size_t nread = fread(src, 1, fsize, f);
                src[nread] = '\0';
                ret = do_str(src);
                free(src);
            } else {
                fprintf(stderr, "out of memory\n");
                ret = 1;
            }
            fclose(f);
        }
    } else {
        // Friendly/raw REPL expects byte-at-a-time, no local echo.
        if (setup_repl_tty_mode(&saved_termios) == 0) {
            tty_mode_active = 1;
        }
        // Interactive REPL
        for (;;) {
            if (pyexec_mode_kind == PYEXEC_MODE_RAW_REPL) {
                if (pyexec_raw_repl() != 0) break;
            } else {
                if (pyexec_friendly_repl() != 0) break;
            }
        }
    }

    if (tty_mode_active) {
        restore_repl_tty_mode(&saved_termios);
    }

    mp_thread_deinit();
    mp_deinit();

    {
        char buf[64];
        if (ret == 0) {
            debug_print("micropython: exit 0");
        } else {
            snprintf(buf, sizeof buf, "micropython: exit %d", ret);
            debug_print(buf);
        }
    }

    return ret;
}

// Required stubs

void nlr_jump_fail(void *val) {
    fprintf(stderr, "FATAL: uncaught NLR %p\n", val);
    _exit(1);
}

#ifndef NDEBUG
void __assert_func(const char *file, int line, const char *func, const char *expr) {
    fprintf(stderr, "Assertion '%s' failed, at %s:%d (%s)\n", expr, file, line, func);
    _exit(1);
}
#endif
