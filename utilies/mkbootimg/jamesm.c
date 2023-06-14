/*
 * mkbootimg/jamesm.c
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
 * @brief Driver for James Molloy's initrd
 * See http://jamesmolloy.co.uk/tutorial_html/8.-The%20VFS%20and%20the%20initrd.html
 *
 */
#include "main.h"

void jamesm_open(gpt_t *gpt_entry)
{
    if(gpt_entry) { fprintf(stderr,"mkbootimg: partition #%d %s jamesm.\r\n", fs_no, lang[ERR_INITRDTYPE]); exit(1); }
    fs_len = 4 + 64 * 73;
    fs_base = realloc(fs_base, fs_len);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    memset(fs_base, 0, fs_len);
}

void jamesm_add(struct stat *st, char *name, unsigned char *content, int size)
{
    unsigned char *end;
    if(!S_ISREG(st->st_mode) || !content || !size) return;
    fs_base = realloc(fs_base, fs_len + size);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    /* this format is specified to hold maximum 64 files... */
    if(fs_base[0] > 63) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_TOOMANY]); exit(1); }
    end = fs_base + fs_len;
    memcpy(end, content, size);
    fs_base[4 + fs_base[0]*73] = 0xBF;
    strncpy((char*)&fs_base[5 + fs_base[0]*73], name, 63);
    memcpy(&fs_base[69 + fs_base[0]*73], &fs_len, 4);
    memcpy(&fs_base[73 + fs_base[0]*73], &size, 4);
    fs_base[0]++;
    fs_len += size;
}

void jamesm_close()
{
}
