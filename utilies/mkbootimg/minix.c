/*
 * mkbootimg/minix.c
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
 * @brief Minix3 file system driver
 * See https://ohm.hgresser.de/sp-ss2012/Intro-MinixFS.pdf
 * (however that's for V2, see the Minix3 source code)
 *
 */
#include "main.h"

typedef uint32_t zone_t;        /* zone number */
typedef uint32_t block_t;       /* block number */
typedef uint32_t bit_t;         /* bit number in a bit map */
typedef uint32_t bitchunk_t;    /* collection of bits in a bitmap */

#define DEFAULT_BLOCK_SIZE      4096
#define SUPER_V3      0x4d5a    /* magic # for V3 file systems */
#define MFS_DIRSIZ     60
#define NR_DZONES       7       /* # direct zone numbers in a V2 inode */
#define NR_TZONES      10       /* total # zone numbers in a V2 inode */
#define NR_DIR_ENTRIES          (int)(DEFAULT_BLOCK_SIZE/sizeof(direct_t))  /* # dir entries/blk  */
#define INDIRECTS               (int)(DEFAULT_BLOCK_SIZE/sizeof(zone_t))  /* # zones/indir block */
#define FS_BITMAP_CHUNKS        (int)(DEFAULT_BLOCK_SIZE/sizeof(bitchunk_t)) /*# map chunks/blk*/
#define FS_BITCHUNK_BITS        (sizeof(bitchunk_t) * 8)
#define FS_BITS_PER_BLOCK       (FS_BITMAP_CHUNKS * FS_BITCHUNK_BITS)

typedef struct {
  uint32_t s_ninodes;           /* # usable inodes on the minor device */
  uint16_t  s_nzones;           /* total device size, including bit maps etc */
  int16_t s_imap_blocks;        /* # of blocks used by inode bit map */
  int16_t s_zmap_blocks;        /* # of blocks used by zone bit map */
  uint16_t s_firstdatazone_old; /* number of first data zone (small) */
  uint16_t s_log_zone_size;     /* log2 of blocks/zone */
  uint16_t s_flags;             /* FS state flags */
  int32_t s_max_size;           /* maximum file size on this device */
  uint32_t s_zones;             /* number of zones (replaces s_nzones in V2) */
  int16_t s_magic;              /* magic number to recognize super-blocks */
  /* The following items are valid on disk only for V3 and above */
  int16_t s_pad2;               /* try to avoid compiler-dependent padding */
  /* The block size in bytes. Minimum MIN_BLOCK SIZE. SECTOR_SIZE multiple.*/
  uint16_t s_block_size;        /* block size in bytes. */
  int8_t s_disk_version;        /* filesystem format sub-version */
} __attribute__((packed)) superblock_t;

typedef struct {
  uint32_t d_ino;
  char d_name[MFS_DIRSIZ];
} __attribute__((packed)) direct_t;

typedef struct {	/* V2/V3 disk inode */
  uint16_t i_mode;		/* file type, protection, etc. */
  uint16_t i_nlinks;		/* how many links to this file. */
  int16_t i_uid;		/* user id of the file's owner. */
  uint16_t i_gid;		/* group number */
  uint32_t i_size;		/* current file size in bytes */
  uint32_t i_atime;		/* when was file data last accessed */
  uint32_t i_mtime;		/* when was file data last changed */
  uint32_t i_ctime;		/* when was inode data last changed */
  uint32_t i_zone[NR_TZONES];	/* zone nums for direct, ind, and dbl ind */
} __attribute__((packed)) inode_t;

block_t mnx_numblk, mnx_inode_offset, mnx_next_zone, mnx_next_inode, mnx_zone_map, mnx_root_inum;
zone_t mnx_zoff;

/* Insert one bit into the bitmap */
void mnx_insert_bit(block_t map, bit_t bit)
{
  int boff, w, s;
  block_t map_block = map + bit / FS_BITS_PER_BLOCK;
  boff = bit % FS_BITS_PER_BLOCK;
  w = boff / FS_BITCHUNK_BITS;
  s = boff % FS_BITCHUNK_BITS;
  *((uint32_t*)(fs_base + map_block * DEFAULT_BLOCK_SIZE + w)) |= (1 << s);
}

/* Increment the link count to inode n */
void mnx_incr_link(ino_t n)
{
    inode_t *inodes = (inode_t*)(fs_base + mnx_inode_offset * DEFAULT_BLOCK_SIZE + (n-1) * sizeof(inode_t));
    inodes[0].i_nlinks++;
}

/* Increment the file-size in inode n */
void mnx_incr_size(ino_t n, size_t count)
{
    inode_t *inodes = (inode_t*)(fs_base + mnx_inode_offset * DEFAULT_BLOCK_SIZE + (n-1) * sizeof(inode_t));
    inodes[0].i_size += count;
}

/* allocate an inode */
static ino_t mnx_alloc_inode(int mode, int usrid, int grpid)
{
    ino_t num;
    inode_t *inodes;
    superblock_t *sup = (superblock_t*)(fs_base + 1024);

    num = mnx_next_inode++;
    if(num > sup->s_ninodes) {
        fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_TOOMANY]);
        exit(1);
    }
    inodes = (inode_t*)(fs_base + mnx_inode_offset * DEFAULT_BLOCK_SIZE + (num-1) * sizeof(inode_t));
    inodes[0].i_mode = mode;
    inodes[0].i_uid = usrid;
    inodes[0].i_gid = grpid;
    /* Set the bit in the bit map. */
    mnx_insert_bit((block_t)2, num);
    return(num);
}

/* Allocate a new zone */
static zone_t mnx_alloc_zone(void)
{
  zone_t z = mnx_next_zone++;
  mnx_insert_bit(mnx_zone_map, z - mnx_zoff);
  return z;
}

void mnx_add_zone(ino_t n, zone_t z, size_t bytes, time_t mtime, char *name)
{
    /* Add zone z to inode n. The file has grown by 'bytes' bytes. */
    int i, j;
    inode_t *p;
    zone_t indir, dindir, *blk, *dblk;

    p = (inode_t*)(fs_base + mnx_inode_offset * DEFAULT_BLOCK_SIZE + (n-1) * sizeof(inode_t));
    p->i_size += bytes;
    p->i_mtime = mtime;
    for (i = 0; i < NR_DZONES; i++)
        if (p->i_zone[i] == 0) {
            p->i_zone[i] = z;
            return;
        }

    /* File has grown beyond a small file. */
    if (p->i_zone[NR_DZONES] == 0)
        p->i_zone[NR_DZONES] = mnx_alloc_zone();
    indir = p->i_zone[NR_DZONES];
    --indir; /* Compensate for ++indir below */
    for (i = 0; i < INDIRECTS; i++) {
        if (i % INDIRECTS == 0)
            blk = (zone_t*)(fs_base + ++indir * DEFAULT_BLOCK_SIZE);
        if (blk[i % INDIRECTS] == 0) {
            blk[i] = z;
            return;
        }
    }

    /* File has grown beyond single indirect; we need a double indirect */
    if (p->i_zone[NR_DZONES+1] == 0)
        p->i_zone[NR_DZONES+1] = mnx_alloc_zone();
    dindir = p->i_zone[NR_DZONES+1];
    --dindir; /* Compensate for ++indir below */
    for (j = 0; j < INDIRECTS; j++) {
        if (j % INDIRECTS == 0)
            dblk = (zone_t*)(fs_base + ++dindir * DEFAULT_BLOCK_SIZE);
        if (dblk[j % INDIRECTS] == 0)
            dblk[j % INDIRECTS] = mnx_alloc_zone();
        indir = dblk[j % INDIRECTS];
        --indir; /* Compensate for ++indir below */
        for (i = 0; i < INDIRECTS; i++) {
            if (i % INDIRECTS == 0)
                blk = (zone_t*)(fs_base + ++indir * DEFAULT_BLOCK_SIZE);
            if (blk[i % INDIRECTS] == 0) {
                blk[i] = z;
                return;
            }
        }
    }
    fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
    exit(1);
}

int mnx_dir_try_enter(zone_t z, ino_t child, char const *name)
{
    direct_t *dir_entry;
    int i;
    dir_entry = (direct_t*)(fs_base + z * DEFAULT_BLOCK_SIZE);
    for (i = 0; i < NR_DIR_ENTRIES; i++)
        if (!dir_entry[i].d_ino)
            break;
    if(i < NR_DIR_ENTRIES) {
        dir_entry[i].d_ino = child;
        strncpy(dir_entry[i].d_name, name, MFS_DIRSIZ);
        return 1;
    }
    return 0;
}

void mnx_enter_dir(ino_t parent, char const *name, ino_t child)
{
    /* Enter child in parent directory */
    /* Works for dir > 1 block and zone > block */
    unsigned int k;
    block_t indir;
    zone_t z;
    inode_t *ino;
    zone_t *indirblock;

    /* Obtain the inode structure */
    ino = (inode_t*)(fs_base + mnx_inode_offset * DEFAULT_BLOCK_SIZE + (parent-1) * sizeof(inode_t));

    for (k = 0; k < NR_DZONES; k++) {
        z = ino->i_zone[k];
        if (z == 0) {
            z = mnx_alloc_zone();
            ino->i_zone[k] = z;
        }

        if(mnx_dir_try_enter(z, child, name))
            return;
    }

    /* no space in directory using just direct blocks; try indirect */
    if (ino->i_zone[NR_DZONES] == 0)
        ino->i_zone[NR_DZONES] = mnx_alloc_zone();

    indir = ino->i_zone[NR_DZONES];
    --indir; /* Compensate for ++indir below */
    for(k = 0; k < INDIRECTS; k++) {
        if (k % INDIRECTS == 0)
            indirblock = (zone_t*)(fs_base + ++indir * DEFAULT_BLOCK_SIZE);
        z = indirblock[k % INDIRECTS];
        if(!z)
            z = indirblock[k % INDIRECTS] = mnx_alloc_zone();
        if(mnx_dir_try_enter(z, child, name))
            return;
    }
    fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
    exit(1);
}

/*** mkbootimg interface ***/
void mnx_open(gpt_t *gpt_entry)
{
    zone_t z;
    superblock_t *sup;
    int i, kb;
    if(!gpt_entry) { fprintf(stderr,"mkbootimg: %s minix.\r\n", lang[ERR_BADINITRDTYPE]); exit(1); }
    mnx_numblk = (gpt_entry->last - gpt_entry->start + 1) * 512 / DEFAULT_BLOCK_SIZE;
    if(mnx_numblk < 8) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]); exit(1); }
    /* "format" the partition to Minix3FS */
    fs_len = mnx_numblk * DEFAULT_BLOCK_SIZE;
    fs_base = realloc(fs_base, fs_len);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(fs_base, 0, fs_len);
    sup = (superblock_t*)(fs_base + 1024);
    kb = fs_len / 1024;
    sup->s_ninodes = kb / 2;
    if (kb >= 100000) sup->s_ninodes = kb / 4;
    if (kb >= 1000000) sup->s_ninodes = kb / 6;
    if (kb >= 10000000) sup->s_ninodes = kb / 8;
    if (kb >= 100000000) sup->s_ninodes = kb / 10;
    if (kb >= 1000000000) sup->s_ninodes = kb / 12;
    sup->s_ninodes += (DEFAULT_BLOCK_SIZE/sizeof(inode_t)) - 1;
    sup->s_ninodes &= ~((DEFAULT_BLOCK_SIZE/sizeof(inode_t)) - 1);
    if(sup->s_ninodes < 1) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", fs_no, lang[ERR_NOSIZE]); exit(1); }
    sup->s_zones = mnx_numblk;
    sup->s_imap_blocks = (1 + sup->s_ninodes + DEFAULT_BLOCK_SIZE - 1) / DEFAULT_BLOCK_SIZE;
    sup->s_zmap_blocks = (mnx_numblk + DEFAULT_BLOCK_SIZE - 1) / DEFAULT_BLOCK_SIZE;
    mnx_inode_offset = 2 + sup->s_imap_blocks + sup->s_zmap_blocks;
    sup->s_magic = SUPER_V3;
    sup->s_block_size = DEFAULT_BLOCK_SIZE;
    i = NR_DZONES+(DEFAULT_BLOCK_SIZE/sizeof(inode_t))+(DEFAULT_BLOCK_SIZE/sizeof(inode_t))*(DEFAULT_BLOCK_SIZE/sizeof(inode_t));
    if(INT32_MAX/DEFAULT_BLOCK_SIZE < i)
        sup->s_max_size = INT32_MAX;
    else
        sup->s_max_size = i * DEFAULT_BLOCK_SIZE;
    mnx_next_zone = mnx_inode_offset + (sup->s_ninodes+(DEFAULT_BLOCK_SIZE/sizeof(inode_t))-1)/(DEFAULT_BLOCK_SIZE/sizeof(inode_t));
    mnx_zoff = mnx_next_zone - 1;
    mnx_next_inode = 1;
    mnx_zone_map = 2 + sup->s_imap_blocks;
    mnx_insert_bit(mnx_zone_map, 0);    /* bit zero must always be allocated */
    mnx_insert_bit((block_t)2, 0);      /* inode zero not used but must be allocated */
    mnx_root_inum = mnx_alloc_inode(0755, 0, 0);
    z = mnx_alloc_zone();
    mnx_add_zone(mnx_root_inum, z, 2 * sizeof(direct_t), t, "rootdir");
    mnx_enter_dir(mnx_root_inum, ".", mnx_root_inum);
    mnx_enter_dir(mnx_root_inum, "..", mnx_root_inum);
    mnx_incr_link(mnx_root_inum);
    mnx_incr_link(mnx_root_inum);
}

void mnx_add(struct stat *st, char *name, unsigned char *content, int size)
{
    ino_t n, parent = mnx_root_inum;
    inode_t *ino;
    zone_t z;
    direct_t *dir_entry;
    int i, k;
    char *end, *fn = strrchr(name, '/');
    if(!fn) fn = name; else fn++;
    if(!strcmp(fn, ".") || !strcmp(fn, "..")) return;
    if(!S_ISREG(st->st_mode) && !S_ISDIR(st->st_mode) && !S_ISLNK(st->st_mode) && !S_ISCHR(st->st_mode) && !S_ISBLK(st->st_mode))
        return;
    n = mnx_alloc_inode(st->st_mode, st->st_uid, st->st_gid);
    /* Enter name in directory and update directory's size. */
    fn = name;
    end = strchr(name, '/');
    if(!end) end = name + strlen(name);
    i = k = 0;
    do {
        /* FIXME: this doesn't handle indirect and double indirect data */
        ino = (inode_t*)(fs_base + mnx_inode_offset * DEFAULT_BLOCK_SIZE + (parent-1) * sizeof(inode_t));
        if(!ino->i_zone[k]) break;
        if(k >= NR_DZONES) {
            fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
            exit(1);
        }
        dir_entry = (direct_t*)(fs_base + ino->i_zone[k] * DEFAULT_BLOCK_SIZE);
        if(!memcmp(dir_entry[i].d_name, fn, end - fn) && !dir_entry[i].d_name[end - fn]) {
            parent = dir_entry[i].d_ino; i = k = 0;
            fn = end + 1;
            end = *end ? strchr(fn, '/') : NULL;
            if(!end) break;
        }
        i++;
        if(i == NR_DIR_ENTRIES) { i = 0; k++; }
        if((k * NR_DIR_ENTRIES + i) * sizeof(direct_t) >= ino->i_size) break;
    } while(1);
    mnx_enter_dir(parent, fn, n);
    mnx_incr_size(parent, sizeof(direct_t));

    /* Check to see if file is directory or special. */
    mnx_incr_link(n);
    if (S_ISDIR(st->st_mode)) {
        /* This is a directory. */
        z = mnx_alloc_zone();	/* zone for new directory */
        mnx_add_zone(n, z, 2 * sizeof(direct_t), st->st_mtime, name);
        mnx_enter_dir(n, ".", n);
        mnx_enter_dir(n, "..", parent);
        mnx_incr_link(parent);
        mnx_incr_link(n);
    } else if (S_ISCHR(st->st_mode) || S_ISBLK(st->st_mode)) {
        /* Special file. */
        mnx_add_zone(n, (zone_t)st->st_rdev, st->st_size, st->st_mtime, name);
    } else if (S_ISLNK(st->st_mode)) {
        if(size > DEFAULT_BLOCK_SIZE - 1) {
            fprintf(stderr,"mkbootimg: partition #%d %s: %s\r\n", fs_no, lang[ERR_TOOBIG], name);
            exit(1);
        }
        z = mnx_alloc_zone();
        memcpy(fs_base + z * DEFAULT_BLOCK_SIZE, content, size + 1);
        mnx_add_zone(n, z, size, st->st_mtime, name);
    } else {
        /* Regular file. Go read it. */
        while(size) {
            z = mnx_alloc_zone();
            memcpy(fs_base + z * DEFAULT_BLOCK_SIZE, content, DEFAULT_BLOCK_SIZE);
            mnx_add_zone(n, z, size < DEFAULT_BLOCK_SIZE ? size : DEFAULT_BLOCK_SIZE, st->st_mtime, name);
            if(size > DEFAULT_BLOCK_SIZE) {
                content += DEFAULT_BLOCK_SIZE;
                size -= DEFAULT_BLOCK_SIZE;
            } else
                break;
        }
    }
}

void mnx_close()
{
}
