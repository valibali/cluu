// CLUU WAD file backend — grant-based bulk read.
//
// Replaces w_file_stdc.c. Instead of stdio fread (1KB IPC copies),
// calls into the Rust doom-cluu staticlib which does a single
// read_file_bulk grant: VFS maps the entire WAD into our address
// space in one zero-copy operation. All subsequent reads are memcpy.

#include <stdlib.h>
#include <string.h>

#include "w_file.h"
#include "z_zone.h"

// Provided by doom-cluu Rust staticlib.
// Opens `path` via VFS, bulk-reads the entire file into memory via
// page grant, and returns a pointer to the mapped data. Writes the
// file length to *out_len. Returns NULL on failure.
extern void *cluu_wad_load(const char *path, unsigned long *out_len);

typedef struct
{
    wad_file_t wad;
    unsigned char *data;
    unsigned long length;
} cluu_wad_file_t;

extern wad_file_class_t cluu_wad_file;

static wad_file_t *W_Cluu_OpenFile(char *path)
{
    unsigned long length = 0;
    void *data = cluu_wad_load(path, &length);
    if (data == NULL) {
        return NULL;
    }

    cluu_wad_file_t *result =
        (cluu_wad_file_t *) Z_Malloc(sizeof(cluu_wad_file_t), PU_STATIC, 0);
    result->wad.file_class = &cluu_wad_file;
    result->wad.mapped = data;
    result->wad.length = length;
    result->data = (unsigned char *) data;
    result->length = length;

    return &result->wad;
}

static void W_Cluu_CloseFile(wad_file_t *wad)
{
    cluu_wad_file_t *cluu_wad = (cluu_wad_file_t *) wad;
    // Data stays mapped; zone allocator owns lump copies. The mapping
    // is freed when the process exits. No explicit unmap needed.
    Z_Free(cluu_wad);
}

static size_t W_Cluu_Read(wad_file_t *wad, unsigned int offset,
                          void *buffer, size_t buffer_len)
{
    cluu_wad_file_t *cluu_wad = (cluu_wad_file_t *) wad;

    if (offset >= cluu_wad->length) {
        return 0;
    }

    size_t avail = cluu_wad->length - offset;
    size_t to_read = buffer_len < avail ? buffer_len : avail;

    memcpy(buffer, cluu_wad->data + offset, to_read);

    return to_read;
}

wad_file_class_t cluu_wad_file =
{
    W_Cluu_OpenFile,
    W_Cluu_CloseFile,
    W_Cluu_Read,
};
