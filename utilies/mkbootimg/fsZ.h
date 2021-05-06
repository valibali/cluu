/*
 * include/fsZ.h
 *
 * Copyright (C) 2017 bzt (bztsrc@gitlab)
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
 * IMPORTANT NOTE: the on disk format of FS/Z is MIT licensed, you
 * can use it without any restrictions and free of charge. The right
 * to create and use disks and images with FS/Z and implement programs
 * to do so is hereby granted to everybody. The filesystem driver for
 * it in OS/Z on the other hand licensed under CC-by-nc-sa.
 *
 * @brief FS/Z filesystem defines and structures for on disk format
 */

#ifndef _FS_Z_H_
#define _FS_Z_H_    1

#define FSZ_VERSION_MAJOR 1
#define FSZ_VERSION_MINOR 0
#define FSZ_SECSIZE 4096

/* logical sector numbers are 128 bits wide to be future proof, and are relative to
 * the superblock. Current implementation only supports 64 bits, that gives you the
 * capacity of 64 Zettabytes with 4096 bytes sector size. */

/* for CRC32 the Castagnoli method is used, polynomial 0x1EDC6F41 (differs to ANSI
 * CRC32a in EFI GPT and in gzip) but has hardware support on many architectures */

/*********************************************************
 *            1st sector, the super block                *
 *********************************************************/
typedef struct {
    uint8_t     loader[512];        /*   0 reserved for loader code */
    uint8_t     magic[4];           /* 512 */
    uint8_t     version_major;      /* 516 */
    uint8_t     version_minor;      /* 517 */
    uint8_t     logsec;             /* 518 logical sector size, 0=2048,1=4096(default),2=8192... */
    uint8_t     enctype;            /* 519 encryption alogirthm */
    uint32_t    flags;              /* 520 flags */
    uint16_t    maxmounts;          /* 524 number of maximum mounts allowed to next fsck */
    uint16_t    currmounts;         /* 526 current mount counter */
    uint64_t    numsec;             /* 528 total number of logical sectors */
    uint64_t    numsec_hi;          /*      128 bit */
    uint64_t    freesec;            /* 544 first free sector if fs is defragmented, otherwise */
    uint64_t    freesec_hi;         /*     it's the last used sector+1 */
    uint64_t    rootdirfid;         /* 560 logical sector number of root directory's inode, usually LSN 1 */
    uint64_t    rootdirfid_hi;
    uint64_t    freesecfid;         /* 576 inode to free records (FSZ_SectorList allocated) */
    uint64_t    freesecfid_hi;
    uint64_t    badsecfid;          /* 592 inode to bad sectors table (FSZ_SectorList allocated) */
    uint64_t    badsecfid_hi;
    uint64_t    indexfid;           /* 608 inode to search index. Zero if not indexed */
    uint64_t    indexfid_hi;
    uint64_t    metafid;            /* 624 inode to meta labels file. Zero if there're no meta labels at all */
    uint64_t    metafid_hi;
    uint64_t    journalfid;         /* 640 inode to journal file. Zero if journaling is turned off */
    uint64_t    journalfid_hi;
    uint64_t    journalhead;        /* 656 logical sector offset inside journal file where buffer starts */
    uint64_t    journaltail;        /* 664 logical sector offset inside journal file where buffer ends */
    uint64_t    journalmax;         /* 672 number of logical sectors in journal file */
    uint8_t     encrypt[28];        /* 680 encryption mask for AES or zero */
    uint32_t    enchash;            /* 708 password CRC32, to avoid decryption with bad passwords */
    uint64_t    createdate;         /* 712 creation timestamp UTC */
    uint64_t    lastmountdate;      /* 720 last time fs was mounted */
    uint64_t    lastumountdate;     /* 728 time when superblock was written to disk, must be zero when its mounted */
    uint64_t    lastcheckdate;      /* 736 last time fs was checked with fsck */
    uint8_t     uuid[16];           /* 744 filesystem UUID */
    uint8_t     reserved[256];      /* 760 reserved for access list */
    uint8_t     magic2[4];          /*1016 */
    uint32_t    checksum;           /*1020 CRC32 of bytes at 512-1020 */
    uint8_t     raidspecific[FSZ_SECSIZE-1024];
} __attribute__((packed)) FSZ_SuperBlock;

#define FSZ_MAGIC               "FS/Z"
#define FSZ_RAID_MAGIC          "FSRD"

#define FSZ_SB_EALG_SHACBC      0       /* encrypted with SHA-XOR-CBC */
#define FSZ_SB_EALG_AESCBC      1       /* encrypted with AES-256-CBC */
/* values 2 - 255 are reserved for future use */

/* feature flags */
#define FSZ_SB_BIGINODE         (1<<0)  /* indicates inode size is 2048 (ACL size 96 instead of 32) */
#define FSZ_SB_JOURNAL_DATA     (1<<1)  /* also put file content records in journal file, not just metadata */
#define FSZ_SB_SOFTRAID         (1<<2)  /* single disk when not set */
#define FSZ_SB_ACCESSDATE       (1<<3)  /* store last access timestamp in i-nodes */
/* bits 4 - 31 are reserved for future use */

/*********************************************************
 *                    I-node sector                      *
 *********************************************************/
/* fid: file id, logical sector number where the sector contains an inode structure. */

/* sizeof = 16, one Access Control Entry, UUID without the last byte */
typedef struct {
    uint32_t Data1;
    uint16_t Data2;
    uint16_t Data3;
    uint8_t  Data4[7];
    uint8_t  access;
} __attribute__((packed)) FSZ_Access;

/* access rights are stored in the last byte. Make sure this matches
 * the system access flags A_* in sys/types.h */
#define FSZ_READ    (1<<0)
#define FSZ_WRITE   (1<<1)
#define FSZ_EXEC    (1<<2)          /* execute or search */
#define FSZ_APPEND  (1<<3)
#define FSZ_DELETE  (1<<4)
#define FSZ_GROUP   (1<<5)
#define FSZ_SUID    (1<<6)          /* Set user id on execution */
#define FSZ_SGID    (1<<7)          /* Inherit ACL, no set group per se in OS/Z */

/* sizeof = 32 */
typedef struct {
    uint64_t    sec;
    uint64_t    sec_hi;
    uint64_t    numsec;
    uint32_t    numsec_hi;
    uint32_t    chksum;
} __attribute__((packed)) FSZ_SectorList;
/* used at several places, like free and bad block list inodes, and with FSZ_IN_FLAG_SECLIST* mappings. */

/* sizeof = 16 */
typedef struct {
    uint64_t    sec;
    uint32_t    sec_hi;
    uint32_t    chksum;
} __attribute__((packed)) FSZ_SectorDir;
/* used with FSZ_IN_FLAG_SD* mappings. */

/* file version structure. You can use this to point to version5, version4 etc. */
/* sizeof = 64 */
typedef struct {
    uint64_t    sec;
    uint64_t    sec_hi;
    uint64_t    size;
    uint64_t    size_hi;
    uint64_t    modifydate;
    uint64_t    flags;
    FSZ_Access  owner;
} __attribute__((packed)) FSZ_Version;

/* sizeof = 4096 */
typedef struct {
    uint8_t     magic[4];       /*   0 magic 'FSIN' */
    uint32_t    checksum;       /*   4 CRC32, filetype to inlinedata (exclusive) */
    uint8_t     filetype[4];    /*   8 first 4 bytes of mime main type, eg: text,imag,vide,audi,appl etc. */
    uint8_t     mimetype[60];   /*  12 mime sub type, eg.: plain, html, gif, jpeg etc. (*) */
    uint64_t    createdate;     /*  72 number of microseconds since 1970. jan. 1 00:00:00 UTC, inode creation time */
    uint64_t    changedate;     /*  80 number of microseconds since 1970. jan. 1 00:00:00 UTC, inode status change time */
    uint64_t    accessdate;     /*  88 last data access time (if implemented, otherwise zero) */
    uint64_t    numblocks;      /*  96 number of blocks allocated for this inode (**) */
    uint64_t    numlinks;       /* 104 number of references to this inode */
    uint64_t    metasec;        /* 112 logical sector number of meta label block */
    uint64_t    metasec_hi;
    FSZ_Version version5;       /* 128 previous oldest version (if versioning enabled) */
    FSZ_Version version4;       /* 192    all the same format as the current one */
    FSZ_Version version3;       /* 256    see FSZ_Version structure above */
    FSZ_Version version2;       /* 320 */
    FSZ_Version version1;       /* 384 */
    /* FSZ_Version current; I haven't used FSZ_Version struct here to save typing when referencing */
    uint64_t         sec;       /* 448 current (or only) version (***) */
    uint64_t         sec_hi;
    uint64_t         size;      /* 464 file size */
    uint64_t         size_hi;
    uint64_t         modifydate;/* 480 */
    uint64_t         flags;     /* 488 see FSZ_IN_FLAG_* (***) */
    /* owner is the last in FSZ_Version to followed by ACL, so it can be considered to be the first
     * entry in the Access Control List. It is the control ACE, specifies who can modify the ACL. */
    FSZ_Access       owner;     /* 496 */
    union {
        struct {
          FSZ_Access groups[32];/* 512 List of 32 FSZ_Access entries, groups */
          uint8_t    inlinedata[FSZ_SECSIZE-1024];
        } small;
        struct {
          FSZ_Access groups[96];/* 512 List of 96 FSZ_Access entries, groups */
          uint8_t    inlinedata[FSZ_SECSIZE-2048];
        } big;
    } data;
} __attribute__((packed)) FSZ_Inode;

#define FSZ_IN_MAGIC            "FSIN"

/* (*) according to IANA, all, more than 1700 mime types are unique on 4+60 bytes. */

/* regular files, 4th character never ':' */
#define FSZ_FILETYPE_REG_TEXT   "text"  /* main part of mime type */
#define FSZ_FILETYPE_REG_IMAGE  "imag"
#define FSZ_FILETYPE_REG_VIDEO  "vide"
#define FSZ_FILETYPE_REG_AUDIO  "audi"
#define FSZ_FILETYPE_REG_APP    "appl"
#define FSZ_FILETYPE_REG_BOOT   "boot"  /* same as "appl", except file cannot be relocated during defrag */
/* special entities, 4th character always ':' */
#define FSZ_FILETYPE_DIR        "dir:"  /* directory */
#define FSZ_FILETYPE_UNION      "uni:"  /* directory union, inlined data is a zero separated list of paths with jokers */
#define FSZ_FILETYPE_INTERNAL   "int:"  /* internal files, like free and bad sectors and meta info */
#define FSZ_FILETYPE_SYMLINK    "lnk:"  /* symbolic link, inlined data is a path */
#define FSZ_FILETYPE_PIPE       "pip:"  /* named pipe (FIFO) */
#define FSZ_FILETYPE_DEV        "dev:"  /* device */
#define FSZ_FILETYPE_SOCK       "sck:"  /* socket */

/* mime types for filesystem specific files */
/* for FSZ_FILETYPE_DIR */
#define FSZ_MIMETYPE_DIR_ROOT    "fs-root"  /* root directory (for recovery it has a special mime type) */
/* for FSZ_FILETYPE_INTERNAL */
#define FSZ_MIMETYPE_INT_FREELST "fs-free-sectors" /* for free sector list */
#define FSZ_MIMETYPE_INT_BADLST  "fs-bad-sectors"  /* for bad sector list */
#define FSZ_MIMETYPE_INT_INDEX   "fs-search-index" /* search cache */
#define FSZ_MIMETYPE_INT_META    "fs-meta-labels"  /* meta labels */
#define FSZ_MIMETYPE_INT_JOURNAL "fs-journal"      /* journaling records (contains only metadata) */
#define FSZ_MIMETYPE_INT_JOURDAT "fs-journal-data" /* journaling records (file contents also journaled) */

/* (**) numblocks counts sector directories, indirect sector lists and data sectors, but
 * not zero sectors (holes) for all versions alltogether */

/* flags */
#define FSZ_IN_FLAG_HIST     (1<<8)   /* indicates that previous file versions are kept */
#define FSZ_IN_FLAG_CHKSUM   (1<<9)   /* file has content data checksums too */
/* bits 10 to 63 are reserved for future use */

/* (***) logical sector address to data sector translation. These file sizes
 * were calculated with 4096 sector size. That is configurable in the
 * FSZ_SuperBlock if you think 11 sector reads is too much to access
 * data at any arbitrary position in a Yotta magnitude file.
 * Sector directory supports 2^128 LSNs, except when FSZ_IN_FLAG_CHECKSUM is set on the
 * file version, in which case LSNs are limited to "only" 2^96 bits.
 * If even that's not enough, you can use FSZ_SectorList (extents) to store file data, which
 * store contiguous runs of sectors with separated checksum fields. */


/* sector translation part of flags */
#define FSZ_FLAG_TRANSLATION(x) (uint8_t)((x>>0)&0xFF)

/*  data size < sector size - 1024 (3072 bytes)
    FSZ_Inode.sec points to itself.
    the data is included in the inode sector at 1024 (or 2048 if BIGINODE)
    FSZ_Inode.sec -> FSZ_Inode.sec; data  */
#define FSZ_IN_FLAG_INLINE  (0xFF<<0)

/*  data size < sector size (4096)
    The inode points to data sector directly
    FSZ_Inode.sec -> data */
#define FSZ_IN_FLAG_DIRECT   (0<<0)

/*  data size < (sector size - 1024) * sector size / 16 (768k)
    FSZ_Inode.sec points to itself, sector directory inlined.
    with BIGINODE, this format supports 512k mapping.
    FSZ_Inode.sec -> FSZ_Inode.sec; sd -> data */
#define FSZ_IN_FLAG_SD0      (0x7F<<0)

/*  data size < sector size * sector size / 16 (1 M)
    FSZ_Inode.sec points to a sector directory,
    which is a sector with up to 256 sector addresses
    FSZ_Inode.sec -> sd -> data */
#define FSZ_IN_FLAG_SD1      (1<<0)

/*  data size < sector size * sector size / 16 * sector size / 16 (256 M)
    FSZ_Inode.sec points to a sector directory,
    which is a sector with up to 256 sector
    directory addresses, which in turn point
    to 256*256 sector addresses
    FSZ_Inode.sec -> sd -> sd -> data */
#define FSZ_IN_FLAG_SD2      (2<<0)

/*  data size < (64 G)
    FSZ_Inode.sec -> sd -> sd -> sd -> data */
#define FSZ_IN_FLAG_SD3      (3<<0)

/*  data size < (16 T)
    FSZ_Inode.sec -> sd -> sd -> sd -> sd -> data */
#define FSZ_IN_FLAG_SD4      (4<<0)

/*  data size < (4 Peta, equals 4096 Terra)
    FSZ_Inode.sec -> sd -> sd -> sd -> sd -> sd -> data */
#define FSZ_IN_FLAG_SD5      (5<<0)

/*  data size < (1 Exa, equals 1024 Peta)
    FSZ_Inode.sec -> sd -> sd -> sd -> sd -> sd -> sd -> data */
#define FSZ_IN_FLAG_SD6      (6<<0)

/*  data size < (256 Exa)
    FSZ_Inode.sec -> sd -> sd -> sd -> sd -> sd -> sd -> sd -> data */
#define FSZ_IN_FLAG_SD7      (7<<0)

/*  data size < (64 Zetta, equals 65536 Exa) */
#define FSZ_IN_FLAG_SD8      (8<<0)

/*  data size < (16 Yotta, equals 16384 Zetta) */
#define FSZ_IN_FLAG_SD9      (9<<0)

/*  as sector list contains a number of sectors field it's impossible to tell
 *  how big file it can store, so we measure it by the number of file fragments */

/*  inlined sector list ((sector size - 1024) / 32, up to 96 entries)
    FSZ_Inode.sec points to itself, FSZ_SectorList entries inlined.
    FSZ_Inode.sec -> FSZ_Inode.sec; sl -> data */
#define FSZ_IN_FLAG_SECLIST0 (0x80<<0)

/*  normal sector list ((sector size - 1024) * sector size / 16 / 32, up to 24576 entries)
    FSZ_Inode.sec points to a sector list with FSZ_SectorList entries.
    FSZ_Inode.sec -> sl -> data */
#define FSZ_IN_FLAG_SECLIST1 (0x81<<0)

/*  indirect sector list (up to 32768 entries)
    FSZ_Inode.sec points to a sector directory with FSZ_SectorLists
    FSZ_Inode.sec -> sd -> sl -> data */
#define FSZ_IN_FLAG_SECLIST2 (0x82<<0)

/*  double-indirect sector list (up to 8388608 entries)
    FSZ_Inode.sec points to a sector directory pointing to
    sector directories with FSZ_SectorLists
    FSZ_Inode.sec -> sd -> sd -> sl -> data */
#define FSZ_IN_FLAG_SECLIST3 (0x83<<0)

/*  triple-indirect sector list (up to 2147483648 entries)
    FSZ_Inode.sec -> sd -> sd -> sd -> sl -> data */
#define FSZ_IN_FLAG_SECLIST4 (0x84<<0)

/*********************************************************
 *                      Directory                        *
 *********************************************************/
/* first entry is the header. */

/* sizeof = 128 */
typedef struct {
    uint8_t     magic[4];
    uint32_t    checksum;       /* CRC32 of entries from byte 16 to the end */
    uint8_t     display_type;
    uint8_t     sorting_order;
    uint8_t     reserved0[6];
    uint64_t    numentries;
    uint64_t    numentries_hi;
    uint64_t    fid;
    uint64_t    fid_hi;
    uint8_t     reserved[79];
    uint8_t     flags;
} __attribute__((packed)) FSZ_DirEntHeader;

#define FSZ_DIR_MAGIC         "FSDR"
#define FSZ_DIR_FLAG_UNSORTED (1<<0)
#define FSZ_DIR_FLAG_HASHED   (2<<0)

enum {
    FSZ_DIR_DSP_DEFAULT,        /* use global configuration */
    FSZ_DIR_DSP_DETAILED,
    FSZ_DIR_DSP_LIST,
    FSZ_DIR_DSP_ICONS,
    FSZ_DIR_DSP_PREVIEW
};

enum {
    FSZ_DIR_SORT_DEFAULT,       /* use global configuration */
    FSZ_DIR_SORT_NAME_ASC,
    FSZ_DIR_SORT_NAME_DESC,
    FSZ_DIR_SORT_SIZE_ASC,
    FSZ_DIR_SORT_SIZE_DESC,
    FSZ_DIR_SORT_TIME_ASC,
    FSZ_DIR_SORT_TIME_DESC
};

/* directory entries are fixed in size and lexicographically ordered.
 * this means a bit slower writes, but also incredibly faster look ups. */

/* sizeof = 128 */
typedef struct {
    uint64_t      fid;
    uint64_t      fid_hi;
    unsigned char name[112];          /* zero terminated UTF-8 string */
} __attribute__((packed)) FSZ_DirEnt;
/* names must not contain zero, '/' and ';'. If the entry points to a
 * directory i-node, then name must be suffixed by '/' */

/*********************************************************
 *                   Directory union                     *
 *********************************************************/

/* inlined data is a list of zero terminated UTF-8 paths that may contain the
 * '...' joker directory. Terminated by and empty path.
 * Example union for /usr/bin: inlinedata=/bin(zero)/usr/.../bin(zero)(zero)
 */

/*********************************************************
 *                    Search index                       *
 *********************************************************/

/* superblock indexfid points to a directory with type "dir:fs-search-index"
 * that directory contains files, with mime type names, like "imagjpeg/". Names
 * starting with "dir:" or "int:" not allowed. Those directory entries must
 * point to files of type "int:fs-search-index", which is a list of fid and
 * path pairs. Each record starts witha 16 byte fid (to the original i-node),
 * followed by a zero terminated UTF-8 path. */

/*********************************************************
 *                     Meta labels                       *
 *********************************************************/

/* meta labels are stored in two places. Meta label file (pointed by metafid in superblock)
 * records the UTF-8 encoded keys for 16 bit meta ids, the same way as unions. Each key is
 * terminated by a zero and cannot be longer than 255 bytes, and the entire list is terminated
 * by an empty string. Meta label sectors in i-nodes point to the starting logical sector with
 * contiguous meta values in a key id value pairs (with possibly binary values), filled up
 * with zeros to be multiple of sector size.
 */

/* Normally meta labels do not exceed logical sector size. But when they do, the allocation
 * must be careful to allocate contiguous sectors for a meta block. This complicates things
 * a bit when large meta label blocks (>4096) are written, but simplifies a lot on read by
 * eliminating the need of translating LSNs for meta labels file. As meta labels are read more
 * often than written, and usually one meta is smaller than 4096, this is intentional.
 */

/* sizeof = 32 */
typedef struct {
    uint8_t     magic[4];
    uint32_t    checksum;       /* CRC32 of entries from byte 8 to end */
    uint16_t    numentries;     /* number of meta label key-value pairs */
    uint16_t    size;           /* total size of the meta label values in bytes */
    uint32_t    reserved;
    uint64_t    fid;            /* back reference to the i-node */
    uint64_t    fid_hi;
} __attribute__((packed)) FSZ_MetaHeader;

#define FSZ_META_MAGIC      "FSMT"

typedef struct {
    uint16_t    key;
    uint16_t    size;
} __attribute__((packed)) FSZ_MetaLabel;
/* followed by size bytes, the actual data */

/*********************************************************
 *                    Journal data                       *
 *********************************************************/

/* If journal file is specified in SB, then journalhead, journaltail is used to create a circular
 * buffer. Journal file must be allocated with FSZ_IN_FLAG_SECLIST and one extent. Each write
 * transaction contains a header and data sectors. Only meta data is journaled, unless the
 * FSZ_SB_JOURNAL_DATA flag is set in the superblock, when both meta and file data written. */

/* sizeof = 32 */
typedef struct {
    uint8_t     magic[4];
    uint32_t    checksum;       /* CRC32 of entries from byte 8 to end */
    uint64_t    numentries;     /* number of extents written in this transaction */
    uint64_t    transdate;      /* transaction's date and time timestamp */
    uint64_t    reserved;
} __attribute__((packed)) FSZ_JournalTransaction;
/* followed by 32 bytes long FSZ_SectorList entries, padded to logical sector size,
 * followed by the data sectors pointed by those FSZ_SectorList entries */

#define FSZ_JT_MAGIC        "FSTR"

/*********************************************************
 *                     Encryption                        *
 *********************************************************/

/* Encryption can be applied to the disk and to individual files (if enchash is not zero):
 *   enckey=sha256(sha256(password+salt)-4 bytes xored with encrypt[])
 *   if EALG_SHACBC: XOR SHA blocks, very fast, and pretty good cyclic block cipher, uses enckey as iv
 *   if EALG_AESCBC: AES-256-CBC, key=enckey, iv=substr(enckey,enckey[0]&15,16)
 * This way the encrypted disk/file doesn't need to be rewritten when password changes. Also you can
 * re-encrypt the disk without changing the password. The enchash CRC is only used to avoid decryption with
 * bad password. Checksum is stored in the first cipher block to make it harder to decrypt.
 */

#endif /* fsZ.h */
