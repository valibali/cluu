/*
 * mkbootimg/cpio.c
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
 * @brief CPIO initrd driver
 * See https://en.wikipedia.org/wiki/Cpio
 *
 */
#include "main.h"

void cpio_open(gpt_t *gpt_entry)
{
    if(gpt_entry) { fprintf(stderr,"mkbootimg: partition #%d %s cpio.\r\n", fs_no, lang[ERR_INITRDTYPE]); exit(1); }
}

void cpio_add(struct stat *st, char *name, unsigned char *content, int size)
{
    unsigned char *end;
    int n = strlen(name);
    if(!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode) && !S_ISLNK(st->st_mode)) return;
    fs_base = realloc(fs_base, fs_len + 76 + n + 1 + size);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    end = fs_base + fs_len;
    end += sprintf((char*)end, "070707000000000000%06o00000000000000000000000000000000000%06o%011o%s",
        st->st_mode & 0777777,n+1,size,name);
    *end++ = 0;
    if(content && size) memcpy(end, content, size);
    fs_len += 76 + n + 1 + size;
}

void cpio_close()
{
    char *end;
    fs_base = realloc(fs_base, fs_len + 76 + 11 + 512);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    end = (char*)fs_base + fs_len;
    memset(end, 0, 76 + 11 + 512);
    end += sprintf(end, "07070700000000000000000000000000000000000010000000000000000%06o%011oTRAILER!!!",11,0);
    fs_len = ((fs_len + 88 + 511) & ~511);
}
