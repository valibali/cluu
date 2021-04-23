/*
 * mkbootimg/main.c
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
 * @brief Bootable image creator main header
 *
 */
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <dirent.h>
#include <time.h>
#include <sys/stat.h>
#include "lang.h"
#include "zlib.h"

#define NUMARCH 3
#define MAXPATH 1024

#ifndef S_ISLNK
#define S_ISLNK(x) (0)
#endif

/*** ELF64 defines and structs ***/
#define ELFMAG      "\177ELF"
#define SELFMAG     4
#define EI_CLASS    4       /* File class byte index */
#define ELFCLASS64  2       /* 64-bit objects */
#define EI_DATA     5       /* Data encoding byte index */
#define ELFDATA2LSB 1       /* 2's complement, little endian */
#define PT_LOAD     1       /* Loadable program segment */
#define EM_X86_64   62      /* AMD x86-64 architecture */
#define EM_AARCH64  183     /* ARM aarch64 architecture */
#define EM_RISCV    243     /* RISC-V */

typedef struct
{
  unsigned char e_ident[16];/* Magic number and other info */
  uint16_t    e_type;         /* Object file type */
  uint16_t    e_machine;      /* Architecture */
  uint32_t    e_version;      /* Object file version */
  uint64_t    e_entry;        /* Entry point virtual address */
  uint64_t    e_phoff;        /* Program header table file offset */
  uint64_t    e_shoff;        /* Section header table file offset */
  uint32_t    e_flags;        /* Processor-specific flags */
  uint16_t    e_ehsize;       /* ELF header size in bytes */
  uint16_t    e_phentsize;    /* Program header table entry size */
  uint16_t    e_phnum;        /* Program header table entry count */
  uint16_t    e_shentsize;    /* Section header table entry size */
  uint16_t    e_shnum;        /* Section header table entry count */
  uint16_t    e_shstrndx;     /* Section header string table index */
} Elf64_Ehdr;

typedef struct
{
  uint32_t    p_type;         /* Segment type */
  uint32_t    p_flags;        /* Segment flags */
  uint64_t    p_offset;       /* Segment file offset */
  uint64_t    p_vaddr;        /* Segment virtual address */
  uint64_t    p_paddr;        /* Segment physical address */
  uint64_t    p_filesz;       /* Segment size in file */
  uint64_t    p_memsz;        /* Segment size in memory */
  uint64_t    p_align;        /* Segment alignment */
} Elf64_Phdr;

typedef struct
{
  uint32_t    sh_name;        /* Section name (string tbl index) */
  uint32_t    sh_type;        /* Section type */
  uint64_t    sh_flags;       /* Section flags */
  uint64_t    sh_addr;        /* Section virtual addr at execution */
  uint64_t    sh_offset;      /* Section file offset */
  uint64_t    sh_size;        /* Section size in bytes */
  uint32_t    sh_link;        /* Link to another section */
  uint32_t    sh_info;        /* Additional section information */
  uint64_t    sh_addralign;   /* Section alignment */
  uint64_t    sh_entsize;     /* Entry size if section holds table */
} Elf64_Shdr;

typedef struct
{
  uint32_t    st_name;        /* Symbol name (string tbl index) */
  uint8_t     st_info;        /* Symbol type and binding */
  uint8_t     st_other;       /* Symbol visibility */
  uint16_t    st_shndx;       /* Section index */
  uint64_t    st_value;       /* Symbol value */
  uint64_t    st_size;        /* Symbol size */
} Elf64_Sym;

/*** PE32+ defines and structs ***/
#define MZ_MAGIC                    0x5a4d      /* "MZ" */
#define PE_MAGIC                    0x00004550  /* "PE\0\0" */
#define IMAGE_FILE_MACHINE_AMD64    0x8664      /* AMD x86_64 architecture */
#define IMAGE_FILE_MACHINE_ARM64    0xaa64      /* ARM aarch64 architecture */
#define IMAGE_FILE_MACHINE_RISCV64  0x5064      /* RISC-V riscv64 architecture */
#define PE_OPT_MAGIC_PE32PLUS       0x020b      /* PE32+ format */
typedef struct
{
  uint16_t magic;         /* MZ magic */
  uint16_t reserved[29];  /* reserved */
  uint32_t peaddr;        /* address of pe header */
} mz_hdr;

typedef struct {
  uint32_t magic;         /* PE magic */
  uint16_t machine;       /* machine type */
  uint16_t sections;      /* number of sections */
  uint32_t timestamp;     /* time_t */
  uint32_t sym_table;     /* symbol table offset */
  uint32_t numsym;        /* number of symbols */
  uint16_t opt_hdr_size;  /* size of optional header */
  uint16_t flags;         /* flags */
  uint16_t file_type;     /* file type, PE32PLUS magic */
  uint8_t  ld_major;      /* linker major version */
  uint8_t  ld_minor;      /* linker minor version */
  uint32_t text_size;     /* size of text section(s) */
  uint32_t data_size;     /* size of data section(s) */
  uint32_t bss_size;      /* size of bss section(s) */
  int32_t entry_point;    /* file offset of entry point */
  int32_t code_base;      /* relative code addr in ram */
} pe_hdr;

typedef struct {
  uint32_t iszero;        /* if this is not zero, then iszero+nameoffs gives UTF-8 string */
  uint32_t nameoffs;
  int32_t value;          /* value of the symbol */
  uint16_t section;       /* section it belongs to */
  uint16_t type;          /* symbol type */
  uint8_t storclass;      /* storage class */
  uint8_t auxsyms;        /* number of pe_sym records following */
} pe_sym;

typedef struct {
    uint32_t Data1;
    uint16_t Data2;
    uint16_t Data3;
    uint8_t  Data4[8];
} __attribute__((packed)) guid_t;

typedef struct {
    guid_t type;
    guid_t guid;
    uint64_t start;
    uint64_t last;
    uint64_t attrib;
    uint16_t name[36];
} __attribute__((packed)) gpt_t;

typedef void (*initrd_open)(gpt_t *gpt_entry);
typedef void (*initrd_add)(struct stat *st, char *name, unsigned char *content, int size);
typedef void (*initrd_close)();

typedef struct {
    char *name;
    guid_t type;
    initrd_open open;
    initrd_add add;
    initrd_close close;
} fsdrv_t;

extern time_t t;
extern struct tm *ts;
extern guid_t diskguid;
extern char *json, *config, *kernelname, *initrd_dir[NUMARCH], initrd_arch[NUMARCH];
extern int fs_len, fs_no, initrd_size[NUMARCH], initrd_gzip, boot_size, boot_fat, disk_size, esp_size, esp_bbs;
extern int iso9660, skipbytes, np, bbp_start, bbp_end;
extern unsigned char *esp, *gpt, gpt2[512], *fs_base, *initrd_buf[NUMARCH];
extern unsigned long int tsize, es, esiz, disk_align, gpt_parts[248];
extern fsdrv_t fsdrv[];
extern initrd_open rd_open;
extern initrd_add rd_add;
extern initrd_close rd_close;

extern long int read_size;
unsigned char* readfileall(char *file);
unsigned int gethex(char *ptr, int len);
void getguid(char *ptr, guid_t *guid);
void parsedir(char *directory, int parent);
void initrdcompress();
void initrduncompress();
char *json_get(const char *jsonstr, char *key);
unsigned char * stbi_zlib_compress(unsigned char *data, int data_len, int *out_len, int quality);
void esp_makepart();
void gpt_maketable();
void img_write(char *fn);
uint32_t crc32_calc(unsigned char *start,int length);
