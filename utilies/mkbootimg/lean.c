/*
 * mkbootimg/lean.c
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
 * @brief LeanFS file system driver
 * See http://freedos-32.sourceforge.net/lean/specification.php
 * See http://www.fysnet.net/leanfs/specification.php
 *
 */
#include "main.h"

#define LEAN_SUPER_MAGIC        0x4E41454C
#define LEAN_SUPER_VERSION      0x0007      /* could be 0x0006 as well, backwards compatible */
#define LEAN_INODE_MAGIC        0x45444F4E
#define LEAN_INODE_EXTENT_CNT   6
#define LEAN_FT_MT              0
#define LEAN_FT_REG             1
#define LEAN_FT_DIR             2
#define LEAN_FT_LNK             3
#define LEAN_ATTR_PREALLOC      (1 << 18)
#define LEAN_ATTR_INLINEXTATTR  (1 << 19)
#define LEAN_ATTR_IFMT          (7 << 29)
#define LEAN_ATTR_IFTYPE(x)     ((uint32_t)(x) << 29)
#define LEAN_LOG_BANDSIZE       12
#define LEAN_BITMAPSIZE         (1 << (LEAN_LOG_BANDSIZE - 12))
#define LEAN_INODE_SIZE         176

typedef struct {
  uint8_t  loader[16384];
  uint32_t checksum;
  uint32_t magic;
  uint16_t fs_version;
  uint8_t  pre_alloc_count;
  uint8_t  log_sectors_per_band;
  uint32_t state;
  uint8_t  uuid[16];
  uint8_t  volume_label[64];
  uint64_t sector_count;
  uint64_t free_sector_count;
  uint64_t primary_super;
  uint64_t backup_super;
  uint64_t bitmap_start;
  uint64_t root_inode;
  uint64_t bad_inode;
  uint64_t journal_inode;
  uint8_t  log_block_size;
  uint8_t  reserved2[344];
} __attribute__((packed)) lean_super_t;

typedef struct {
  uint32_t checksum;
  uint32_t magic;
  uint8_t  extent_count;
  uint8_t  reserved[3];
  uint32_t indirect_count;
  uint32_t links_count;
  uint32_t uid;
  uint32_t gid;
  uint32_t attributes;
  uint64_t file_size;
  uint64_t sector_count;
  uint64_t atime;
  uint64_t ctime;
  uint64_t mtime;
  uint64_t btime;
  uint64_t first_indirect;
  uint64_t last_indirect;
  uint64_t fork;
  uint64_t extent_start[LEAN_INODE_EXTENT_CNT];
  uint32_t extent_size[LEAN_INODE_EXTENT_CNT];
} __attribute__((packed)) lean_inode_t;

typedef struct {
  uint64_t inode;
  uint8_t  type;
  uint8_t  rec_len;
  uint16_t name_len;
} __attribute__((packed)) lean_dirent_t;

int len_numblk, len_nextblk;
lean_super_t *len_sb;

uint32_t len_checksum(void *data, int size)
{
  uint32_t ret = 0, *ptr = (uint32_t*)data;
  int i;
  for(i = 1; i < size; i++)
    ret = (ret << 31) + (ret >> 1) + ptr[i];
  return ret;
}

int len_alloc_blk()
{
    int r, g = len_nextblk / (1 << LEAN_LOG_BANDSIZE), o = len_nextblk % (1 << LEAN_LOG_BANDSIZE);
    while((uint64_t)len_nextblk < len_sb->sector_count &&
        fs_base[(g * (1 << LEAN_LOG_BANDSIZE) + (!g ? len_sb->bitmap_start : 0)) * 512 + o / 8] & (1 << (o & 7))) {
            o++; len_nextblk++;
            if(o >= (1 << LEAN_LOG_BANDSIZE)) { o = 0; g++; len_nextblk += LEAN_BITMAPSIZE; }
    }
    if((uint64_t)len_nextblk + 1 >= len_sb->sector_count || len_sb->free_sector_count < 1) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOBIG]);
        exit(1);
    }
    fs_base[(g * (1 << LEAN_LOG_BANDSIZE) + (!g ? len_sb->bitmap_start : 0)) * 512 + o / 8] |= 1 << (o & 7);
    len_sb->free_sector_count--;
    r = len_nextblk++;
    if(!(len_nextblk % (1 << LEAN_LOG_BANDSIZE))) len_nextblk += LEAN_BITMAPSIZE;
    return r;
}

void len_add_to_inode(uint32_t ino, uint32_t blk, char *name)
{
    lean_inode_t *inode = (lean_inode_t*)(fs_base + ino * 512);
    inode->sector_count++;
    if(inode->extent_start[inode->extent_count - 1] + inode->extent_size[inode->extent_count - 1] == blk) {
        inode->extent_size[inode->extent_count - 1]++;
    } else {
        inode->extent_count++;
        if(inode->extent_count < LEAN_INODE_EXTENT_CNT) {
            inode->extent_start[inode->extent_count - 1] = blk;
            inode->extent_size[inode->extent_count - 1] = 1;
        } else {
            fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
            exit(1);
        }
    }
    inode->checksum = len_checksum(inode, LEAN_INODE_SIZE / 4);
}

int len_alloc_inode(uint16_t mode, uint8_t type, uint64_t size, time_t t)
{
    lean_inode_t *inode;
    int n = len_alloc_blk(), i;
    inode = (lean_inode_t*)(fs_base + n * 512);
    inode->magic = LEAN_INODE_MAGIC;
    inode->attributes = (mode & 0xFFF) | LEAN_ATTR_IFTYPE(type) | LEAN_ATTR_INLINEXTATTR |
        (type == LEAN_FT_DIR ? LEAN_ATTR_PREALLOC : 0);
    inode->atime = inode->ctime = inode->mtime = inode->btime = (uint64_t)t * 1000000;
    inode->extent_count = 1;
    inode->extent_start[0] = n;
    inode->extent_size[0] = 1;
    inode->sector_count = 1;
    if(type == LEAN_FT_DIR)
        for(i = 0; i < len_sb->pre_alloc_count; i++)
            len_add_to_inode(n, len_alloc_blk(), NULL);
    else
        inode->file_size = size;
    inode->checksum = len_checksum(inode, LEAN_INODE_SIZE / 4);
    return n;
}

uint8_t *len_add_dirent(uint8_t *dir, uint64_t toinode, uint64_t ino, uint8_t type, char *name, int len)
{
    lean_dirent_t *de;
    lean_inode_t *inode;
    uint8_t *end = NULL;
    int l = 16 + (len < 4 ? 0 : ((len + 11) & ~15)), i = 0;
    uint64_t j = 0;
    inode = (lean_inode_t*)(fs_base + ino * 512);
    inode->links_count++;
    inode->checksum = len_checksum(inode, LEAN_INODE_SIZE / 4);
    inode = (lean_inode_t*)(fs_base + toinode * 512);
    if(!dir) {
        if(!inode->extent_count || inode->extent_size[0] == 1)
            len_add_to_inode(toinode, len_alloc_blk(), NULL);
        dir = fs_base + inode->extent_start[0] * 512 + 512;
        end = fs_base + (inode->extent_start[0] + inode->extent_size[0]) * 512;
        while(j < inode->file_size && ((lean_dirent_t*)dir)->inode && ((lean_dirent_t*)dir)->rec_len) {
            j += ((lean_dirent_t*)dir)->rec_len * 16;
            dir += ((lean_dirent_t*)dir)->rec_len * 16;
            if(dir >= end) {
                dir = fs_base + inode->extent_start[++i] * 512 + ((uintptr_t)(dir - end) & 511);
                end = fs_base + (inode->extent_start[i] + inode->extent_size[i]) * 512;
            }
        }
    }
    inode->file_size += l;
    if(inode->file_size > inode->sector_count * 512) {
        fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOMANY], name);
        exit(1);
    }
    inode->checksum = len_checksum(inode, LEAN_INODE_SIZE / 4);
    de = (lean_dirent_t*)dir;
    de->inode = ino;
    de->type = type;
    de->rec_len = l >> 4;
    de->name_len = len;
    if(end && len > 4) {
        memcpy(dir + 12, name, 4);
        dir += 16;
        name += 4;
        len -= 4;
        while(len) {
            if(dir >= end) {
                dir = fs_base + inode->extent_start[++i] * 512;
                end = fs_base + (inode->extent_start[i] + inode->extent_size[i]) * 512;
            }
            memcpy(dir, name, len > 16 ? 16 : len);
            name += 16;
            dir += 16;
            if(len > 16) len -= 16; else break;
        }
    } else {
        memcpy(dir + 12, name, len);
        dir += l;
    }
    return dir;
}

/*** mkbootimg interface ***/
void len_open(gpt_t *gpt_entry)
{
    int i, j, numband;
    if(!gpt_entry) { fprintf(stderr,"mkbootimg: %s lean.\r\n", lang[ERR_BADINITRDTYPE]); exit(1); }
    len_numblk = (gpt_entry->last - gpt_entry->start + 1);
    if(len_numblk < 32 + LEAN_BITMAPSIZE) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]); exit(1); }
    fs_len = len_numblk * 512;
    fs_base = realloc(fs_base, fs_len);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(fs_base, 0, fs_len);
    numband = len_numblk / ((1 << LEAN_LOG_BANDSIZE) * 512);
    if(numband < 1) numband = 1;
    len_sb = (lean_super_t*)fs_base;
    len_sb->magic = LEAN_SUPER_MAGIC;
    len_sb->fs_version = LEAN_SUPER_VERSION;
    len_sb->log_sectors_per_band = LEAN_LOG_BANDSIZE;
    len_sb->pre_alloc_count = 7;
    len_sb->state = 1;
    memcpy(&len_sb->uuid, &gpt_entry->guid, sizeof(guid_t));
    memcpy(&len_sb->volume_label, "NO NAME", 7);
    len_sb->log_block_size = 9;
    len_sb->sector_count = len_numblk;
    len_sb->free_sector_count = len_numblk - 34 - numband * LEAN_BITMAPSIZE; /* loader, superblock, backup, bitmaps */
    len_sb->primary_super = 32;
    len_sb->backup_super = (len_numblk < (1 << LEAN_LOG_BANDSIZE) ? len_numblk : (1 << LEAN_LOG_BANDSIZE)) - 1;
    len_sb->bitmap_start = len_sb->primary_super + 1;
    for(j = 0; j < numband; j++) {
        for(i = 0; i < LEAN_BITMAPSIZE + (!j ? (int)len_sb->bitmap_start : 0); i++)
            fs_base[(j * (1 << LEAN_LOG_BANDSIZE) + (!j ? len_sb->bitmap_start : 0)) * 512 + i / 8] |= 1 << (i & 7);
    }
    fs_base[len_sb->bitmap_start * 512 + len_sb->backup_super / 8] |= 1 << (len_sb->backup_super & 7);
    len_nextblk = len_sb->bitmap_start + LEAN_BITMAPSIZE;
    len_sb->root_inode = len_alloc_inode(0755, LEAN_FT_DIR, 0, t);
    len_add_dirent(len_add_dirent(NULL,
        len_sb->root_inode, len_sb->root_inode, LEAN_FT_DIR, ".", 1),
        len_sb->root_inode, len_sb->root_inode, LEAN_FT_DIR, "..", 2);
}

void len_add(struct stat *st, char *name, unsigned char *content, int size)
{
    uint64_t parent = len_sb->root_inode, ino, n, j;
    lean_inode_t *inode;
    uint8_t *dir, *end = NULL, type = LEAN_FT_REG;
    char d_name[MAXPATH], *dn, *nend, *fn = strrchr(name, '/');
    int i, k, l;
    if(!fn) fn = name; else fn++;
    if(!strcmp(fn, ".") || !strcmp(fn, "..") || (!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode) && !S_ISLNK(st->st_mode))) return;
    type = S_ISDIR(st->st_mode) ? LEAN_FT_DIR : (S_ISLNK(st->st_mode) ? LEAN_FT_LNK : LEAN_FT_REG);
    n = len_alloc_inode(st->st_mode, type, size, st->st_mtime);
    /* Enter name in directory */
    fn = name;
    nend = strchr(name, '/');
    if(!nend) nend = name + strlen(name);
again:
    i = 0; j = 0;
    inode = (lean_inode_t*)(fs_base + parent * 512);
    if(i < inode->extent_count) {
        dir = fs_base + inode->extent_start[i] * 512 + (!i ? 512 : 0);
        end = fs_base + (inode->extent_start[i] + inode->extent_size[i]) * 512;
        while(j < inode->file_size) {
            ino = ((lean_dirent_t*)dir)->inode;
            k = ((lean_dirent_t*)dir)->rec_len - 1;
            l = ((lean_dirent_t*)dir)->name_len;
            dn = d_name;
            memset(d_name, 0, sizeof(d_name));
            memcpy(dn, dir + 12, 4);
            dn += 4;
            dir += 16;
            j += 16;
            while(j < inode->file_size && k--) {
                if(dir >= end) {
                    dir = fs_base + inode->extent_start[++i] * 512;
                    end = fs_base + (inode->extent_start[i] + inode->extent_size[i]) * 512;
                }
                memcpy(dn, dir, 16);
                dn += 16;
                dir += 16;
                j += 16;
            }
            if(l == nend - fn && !memcmp(d_name, fn, nend - fn)) {
                parent = ino;
                fn = nend + 1;
                nend = *nend ? strchr(fn, '/') : NULL;
                if(!nend) { nend = fn + strlen(fn); break; }
                goto again;
            }
        }
    }
    len_add_dirent(NULL, parent, n, type, fn, nend - fn);
    if(type == LEAN_FT_DIR) {
        len_add_dirent(len_add_dirent(NULL,
            n, n, LEAN_FT_DIR, ".", 1),
            n, parent, LEAN_FT_DIR, "..", 2);
    } else {
        /* works for both regular files and symlinks */
        while(size) {
            k = size > 512 ? 512 : size;
            i = len_alloc_blk();
            memcpy(fs_base + i * 512, content, k);
            len_add_to_inode(n, i, name);
            content += k;
            size -= k;
        }
    }
}

void len_close()
{
    if(!fs_base || (uint64_t)fs_len < (len_sb->backup_super + 1) * 512) return;
    len_sb->checksum = len_checksum(fs_base + len_sb->primary_super * 512, 128);
    memcpy(fs_base + len_sb->backup_super * 512, fs_base + len_sb->primary_super * 512, 512);
}
