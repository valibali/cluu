/*
 * mkbootimg/tar.c
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
 * @brief POSIX ustar file system driver
 * See https://en.wikipedia.org/wiki/Tarball_(computing)#UStar_format
 *
 */
#include "main.h"

void tar_open(gpt_t *gpt_entry)
{
    if(gpt_entry && (gpt_entry->last - gpt_entry->start) < 1) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]);
        exit(1);
    }
}

void tar_add(struct stat *st, char *name, unsigned char *content, int size)
{
    unsigned char *end;
    int i, j = 0;
    if(!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode) && !S_ISLNK(st->st_mode)) return;
    fs_base = realloc(fs_base, fs_len + 512 + ((size + 511) & ~511));
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    end = fs_base + fs_len;
    memset(end, 0, 512);
    strncpy((char*)end, name, 99);
    sprintf((char*)end + 100, "%07o", st->st_mode & 077777);
    sprintf((char*)end + 108, "%07o", 0);
    sprintf((char*)end + 116, "%07o", 0);
    sprintf((char*)end + 124, "%011o", size);
    sprintf((char*)end + 136, "%011o", 0);
    sprintf((char*)end + 148, "%06o", 0);
    sprintf((char*)end + 155, " %1d", S_ISDIR(st->st_mode) ? 5 : (S_ISLNK(st->st_mode) ? 2 : 0));
    if(S_ISLNK(st->st_mode)) { strncpy((char*)end + 157, (char*)content, 99); size = 0; }
    memcpy(end + 257, "ustar  ", 7);
    memcpy(end + 265, "root", 4); memcpy(end + 297, "root", 4);
    for(i = 0; i < 512; i++) j += end[i];
    for(i = 0; i < 8; i++) j += ' ' - end[148 + i];
    sprintf((char*)end + 148, "%06o", j);
    end += 512;
    if(content && size) { memcpy(end, content, size); memset(end + size, 0, ((size + 511) & ~511) - size); }
    fs_len += 512 + ((size + 511) & ~511);
}

void tar_close()
{
}
