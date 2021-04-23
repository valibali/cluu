/*
 * mkbootimg/ech.c
 *
 * Copyright (C) 2017 - 2021 bzt (bztsrc@gitlab)
 *
 * Permission is hereby granted, free of charge, to any person
 * obtaining a copy of this software and associated documentation
 * files (the "Software"), to deal in the Software without
 * restriction, including without limitation the rights to use, copy,
 * modify, merge, publish, distribute, sublicense, and/or sell copies
 * of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
 * NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
 * HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
 * WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 *
 * This file is part of the BOOTBOOT Protocol package.
 * @brief a very minimal echfs driver which is much easier to use than echfs-utils
 * See https://github.com/echfs/echfs
 *
 */
#include "main.h"


typedef struct {
    uint64_t parent_id;
    uint8_t type;
    char name[201];
    uint64_t atime;
    uint64_t mtime;
    uint16_t perms;
    uint16_t owner;
    uint16_t group;
    uint64_t ctime;
    uint64_t payload;
    uint64_t size;
}__attribute__((packed)) ech_entry_t;
ech_entry_t *ech_ents = NULL;
int ech_numents, ech_maxents;

uint8_t *ech_data = NULL, ech_uuid[16];
uint64_t ech_size;
uint64_t ech_numblk;

/*** mkbootimg interface ***/
void ech_open(gpt_t *gpt_entry)
{
    if(gpt_entry) {
        if((gpt_entry->last - gpt_entry->start) < 1) {
            fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]);
            exit(1);
        }
        memcpy(ech_uuid, &gpt_entry->guid, 16);
        ech_numblk = gpt_entry->last - gpt_entry->start + 1;
        ech_maxents = (ech_numblk * 5 / 100) * 512 / sizeof(ech_entry_t);
    } else {
        memcpy(ech_uuid, "INITRD", 6);
        memset(ech_uuid + 6, 0, 10);
        ech_numblk = 0;
        ech_maxents = 0;
    }
    ech_numents = 0;
    ech_size = 0;
}

void ech_add(struct stat *st, char *name, unsigned char *content, int size)
{
    uint64_t parent = UINT64_C(0xffffffffffffffff);
    int i, j;
    char *end, *fn = strrchr(name, '/');
    if(!fn) fn = name; else fn++;
    if(!strcmp(fn, ".") || !strcmp(fn, "..")) return;
    if(!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode)) return;

    fn = name;
    end = strchr(name, '/');
    if(!end) end = name + strlen(name);
    for(i = 0; i < ech_numents; i++) {
        if(ech_ents[i].parent_id == parent && !memcmp(ech_ents[i].name, fn, end - fn) && !ech_ents[i].name[end - fn]) {
            parent = ech_ents[i].payload;
            fn = end + 1;
            end = *end ? strchr(fn, '/') : NULL;
            if(!end) { end = fn + strlen(fn); break; }
        }
    }
    if(ech_numblk && ech_numblk * 512 < ech_size + size) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOBIG]);
        exit(1);
    }
    if(ech_maxents && ech_numents + 1 >= ech_maxents) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOMANY]);
        exit(1);
    }
    ech_ents = (ech_entry_t*)realloc(ech_ents, (ech_numents + 1) * sizeof(ech_entry_t));
    if(!ech_ents) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(&ech_ents[ech_numents], 0, sizeof(ech_entry_t));
    ech_ents[ech_numents].parent_id = parent;
    memcpy(ech_ents[ech_numents].name, fn, end - fn);
    ech_ents[ech_numents].atime = st->st_atime;
    ech_ents[ech_numents].mtime = st->st_mtime;
    ech_ents[ech_numents].ctime = st->st_ctime;
    ech_ents[ech_numents].perms = st->st_mode & 0xFFF;
    if(S_ISDIR(st->st_mode)) {
        ech_ents[ech_numents].type = 1;
        ech_ents[ech_numents].payload = ech_numents + 1;
    } else {
        ech_ents[ech_numents].size = size;
        ech_ents[ech_numents].payload = ech_size / 512;
        j = (size + 511) & ~511;
        if(j > 0) {
            ech_data = (uint8_t*)realloc(ech_data, ech_size + j);
            if(!ech_ents) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
            memcpy(ech_data + ech_size, content, size);
            ech_size += j;
        }
    }
    ech_numents++;
}

void ech_close()
{
    ech_entry_t *ent;
    uint64_t offs, *ptr, i, j;
    if(!ech_numblk) {
        ech_maxents = ech_numents;
        ech_numblk = 16 + (ech_numents * sizeof(ech_entry_t) + 511 + ech_size) / 512;
        ech_numblk += (ech_numblk * 8 + 511) / 512;
    }
    offs = 16 + ((ech_maxents * sizeof(ech_entry_t) + 511) / 512) + (ech_numblk * 8 + 511) / 512;
    fs_len = ech_numblk*512;
    fs_base = realloc(fs_base, fs_len);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(fs_base, 0, fs_len);
    /* superblock */
    memcpy(fs_base + 4, "_ECH_FS_", 8);
    memcpy(fs_base + 12, &ech_numblk, 8);
    j = (ech_maxents * sizeof(ech_entry_t) + 511) / 512;
    memcpy(fs_base + 20, &j, 8);
    j = 512;
    memcpy(fs_base + 28, &j, 8);
    memcpy(fs_base + 40, &ech_uuid, 16);
    /* allocation table */
    for(i = 0, ptr = (uint64_t*)(fs_base + 16 * 512); i < offs; i++, ptr++)
        *ptr = UINT64_C(0xfffffffffffffff0);
    /* directory entries */
    ent = (ech_entry_t*)(fs_base + (16 + (ech_numblk * 8 + 511) / 512) * 512);
    for(i = 0; (int)i < ech_numents; i++, ent++) {
        memcpy(ent, &ech_ents[i], sizeof(ech_entry_t));
        if(!ent->type) {
            if(!ent->size)
                ent->payload = UINT64_C(0xffffffffffffffff);
            else {
                ent->payload += offs;
                j = ent->payload + 1;
                while(ech_ents[i].size > 512) {
                    ech_ents[i].size -= 512;
                    *ptr++ = j++;
                }
                *ptr++ = UINT64_C(0xffffffffffffffff);
            }
        }
    }
    /* file data */
    if(ech_data && ech_size)
        memcpy(fs_base + offs * 512, ech_data, ech_size);
    /* free resources */
    if(ech_ents) { free(ech_ents); ech_ents = NULL; }
    ech_numents = 0;
    ech_maxents = 0;
    if(ech_data) { free(ech_data); ech_data = NULL; }
    ech_size = 0;
    ech_numblk = 0;
}
