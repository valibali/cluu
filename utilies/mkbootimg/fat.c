/*
 * mkbootimg/fat.c
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
 * @brief normal (non-ESP) FAT16/32 file system driver with long filename support
 * See https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/vfat.pdf
 *
 */
#include "main.h"

#define SECTOR_PER_CLUSTER 1

struct tm *fat_ts;
int fat_nextcluster, fat_bpc, fat_spf, fat_lfncnt, fat_numclu;
unsigned char *fat_rootdir, *fat_data, fat_lfn[769];
uint16_t *fat_fat16_1, *fat_fat16_2;
uint32_t *fat_fat32_1, *fat_fat32_2;

unsigned char *fat_newclu(int parent)
{
    int clu;
    if(fat_fat16_1) {
        while(parent != fat_nextcluster && fat_fat16_1[parent] && fat_fat16_1[parent] != 0xFFFF)
            parent = fat_fat16_1[parent];
        fat_fat16_1[parent] = fat_fat16_2[parent] = fat_nextcluster;
        fat_fat16_1[fat_nextcluster] = fat_fat16_2[fat_nextcluster] = 0xFFFF;
    } else {
        while(parent != fat_nextcluster && fat_fat32_1[parent] && fat_fat32_1[parent] != 0xFFFFFFF)
            parent = fat_fat32_1[parent];
        fat_fat32_1[parent] = fat_fat32_2[parent] = fat_nextcluster;
        fat_fat32_1[fat_nextcluster] = fat_fat32_2[fat_nextcluster] = 0xFFFFFFF;
    }
    clu = fat_nextcluster++;
    if(fat_nextcluster >= fat_numclu) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOBIG]); exit(1); }
    return fat_data + clu * fat_bpc;
}

unsigned char *fat_readlfn(unsigned char *dir, int *clu, int *size, int parent)
{
    uint16_t uc2[256], *u;
    unsigned char *s, *d;
    int i = 0, n;
    memset(fat_lfn, 0, sizeof(fat_lfn));
    if(!dir[0]) return dir;
    while(dir[0] == '.') dir += 32;
    fat_lfncnt++;
    if(parent != 2 && !((uint64_t)(dir - fs_base) & (fat_bpc - 1))) {
        if(fat_fat16_1) {
            parent = fat_fat16_1[parent];
            if(!parent || parent == 0xFFFF) return NULL;
        } else {
            parent = fat_fat32_1[parent];
            if(!parent || parent == 0xFFFFFFF) return NULL;
        }
        dir = fat_data + parent * fat_bpc;
    }
    if(dir[0xB] != 0xF) {
        for(s = dir, d = fat_lfn, i = 0; *s && *s != ' ' && i < 8; i++)
            *d++ = *s++;
        if(dir[8] && dir[8] != ' ') {
            *d++ = '.';
            for(s = dir + 8; *s != ' ' && i < 3; i++)
                *d++ = *s++;
        }
    } else {
        memset(uc2, 0, sizeof(uc2));
        n = dir[0] & 0x3F;
        u = uc2 + (n - 1) * 13;
        while(n--) {
            for(i = 0; i < 5; i++)
                u[i] = dir[i*2+2] << 8 | dir[i*2+1];
            for(i = 0; i < 6; i++)
                u[i+5] = dir[i*2+0xF] << 8 | dir[i*2+0xE];
            u[11] = dir[0x1D] << 8 | dir[0x1C];
            u[12] = dir[0x1F] << 8 | dir[0x1E];
            u -= 13;
            dir += 32;
            if(!((uint64_t)(dir - fs_base) & (fat_bpc - 1))) {
                if(fat_fat16_1) {
                    parent = fat_fat16_1[parent];
                    if(!parent || parent == 0xFFFF) return NULL;
                } else {
                    parent = fat_fat32_1[parent];
                    if(!parent || parent == 0xFFFFFFF) return NULL;
                }
                dir = fat_data + parent * fat_bpc;
            }
        }
        for(d = fat_lfn, u = uc2; *u; u++)
            if(*u < 0x80) {
                *d++ = *u;
            } else if(*u < 0x800) {
                *d++ = ((*u>>6)&0x1F)|0xC0;
                *d++ = (*u&0x3F)|0x80;
            } else {
                *d++ = ((*u>>12)&0x0F)|0xE0;
                *d++ = ((*u>>6)&0x3F)|0x80;
                *d++ = (*u&0x3F)|0x80;
            }
    }
    *clu = (dir[0x15] << 24) | (dir[0x14] << 16) | (dir[0x1B] << 8) | dir[0x1A];
    *size = (dir[0x1F] << 24) | (dir[0x1E] << 16) | (dir[0x1D] << 8) | dir[0x1C];
    return dir + 32;
}

unsigned char *fat_writelfn(unsigned char *dir, char *name, int type, int size, int parent, int clu)
{
    uint16_t uc2[256], *u;
    unsigned char *s, c = 0, sfn[12];
    int i, n;
    if(name[0] == '.') {
        memset(dir, ' ', 11);
        memcpy(dir, name, strlen(name));
    } else {
        memset(uc2, 0, sizeof(uc2));
        for(n = 0, u = uc2, s = (unsigned char*)name; *s; n++, u++) {
            if((*s & 128) != 0) {
                if((*s & 32) == 0) { *u = ((*s & 0x1F)<<6)|(*(s+1) & 0x3F); s += 2; } else
                if((*s & 16) == 0) { *u = ((*s & 0xF)<<12)|((*(s+1) & 0x3F)<<6)|(*(s+2) & 0x3F); s += 3; }
                else { fprintf(stderr,"mkbootimg: partition #%d %s '%s'\r\n", fs_no, lang[ERR_WRITE], name); exit(1); }
            } else
                *u = *s++;
        }
        /* don't convert "Microsoft" to "MICROS~1   ", that's patented... */
        sprintf((char*)sfn, "~%07xLFN", fat_lfncnt++);
        for(i = 0; i < 11; i++)
            c = (((c & 1) << 7) | ((c & 0xfe) >> 1)) + sfn[i];
        n = (n + 12) / 13;
        u = uc2 + (n - 1) * 13;
        i = 0x40;
        while(n--) {
            if(parent > 2 && !((uint64_t)(dir - fs_base) & (fat_bpc - 1)))
                dir = fat_newclu(parent);
            dir[0] = i | (n + 1);
            dir[11] = 0xF;
            dir[0xD] = c;
            memcpy(dir + 1, (unsigned char*)u, 10);
            memcpy(dir + 14, (unsigned char*)u + 10, 12);
            memcpy(dir + 28, (unsigned char*)u + 22, 4);
            i = 0;
            u -= 13;
            dir += 32;
        }
        if(parent > 2 && !((uint64_t)(dir - fs_base) & (fat_bpc - 1)))
            dir = fat_newclu(parent);
        memcpy(dir, sfn, 11);
    }
    if(type) {
        dir[0xB] = 0x10;
    } else {
        dir[0x1C] = size & 0xFF; dir[0x1D] = (size >> 8) & 0xFF;
        dir[0x1E] = (size >> 16) & 0xFF; dir[0x1F] = (size >> 24) & 0xFF;
    }
    if(!clu) clu = fat_nextcluster;
    if(clu < 3) clu = 0;
    dir[0x1A] = clu & 0xFF; dir[0x1B] = (clu >> 8) & 0xFF;
    dir[0x14] = (clu >> 16) & 0xFF; dir[0x15] = (clu >> 24) & 0xFF;
    i = (fat_ts->tm_hour << 11) | (fat_ts->tm_min << 5) | (fat_ts->tm_sec/2);
    dir[0xE] = dir[0x16] = i & 0xFF; dir[0xF] = dir[0x17] = (i >> 8) & 0xFF;
    i = ((fat_ts->tm_year+1900-1980) << 9) | ((fat_ts->tm_mon+1) << 5) | (fat_ts->tm_mday);
    return dir + 32;
}

/*** mkbootimg interface ***/
void fat_open(gpt_t *gpt_entry)
{
    int i;
    if(!gpt_entry) { fprintf(stderr,"mkbootimg: %s fat.\r\n", lang[ERR_BADINITRDTYPE]); exit(1); }
    fat_numclu = (gpt_entry->last - gpt_entry->start + 1) / SECTOR_PER_CLUSTER;
    if(fat_numclu < 4085) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]); exit(1); }
    /* "format" the partition to either FAT16 or FAT32 */
    fs_len = fat_numclu * 512 * SECTOR_PER_CLUSTER;
    fs_base = realloc(fs_base, fs_len);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(fs_base, 0, fs_len);
    memcpy(fs_base + 3, "MSWIN4.1", 8);
    fs_base[0xC] = 2; fs_base[0x10] = 2; fs_base[0x15] = 0xF8; fs_base[0x1FE] = 0x55; fs_base[0x1FF] = 0xAA;
    fs_base[0x18] = 0x20; fs_base[0x1A] = 0x40;
    memcpy(fs_base + 0x1C, &gpt_entry->start, 4);
    if(fat_numclu > 65535)
        memcpy(fs_base + 0x20, &fat_numclu, 4);
    else
        memcpy(fs_base + 0x13, &fat_numclu, 2);
    if(fat_numclu < 65525) {
        /* FAT16 */
        fat_spf = (fat_numclu*2 + 511) / 512;
        fs_base[0xD] = SECTOR_PER_CLUSTER; fs_base[0xE] = 4; fs_base[0x12] = 2;
        fs_base[0x16] = fat_spf & 0xFF; fs_base[0x17] = (fat_spf >> 8) & 0xFF;
        fs_base[0x24] = 0x80; fs_base[0x26] = 0x29;
        memcpy(fs_base + 0x27, &gpt_entry->guid, 4);
        memcpy(fs_base + 0x2B, "NO NAME    FAT16   ", 19);
        fat_bpc = fs_base[0xD] * 512;
        fat_rootdir = fs_base + (fat_spf*fs_base[0x10]+fs_base[0xE]) * 512;
        fat_data = fat_rootdir + ((((fs_base[0x12]<<8)|fs_base[0x11])*32 - 2*fat_bpc) & ~(fat_bpc-1));
        fat_fat16_1 = (uint16_t*)(&fs_base[fs_base[0xE] * 512]);
        fat_fat16_2 = (uint16_t*)(&fs_base[(fs_base[0xE]+fat_spf) * 512]);
        fat_fat16_1[0] = fat_fat16_2[0] = 0xFFF8; fat_fat16_1[1] = fat_fat16_2[1] = 0xFFFF;
        fat_fat32_1 = fat_fat32_2 = NULL;
    } else {
        /* FAT32 */
        fat_spf = (fat_numclu*4) / 512 - 8;
        fs_base[0xD] = SECTOR_PER_CLUSTER; fs_base[0xE] = 8;
        fs_base[0x24] = fat_spf & 0xFF; fs_base[0x25] = (fat_spf >> 8) & 0xFF;
        fs_base[0x26] = (fat_spf >> 16) & 0xFF; fs_base[0x27] = (fat_spf >> 24) & 0xFF;
        fs_base[0x2C] = 2; fs_base[0x30] = 1; fs_base[0x32] = 6; fs_base[0x40] = 0x80; fs_base[0x42] = 0x29;
        memcpy(fs_base + 0x43, &gpt_entry->guid, 4);
        memcpy(fs_base + 0x47, "NO NAME    FAT32   ", 19);
        memcpy(fs_base + 0x200, "RRaA", 4); memcpy(fs_base + 0x3E4, "rrAa", 4);
        for(i = 0; i < 8; i++) fs_base[0x3E8 + i] = 0xFF;
        fs_base[0x3FE] = 0x55; fs_base[0x3FF] = 0xAA;
        fat_bpc = fs_base[0xD] * 512;
        fat_rootdir = fs_base + (fat_spf*fs_base[0x10]+fs_base[0xE]) * 512;
        fat_data = fat_rootdir - 2*fat_bpc;
        fat_fat32_1 = (uint32_t*)(&fs_base[fs_base[0xE] * 512]);
        fat_fat32_2 = (uint32_t*)(&fs_base[(fs_base[0xE]+fat_spf) * 512]);
        fat_fat32_1[0] = fat_fat32_2[0] = fat_fat32_1[2] = fat_fat32_2[2] = 0x0FFFFFF8;
        fat_fat32_1[1] = fat_fat32_2[1] = 0x0FFFFFFF;
        fat_fat16_1 = fat_fat16_2 = NULL;
    }
    fat_nextcluster = 3;
}

void fat_add(struct stat *st, char *name, unsigned char *content, int size)
{
    int parent = 2, clu, i;
    unsigned char *dir = fat_rootdir;
    char *end, *fn = strrchr(name, '/');
    if(!fn) fn = name; else fn++;
    if(!strcmp(fn, ".") || !strcmp(fn, "..")) return;
    if(!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode)) return;
    fat_ts = gmtime(&st->st_mtime);
    fn = name;
    end = strchr(name, '/');
    if(!end) end = name + strlen(name);
    fat_lfncnt = 1;
    do {
        dir = fat_readlfn(dir, &clu, &size, parent);
        if(!dir) return;
        if(!memcmp(fat_lfn, fn, end - fn) && !fat_lfn[end - fn]) {
            fat_lfncnt = 1;
            parent = clu;
            dir = fat_data + parent * fat_bpc + 64;
            fn = end + 1;
            end = *end ? strchr(fn, '/') : NULL;
            if(!end) { end = fn + strlen(fn); break; }
        }
    } while(dir[0]);
    dir = fat_writelfn(dir, fn, S_ISDIR(st->st_mode), size, parent, 0);
    if(S_ISDIR(st->st_mode)) {
        dir = fat_newclu(fat_nextcluster);
        dir = fat_writelfn(dir, ".", 1, 0, 2, fat_nextcluster - 1);
        dir = fat_writelfn(dir, "..", 1, 0, 2, parent);
    } else if(content && size > 0) {
        if(fat_nextcluster * fat_bpc + size >= fs_len) {
            fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOBIG]);
            exit(1);
        }
        memcpy(fat_data + fat_nextcluster * fat_bpc, content, size);
        for(i = 0; i < ((size + fat_bpc-1) & ~(fat_bpc-1)); i += fat_bpc, fat_nextcluster++) {
            if(fat_fat16_1) fat_fat16_1[fat_nextcluster] = fat_fat16_2[fat_nextcluster] = fat_nextcluster+1;
            else fat_fat32_1[fat_nextcluster] = fat_fat32_2[fat_nextcluster] = fat_nextcluster+1;
        }
        if(fat_fat16_1) fat_fat16_1[fat_nextcluster-1] = fat_fat16_2[fat_nextcluster-1] = 0xFFFF;
        else fat_fat32_1[fat_nextcluster-1] = fat_fat32_2[fat_nextcluster-1] = 0xFFFFFFF;
    }
}

void fat_close()
{
    int i;
    if(!fs_base || fs_len < 512) return;
    if(fat_fat32_1) {
        fat_nextcluster -= 2;
        i = ((fs_len - (fat_spf*fs_base[0x10]+fs_base[0xE]) * 512)/fat_bpc) - fat_nextcluster;
        fs_base[0x3E8] = i & 0xFF; fs_base[0x3E9] = (i >> 8) & 0xFF;
        fs_base[0x3EA] = (i >> 16) & 0xFF; fs_base[0x3EB] = (i >> 24) & 0xFF;
        fs_base[0x3EC] = fat_nextcluster & 0xFF; fs_base[0x3ED] = (fat_nextcluster >> 8) & 0xFF;
        fs_base[0x3EE] = (fat_nextcluster >> 16) & 0xFF; fs_base[0x3EF] = (fat_nextcluster >> 24) & 0xFF;
        /* copy backup boot sectors */
        memcpy(fs_base + (fs_base[0x32]*512), fs_base, 1024);
    }
}
