/*
 * mkbootimg/ext2.c
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
 * @brief very simple ext2 file system driver
 * See https://www.nongnu.org/ext2-doc/ext2.html
 *
 */
#include "main.h"

#define EXT2_SUPER_MAGIC 0xEF53
#define EXT2_S_IFLNK    0xA000
#define EXT2_S_IFREG    0x8000
#define EXT2_S_IFBLK    0x6000
#define EXT2_S_IFDIR    0x4000
#define EXT2_S_IFCHR    0x2000
#define EXT2_S_IFIFO    0x1000

enum {
    EXT2_FT_UNKNOWN,
    EXT2_FT_REG_FILE,
    EXT2_FT_DIR,
    EXT2_FT_CHRDEV,
    EXT2_FT_BLKDEV,
    EXT2_FT_FIFO,
    EXT2_FT_SOCK,
    EXT2_FT_SYMLINK
};

#define SECSIZE 4096

typedef struct {
    uint32_t bg_block_bitmap;
    uint32_t bg_inode_bitmap;
    uint32_t bg_inode_table;
    uint16_t bg_free_blocks_count;
    uint16_t bg_free_inodes_count;
    uint16_t bg_used_dirs_count;
    uint16_t bg_flags;
    uint8_t pad[12];
} __attribute__((packed)) ext_bg_t;

typedef struct {
    uint8_t loader[1024];
    uint32_t s_inodes_count;
    uint32_t s_blocks_count;
    uint32_t s_r_blocks_count;
    uint32_t s_free_blocks_count;
    uint32_t s_free_inodes_count;
    uint32_t s_first_data_block;
    uint32_t s_log_block_size;
    uint32_t s_log_frag_size;
    uint32_t s_blocks_per_group;
    uint32_t s_frags_per_group;
    uint32_t s_inodes_per_group;
    uint32_t s_mtime;
    uint32_t s_wtime;
    uint16_t s_mnt_count;
    uint16_t s_max_mnt_count;
    uint16_t s_magic;
    uint16_t s_state;
    uint16_t s_errors;
    uint16_t s_minor_rev_level;
    uint32_t s_lastcheck;
    uint32_t s_checkinterval;
    uint32_t s_creator_os;
    uint32_t s_rev_level;
    uint16_t s_def_resuid;
    uint16_t s_def_resgid;
    uint32_t s_first_ino;
    uint16_t s_inode_size;
    uint16_t s_block_group_nr;
    uint32_t s_feature_compat;
    uint32_t s_feature_incompat;
    uint32_t s_feature_ro_compat;
    uint8_t s_uuid[16];
    uint8_t pad1[SECSIZE-120-1024];
    ext_bg_t s_bg[SECSIZE/sizeof(ext_bg_t)];
} __attribute__((packed)) ext_sb_t;

typedef struct {
    uint16_t i_mode;
    uint16_t i_uid;
    uint32_t i_size;
    uint32_t i_atime;
    uint32_t i_ctime;
    uint32_t i_mtime;
    uint32_t i_dtime;
    uint16_t i_gid;
    uint16_t i_links_count;
    uint32_t i_blocks;
    uint32_t i_flags;
    uint32_t i_osd1;
    uint32_t i_block[15];
    uint32_t i_generation;
    uint32_t i_file_acl;
    uint32_t i_dir_acl;
    uint32_t i_faddr;
    uint16_t i_osd2[6];
} __attribute__((packed)) ext_inode_t;

typedef struct {
    uint32_t inode;
    uint16_t rec_len;
    uint8_t name_len;
    uint8_t type;
} __attribute__((packed)) ext_dirent_t;

uint32_t ext_numblk, ext_numbg, ext_nextinode, ext_nextblk, ext_blkgap, ext_root;
uint8_t *ext_lastdir;
ext_sb_t *ext_sb;

int ext_alloc_blk()
{
    int r, g = ext_nextblk / ext_sb->s_blocks_per_group, o = ext_nextblk % ext_sb->s_blocks_per_group;
    if(ext_nextblk + 1 >= ext_sb->s_blocks_count || ext_sb->s_free_blocks_count < 1) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOBIG]);
        exit(1);
    }
    fs_base[ext_sb->s_bg[g].bg_block_bitmap * SECSIZE + o/8] |= 1<<(o&7);
    ext_sb->s_bg[g].bg_free_blocks_count--;
    ext_sb->s_free_blocks_count--;
    r = ext_nextblk++;
    if(!(ext_nextblk % ext_sb->s_blocks_per_group)) ext_nextblk += ext_blkgap;
    return r;
}

int ext_alloc_inode(uint16_t mode, uint32_t size, uint16_t uid, uint16_t gid, time_t t)
{
    ext_inode_t *inode;
    int g = ext_nextinode / ext_sb->s_inodes_per_group, o = ext_nextinode % ext_sb->s_inodes_per_group;
    if(ext_nextinode + 1 >= ext_sb->s_inodes_count || ext_sb->s_free_inodes_count < 1) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOMANY]);
        exit(1);
    }
    fs_base[ext_sb->s_bg[g].bg_inode_bitmap * SECSIZE + o/8] |= 1<<(o&7);
    inode = (ext_inode_t*)(fs_base + ext_sb->s_bg[g].bg_inode_table * SECSIZE);
    inode[o].i_mode = mode | (!(mode & 0xFFF) ? 0755 : 0);
    inode[o].i_size = size;
    inode[o].i_blocks = (size + 511) / 512;
    inode[o].i_uid = uid;
    inode[o].i_gid = gid;
    inode[o].i_ctime = inode[o].i_atime = inode[o].i_mtime = (uint32_t)t;
    if((mode & 0xF000) == EXT2_S_IFDIR)
        ext_sb->s_bg[g].bg_used_dirs_count++;
    ext_sb->s_bg[g].bg_free_inodes_count--;
    ext_sb->s_free_inodes_count--;
    ext_nextinode++;
    return ext_nextinode;
}

void ext_add_to_inode(uint32_t ino, uint32_t blk, char *name)
{
    int i, j;
    ext_inode_t *inode;
    uint32_t *ind, *dind;
    int g = (ino - 1) / ext_sb->s_inodes_per_group, o = (ino - 1) % ext_sb->s_inodes_per_group;
    inode = (ext_inode_t*)(fs_base + ext_sb->s_bg[g].bg_inode_table * SECSIZE);
    for(i = 0; i < 12; i++)
        if(!inode[o].i_block[i]) {
            inode[o].i_block[i] = blk;
            return;
        }
    if(!inode[o].i_block[12])
        inode[o].i_block[12] = ext_alloc_blk();
    ind = (uint32_t*)(fs_base + inode[o].i_block[12] * SECSIZE);
    for(i = 0; i < SECSIZE / 4; i++)
        if(!ind[i]) {
            ind[i] = blk;
            return;
        }
    if(!inode[o].i_block[13])
        inode[o].i_block[13] = ext_alloc_blk();
    dind = (uint32_t*)(fs_base + inode[o].i_block[13] * SECSIZE);
    for(j = 0; j < SECSIZE / 4; j++) {
        if(!dind[j])
            dind[j] = ext_alloc_blk();
        ind = (uint32_t*)(fs_base + dind[j] * SECSIZE);
        for(i = 0; i < SECSIZE / 4; i++)
            if(!ind[i]) {
                ind[i] = blk;
                return;
            }
    }
    fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
    exit(1);
}

uint8_t *ext_add_dirent(uint8_t *dir, uint32_t toinode, uint32_t ino, uint8_t type, char *name, int len)
{
    ext_dirent_t *de;
    ext_inode_t *inode;
    int k;
    int g = (ino - 1) / ext_sb->s_inodes_per_group, o = (ino - 1) % ext_sb->s_inodes_per_group;
    if(ino) {
        inode = (ext_inode_t*)(fs_base + ext_sb->s_bg[g].bg_inode_table * SECSIZE);
        inode[o].i_links_count++;
    }
    if(ext_lastdir && dir) {
        if((uint32_t)(dir - fs_base)/SECSIZE != (uint32_t)(dir - fs_base + len + 8)/SECSIZE) {
            dir = NULL;
        } else
            ((ext_dirent_t*)ext_lastdir)->rec_len = dir - ext_lastdir;
    }
    if(!dir) {
        k = ext_alloc_blk();
        ext_add_to_inode(toinode, k, name);
        dir = fs_base + k * SECSIZE;
    }
    de = (ext_dirent_t*)dir;
    de->inode = ino;
    de->rec_len = SECSIZE - ((uintptr_t)(dir - fs_base) & (SECSIZE - 1));
    de->name_len = len;
    de->type = type;
    if(name && len)
        memcpy(dir + 8, name, len);
    ext_lastdir = dir;
    return dir + ((len + 3) & ~3) + 8;
}

/*** mkbootimg interface ***/
void ext_open(gpt_t *gpt_entry)
{
    uint32_t i, j, k, l, n, m, o;
    if(!gpt_entry) { fprintf(stderr,"mkbootimg: %s ext2.\r\n", lang[ERR_BADINITRDTYPE]); exit(1); }
    ext_numblk = (gpt_entry->last - gpt_entry->start + 1) * 512 / SECSIZE;
    if(ext_numblk < 8) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]); exit(1); }
    fs_len = ext_numblk * SECSIZE;
    fs_base = realloc(fs_base, fs_len);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(fs_base, 0, fs_len);
    ext_nextinode = ext_nextblk = 0;
    ext_numbg = ext_numblk / (SECSIZE<<3);
    if(ext_numbg < 1) ext_numbg = 1;
    if(ext_numbg > (int)(SECSIZE/sizeof(ext_bg_t)) - 1) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOMANY]);
        exit(1);
    }
    ext_sb = (ext_sb_t*)fs_base;
    ext_sb->s_blocks_count = ext_numblk;
    ext_sb->s_r_blocks_count = ext_numblk * 5 / 100;
    ext_sb->s_log_block_size = ext_sb->s_log_frag_size = 2;
    ext_sb->s_blocks_per_group = ext_sb->s_frags_per_group = (SECSIZE<<3);
    ext_sb->s_inodes_count = ext_sb->s_blocks_count;
    ext_sb->s_inodes_per_group = ext_sb->s_inodes_count / ext_numbg;
    if(ext_sb->s_inodes_per_group > (SECSIZE<<3)) ext_sb->s_inodes_per_group = (SECSIZE<<3);
    ext_sb->s_inodes_count = ext_sb->s_inodes_per_group * ext_numbg;
    if(ext_sb->s_inodes_count > ext_sb->s_blocks_count)
        ext_sb->s_inodes_count = ext_sb->s_blocks_count;
    ext_sb->s_free_inodes_count = ext_sb->s_inodes_count;
    ext_sb->s_wtime = ext_sb->s_lastcheck = (uint32_t)t;
    ext_sb->s_max_mnt_count = 65535;
    ext_sb->s_magic = EXT2_SUPER_MAGIC;
    ext_sb->s_state = ext_sb->s_errors = 1;
    ext_sb->s_rev_level = 1;
    ext_sb->s_feature_incompat = 2;
    ext_sb->s_first_ino = 11;
    ext_sb->s_inode_size = 128;
    memcpy(fs_base + 1024 + 104, &gpt_entry->guid, 16);
    for(i = k = m = 0; i < ext_numbg; i++) {
        j = ((ext_sb->s_blocks_per_group+7)/8 + SECSIZE - 1) / SECSIZE;
        if(k + j > ext_numblk) j = ext_numblk - k;
        ext_sb->s_bg[i].bg_block_bitmap = (SECSIZE<<3) * i + 2;
        ext_sb->s_bg[i].bg_inode_bitmap = (SECSIZE<<3) * i + 2 + j;
        ext_sb->s_bg[i].bg_inode_table = (SECSIZE<<3) * i + 3 + j;
        o = m + ext_sb->s_inodes_per_group > ext_sb->s_inodes_count ? ext_sb->s_inodes_count - m : ext_sb->s_inodes_per_group;
        if((uint32_t)o > ext_sb->s_free_inodes_count) o = ext_sb->s_free_inodes_count;
        l = 3 + j + (o * sizeof(ext_inode_t) + SECSIZE - 1) / SECSIZE;
        for(n = 0; n < l; n++)
            fs_base[((SECSIZE<<3) * i + 2) * SECSIZE + n/8] |= 1<<(n&7);
        if(!ext_nextblk) ext_nextblk = ext_blkgap = l;
        ext_sb->s_bg[i].bg_free_inodes_count = o;
        ext_sb->s_bg[i].bg_free_blocks_count = (ext_numblk - k > (SECSIZE<<3) ? (SECSIZE<<3) : ext_numblk - k) - l;
        ext_sb->s_free_blocks_count += ext_sb->s_bg[i].bg_free_blocks_count;
        k += ext_sb->s_blocks_per_group;
        m += ext_sb->s_inodes_per_group;
    }
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 1. bad blocks list inode */
    ext_root = ext_alloc_inode(EXT2_S_IFDIR, SECSIZE, 0, 0, t); /* 2. root directory inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 3. acl index inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 4. acl data inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 5. loader inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 6. undelete inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 7. resize inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 8. journal inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 9. exclude inode */
    ext_alloc_inode(EXT2_S_IFREG, 0, 0, 0, t);                  /* 10. replica inode */
    i = ext_alloc_inode(EXT2_S_IFDIR|0700, 4*SECSIZE, 0, 0, t); /* 11. lost+found inode, needs 4 blocks at minimum */
    ext_add_dirent(ext_add_dirent(ext_add_dirent(
        NULL, ext_root, ext_root, EXT2_FT_DIR, ".", 1),
        ext_root, ext_root, EXT2_FT_DIR, "..", 2),
        ext_root, i, EXT2_FT_DIR, "lost+found", 10);
    ext_add_dirent(ext_add_dirent(NULL, i, i, EXT2_FT_DIR, ".", 1), i, ext_root, EXT2_FT_DIR, "..", 2);
    ext_add_dirent(NULL, i, 0, EXT2_FT_UNKNOWN, NULL, 0);
    ext_add_dirent(NULL, i, 0, EXT2_FT_UNKNOWN, NULL, 0);
    ext_add_dirent(NULL, i, 0, EXT2_FT_UNKNOWN, NULL, 0);
}

void ext_add(struct stat *st, char *name, unsigned char *content, int size)
{
    uint8_t *dir_entry, *blk, t = EXT2_FT_REG_FILE;
    ext_inode_t *inode;
    uint32_t n, parent = ext_root;
    int i, k, g, o;
    char *end, *fn = strrchr(name, '/');
    if(!fn) fn = name; else fn++;
    if(!strcmp(fn, ".") || !strcmp(fn, "..")) return;
    if(!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode) && !S_ISLNK(st->st_mode) && !S_ISCHR(st->st_mode) && !S_ISBLK(st->st_mode))
        return;
    if(S_ISDIR(st->st_mode)) t = EXT2_FT_DIR; else
    if(S_ISLNK(st->st_mode)) t = EXT2_FT_SYMLINK; else
    if(S_ISCHR(st->st_mode)) t = EXT2_FT_CHRDEV; else
    if(S_ISBLK(st->st_mode)) t = EXT2_FT_BLKDEV;
    n = ext_alloc_inode(st->st_mode, st->st_size, st->st_uid, st->st_gid, st->st_mtime);
    /* Enter name in directory */
    fn = name;
    end = strchr(name, '/');
    if(!end) end = name + strlen(name);
    i = k = 0; ext_lastdir = NULL;
again:
    /* FIXME: this doesn't handle indirect and double indirect data */
    g = (parent - 1) / ext_sb->s_inodes_per_group; o = (parent - 1) % ext_sb->s_inodes_per_group;
    inode = (ext_inode_t*)(fs_base + ext_sb->s_bg[g].bg_inode_table * SECSIZE);
    if(k > 11) {
        fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOMANY], name);
        exit(1);
    }
    if(!inode[o].i_block[k])
        dir_entry = ext_lastdir = NULL;
    else {
        dir_entry = blk = (uint8_t*)(fs_base + inode[o].i_block[k] * SECSIZE);
        while(((ext_dirent_t*)dir_entry)->inode) {
            if(((ext_dirent_t*)dir_entry)->name_len == end - fn && !memcmp(dir_entry + 8, fn, end - fn)) {
                parent = ((ext_dirent_t*)dir_entry)->inode; i = k = 0; ext_lastdir = NULL;
                fn = end + 1;
                end = *end ? strchr(fn, '/') : NULL;
                if(!end) end = fn + strlen(fn);
                goto again;
            }
            ext_lastdir = dir_entry;
            if((uint32_t)i + ((ext_dirent_t*)dir_entry)->rec_len >= inode[o].i_size) {
                dir_entry += ((((ext_dirent_t*)dir_entry)->name_len + 3) & ~3) + 8;
                break;
            }
            i += ((ext_dirent_t*)dir_entry)->rec_len;
            dir_entry += ((ext_dirent_t*)dir_entry)->rec_len;
            if(dir_entry - blk >= SECSIZE) { k++; goto again; }
        }
    }
    ext_add_dirent(dir_entry, parent, n, t, fn, end - fn);

    if(S_ISDIR(st->st_mode)) {
        ext_add_dirent(ext_add_dirent(NULL, n, n, EXT2_FT_DIR, ".", 1), n, parent, EXT2_FT_DIR, "..", 2);
    } else
    if(S_ISCHR(st->st_mode) || S_ISBLK(st->st_mode)) {
        g = (n - 1) / ext_sb->s_inodes_per_group; o = (n - 1) % ext_sb->s_inodes_per_group;
        inode = (ext_inode_t*)(fs_base + ext_sb->s_bg[g].bg_inode_table * SECSIZE);
        inode[o].i_block[0] = st->st_rdev;
    } else
    if(S_ISLNK(st->st_mode)) {
        if(size >= SECSIZE) {
            fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
            exit(1);
        }
        i = ext_alloc_blk();
        memcpy(fs_base + i * SECSIZE, content, k);
        ext_add_to_inode(n, i, name);
    } else {
        while(size) {
            k = size > SECSIZE ? SECSIZE : size;
            i = ext_alloc_blk();
            memcpy(fs_base + i * SECSIZE, content, k);
            ext_add_to_inode(n, i, name);
            content += SECSIZE;
            size -= k;
        }
    }
}

void ext_close()
{
    ext_sb_t *sb;
    uint32_t i;
    for(i = 1; i < ext_numbg && (int)i * (SECSIZE<<3) + 2 * SECSIZE <= fs_len; i++) {
        sb = (ext_sb_t*)(fs_base + i * (SECSIZE<<3));
        memcpy(sb, fs_base, 2 * SECSIZE);
        sb->s_block_group_nr = i;
    }
}
