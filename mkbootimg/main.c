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
 * @brief Bootable image creator main file
 *
 */
#include "main.h"
#include "fs.h"

#ifdef __WIN32__
#include <windows.h>
#define ISHH(x) ((((x)>>30)&0xFFFFFFFF)==0xFFFFFFFF)
#else
#define ISHH(x) (((x)>>30)==0x3FFFFFFFF)
#endif
#if defined(MACOSX) || __WORDSIZE == 32
#define LL "ll"
#else
#define LL "l"
#endif

extern const char deflate_copyright[];
char **lang = NULL;

/**
 * Get language dictionary
 */
char **getlang(int *argc, char **argv)
{
    char *loc = NULL;
    int i;
#ifdef __WIN32__
    /* see https://docs.microsoft.com/en-us/windows/win32/intl/language-identifier-constants-and-strings */
    switch((GetUserDefaultLangID() /* GetUserDefaultUILanguage() */) & 0xFF) {
        case 0x01: loc = "ar"; break;   case 0x02: loc = "bg"; break;
        case 0x03: loc = "ca"; break;   case 0x04: loc = "zh"; break;
        case 0x05: loc = "cs"; break;   case 0x06: loc = "da"; break;
        case 0x07: loc = "de"; break;   case 0x08: loc = "el"; break;
        case 0x0A: loc = "es"; break;   case 0x0B: loc = "fi"; break;
        case 0x0C: loc = "fr"; break;   case 0x0D: loc = "he"; break;
        case 0x0E: loc = "hu"; break;   case 0x0F: loc = "is"; break;
        case 0x10: loc = "it"; break;   case 0x11: loc = "jp"; break;
        case 0x12: loc = "ko"; break;   case 0x13: loc = "nl"; break;
        case 0x14: loc = "no"; break;   case 0x15: loc = "pl"; break;
        case 0x16: loc = "pt"; break;   case 0x17: loc = "rm"; break;
        case 0x18: loc = "ro"; break;   case 0x19: loc = "ru"; break;
        case 0x1A: loc = "hr"; break;   case 0x1B: loc = "sk"; break;
        case 0x1C: loc = "sq"; break;   case 0x1D: loc = "sv"; break;
        case 0x1E: loc = "th"; break;   case 0x1F: loc = "tr"; break;
        case 0x20: loc = "ur"; break;   case 0x21: loc = "id"; break;
        case 0x22: loc = "uk"; break;   case 0x23: loc = "be"; break;
        case 0x24: loc = "sl"; break;   case 0x25: loc = "et"; break;
        case 0x26: loc = "lv"; break;   case 0x27: loc = "lt"; break;
        case 0x29: loc = "fa"; break;   case 0x2A: loc = "vi"; break;
        case 0x2B: loc = "hy"; break;   case 0x2D: loc = "bq"; break;
        case 0x2F: loc = "mk"; break;   case 0x36: loc = "af"; break;
        case 0x37: loc = "ka"; break;   case 0x38: loc = "fo"; break;
        case 0x39: loc = "hi"; break;   case 0x3A: loc = "mt"; break;
        case 0x3C: loc = "gd"; break;   case 0x3E: loc = "ms"; break;
        case 0x3F: loc = "kk"; break;   case 0x40: loc = "ky"; break;
        case 0x45: loc = "bn"; break;   case 0x47: loc = "gu"; break;
        case 0x4D: loc = "as"; break;   case 0x4E: loc = "mr"; break;
        case 0x4F: loc = "sa"; break;   case 0x53: loc = "kh"; break;
        case 0x54: loc = "lo"; break;   case 0x56: loc = "gl"; break;
        case 0x5E: loc = "am"; break;   case 0x62: loc = "fy"; break;
        case 0x68: loc = "ha"; break;   case 0x6D: loc = "ba"; break;
        case 0x6E: loc = "lb"; break;   case 0x6F: loc = "kl"; break;
        case 0x7E: loc = "br"; break;   case 0x92: loc = "ku"; break;
        case 0x09: default: loc = "en"; break;
    }
#endif
    if(!loc) loc = getenv("LANG");
    if(!loc) loc = "en";
    if(*argc > 2 && !strcmp(argv[1], "-l")) { loc = argv[2]; (*argc) -= 2; argv += 2; }
    for(i = 0; i < NUMLANGS; i++)
        if(!strncmp(loc, dict[i][0], strlen(dict[i][0]))) break;
    if(i >= NUMLANGS) { i = 0; loc = "en"; }
    lang = &dict[i][1];
    return argv;
}

/**
 * Parse the mkbootimg json configuration file
 */
void parsejson(char *json)
{
    char *tmp, key[64];
    int i;
    tmp = json_get(json, "diskguid"); getguid(tmp, &diskguid); free(tmp);
    tmp = json_get(json, "disksize"); if(tmp) { disk_size = atoi(tmp); } free(tmp);
    tmp = json_get(json, "align"); if(tmp) { disk_align = atoi(tmp); } free(tmp);
    memset(initrd_dir, 0, NUMARCH*sizeof(void*));
    memset(initrd_buf, 0, NUMARCH*sizeof(void*));
    for(i = 0; i < NUMARCH; i++) {
        sprintf(key, "initrd.file.%d", i);
        tmp = json_get(json, key);
        if(!i && (!tmp || !*tmp)) tmp = json_get(json, "initrd.file");
        if(tmp && *tmp) {
            initrd_buf[i] = readfileall(tmp);
            initrd_size[i] = read_size;
            if(!initrd_buf[i] || !read_size) { fprintf(stderr,"mkbootimg: %s %s\r\n",lang[ERR_INITRDIMG],tmp); exit(1); }
            free(tmp);
        } else {
            sprintf(key, "initrd.directory.%d", i);
            initrd_dir[i] = json_get(json, key);
            if(!i && !initrd_dir[i]) initrd_dir[i] = json_get(json, "initrd.directory");
        }
        if(!initrd_dir[i] && !initrd_buf[i]) break;
    }
    if((!initrd_dir[0] || !initrd_dir[0][0]) && !initrd_buf[0]) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_NOINITRD]); exit(1); }
    if(initrd_dir[0]) {
        tmp = json_get(json, "initrd.type");
        if(!tmp || !*tmp) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_NOINITRDTYPE]); exit(1); }
        if(strcmp(tmp, "fat16") && strcmp(tmp, "fat32"))
            for(i = 0; fsdrv[i].name && fsdrv[i].add; i++)
                if(!strcmp(tmp, fsdrv[i].name)) { rd_open = fsdrv[i].open; rd_add = fsdrv[i].add; rd_close = fsdrv[i].close; break; }
        if(!rd_add) {
            fprintf(stderr,"mkbootimg: %s %s. %s:", lang[ERR_BADINITRDTYPE],tmp,lang[ERR_ACCEPTVALUES]);
            for(i = 0; fsdrv[i].name && fsdrv[i].add; i++) fprintf(stderr,"%s %s",i ? "," : "",fsdrv[i].name);
            fprintf(stderr,"\r\n");
            exit(1);
        }
        free(tmp);
    }
    tmp = json_get(json, "initrd.gzip");
    if(tmp && tmp[0] != '1' && tmp[0] != 't' && tmp[0] != 'y') initrd_gzip = 0;
    free(tmp);
    tmp = json_get(json, "config");
    if(tmp && *tmp) {
        config = (char*)readfileall(tmp);
        if(!config || !*config) { fprintf(stderr,"mkbootimg: %s %s\r\n",lang[ERR_NOCONF],tmp); exit(1); }
        if(read_size > 4095) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_BIGCONF]); exit(1); }
    }
    free(tmp);
    tmp = json_get(json, "iso9660"); if(tmp && (*tmp=='1' || *tmp=='t' || *tmp=='y')) { iso9660 = 1; } free(tmp);
    tmp = json_get(json, "partitions.0.type");
    if(!tmp || !*tmp) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_NOPART]); exit(1); }
    if(tmp && !memcmp(tmp, "fat32", 5)) boot_fat = 32;
    free(tmp);
    tmp = json_get(json, "partitions.0.size");
    if(!tmp || !*tmp) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_NOPARTSIZE]); exit(1); }
    boot_size = atoi(tmp); free(tmp); if(boot_size < 8) boot_size = 8;
    if(!diskguid.Data1) diskguid.Data1 = crc32(0,(uint8_t*)&t, sizeof(time_t)) ^ 0x08040201;
    if(!diskguid.Data2 && !diskguid.Data3) {
        ((uint32_t*)&diskguid)[1] = crc32(0,(uint8_t*)&diskguid.Data1, 4);
        ((uint32_t*)&diskguid)[2] = crc32(0,(uint8_t*)&diskguid.Data2, 4) ^ (unsigned long int)t;
        ((uint32_t*)&diskguid)[3] = crc32(0,(uint8_t*)&diskguid.Data3, 4);
    }
}

/**
 * Parse the BOOTBOOT configuration file
 */
void parseconfig()
{
    char *ptr = config, *e;
    while(ptr && *ptr) {
        if(ptr[0]==' '||ptr[0]=='\t'||ptr[0]=='\r'||ptr[0]=='\n') { ptr++; continue; }
        if((ptr[0]=='/'&&ptr[1]=='/')||ptr[0]=='#') { while(ptr[0]!=0 && ptr[0]!='\r' && ptr[0]!='\n') ptr++; }
        if(ptr[0]=='/'&&ptr[1]=='*') { ptr+=2; while(ptr[0]!=0 && ptr[-1]!='*' && ptr[0]!='/') ptr++; }
        if(!memcmp(ptr, "kernel=", 7)) {
            ptr += 7; for(e = ptr; *e && *e != '\r' && *e != '\n'; e++);
            kernelname = malloc(e - ptr + 1);
            if(!kernelname) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
            memcpy(kernelname, ptr, e - ptr); kernelname[e - ptr] = 0;
            break;
        }
        ptr++;
    }
    if(!kernelname || !*kernelname) {
        kernelname = malloc(10);
        if(!kernelname) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
        strcpy(kernelname, "sys/core");
    }
}

/**
 * Parse the ELF or PE kernel executable
 */
void parsekernel(int idx, unsigned char *data, int v)
{
    Elf64_Ehdr *ehdr;
    Elf64_Phdr *phdr;
    Elf64_Shdr *shdr, *strt, *sym_sh = NULL, *str_sh = NULL;
    Elf64_Sym *sym = NULL, *s;
    pe_hdr *pehdr;
    pe_sym *ps;
    uint32_t i, n = 0, bss = 0, strsz = 0, syment = 0, ma, fa;
    uint64_t core_ptr = 0, core_size = 0, core_addr = 0, entrypoint = 0, mm_addr = 0, fb_addr = 0, bb_addr = 0, env_addr = 0;
    uint64_t initstack = 0;
    char *strtable, *name;
    ehdr=(Elf64_Ehdr *)(data);
    pehdr=(pe_hdr*)(data + ((mz_hdr*)(data))->peaddr);
    /* do not translate stdout, it might be parsed by scripts. Only translate stderr */
    if(v) printf("File format:  ");
    if((!memcmp(ehdr->e_ident,ELFMAG,SELFMAG)||!memcmp(ehdr->e_ident,"OS/Z",4)) &&
        ehdr->e_ident[EI_CLASS]==ELFCLASS64 && ehdr->e_ident[EI_DATA]==ELFDATA2LSB) {
        if(v) printf("ELF64\r\nArchitecture: %s\r\n", ehdr->e_machine==EM_AARCH64 ? "AArch64" : (ehdr->e_machine==EM_X86_64 ?
            "x86_64" : (ehdr->e_machine==EM_RISCV ? "riscv64" : "invalid")));
        if(ehdr->e_machine == EM_AARCH64) { ma = 2*1024*1024-1; fa = 4095; initrd_arch[idx] = 1; } else
        if(ehdr->e_machine == EM_X86_64)  { ma = 4095; fa = 2*1024*1024-1; initrd_arch[idx] = 2; } else
        if(ehdr->e_machine == EM_RISCV)   { ma = 4095; fa = 2*1024*1024-1; initrd_arch[idx] = 3; } else
        { fprintf(stderr,"mkbootimg: %s. %s: e_machine 62, 183, 243.\r\n",lang[ERR_BADARCH],lang[ERR_ACCEPTVALUES]); exit(1); }
        phdr=(Elf64_Phdr *)((uint8_t *)ehdr+ehdr->e_phoff);
        for(i=0;i<ehdr->e_phnum;i++){
            if(phdr->p_type==PT_LOAD) {
                n++;
                core_size = phdr->p_filesz + (ehdr->e_type==3?0x4000:0);
                bss = phdr->p_memsz - core_size;
                core_ptr = phdr->p_offset;
                core_addr = phdr->p_vaddr;
                entrypoint = ehdr->e_entry;
                /* these are just warnings, hopefully not a problem, but better to be fixed in the kernel linker script */
                if(v) {
                    if(phdr->p_vaddr != phdr->p_paddr)
                        fprintf(stderr,"mkbootimg: phdr #%d p_vaddr %016" LL "x != p_paddr %016" LL "x ???\r\n",n,phdr->p_vaddr,phdr->p_paddr);
                    if(phdr->p_align > 4096)
                        fprintf(stderr,"mkbootimg: phdr #%d %s (p_align %" LL "d)\r\n",n,lang[ERR_PAGEALIGN],phdr->p_align);
                }
                break;
            }
            phdr=(Elf64_Phdr *)((uint8_t *)phdr+ehdr->e_phentsize);
        }
        if(n != 1) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MORESEG]); exit(1); }
        if(v) printf("Entry point:  %016" LL "x ", entrypoint);
        if(entrypoint < core_addr || entrypoint > core_addr+core_size)
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_BADENTRYP]); exit(1); }
        if(ehdr->e_shoff > 0) {
            shdr = (Elf64_Shdr *)((uint8_t *)ehdr + ehdr->e_shoff);
            strt = (Elf64_Shdr *)((uint8_t *)shdr+(uint64_t)ehdr->e_shstrndx*(uint64_t)ehdr->e_shentsize);
            strtable = (char *)ehdr + strt->sh_offset;
            for(i = 0; i < ehdr->e_shnum; i++){
                /* checking shdr->sh_type is not enough, there can be multiple SHT_STRTAB records... */
                if(!memcmp(strtable + shdr->sh_name, ".symtab", 8)) sym_sh = shdr;
                if(!memcmp(strtable + shdr->sh_name, ".strtab", 8)) str_sh = shdr;
                shdr = (Elf64_Shdr *)((uint8_t *)shdr + ehdr->e_shentsize);
            }
            if(str_sh && sym_sh) {
                strtable = (char *)ehdr + str_sh->sh_offset; strsz = str_sh->sh_size;
                sym = (Elf64_Sym *)((uint8_t*)ehdr + sym_sh->sh_offset); syment = sym_sh->sh_entsize;
                if(str_sh->sh_offset && strsz > 0 && sym_sh->sh_offset && syment > 0)
                    for(s = sym, i = 0; i<(strtable-(char*)sym)/syment && s->st_name < strsz; i++, s++) {
                        if(!memcmp(strtable + s->st_name, "bootboot", 9)) bb_addr = s->st_value;
                        if(!memcmp(strtable + s->st_name, "environment", 12)) env_addr = s->st_value;
                        if(!memcmp(strtable + s->st_name, "mmio", 4)) mm_addr = s->st_value;
                        if(!memcmp(strtable + s->st_name, "fb", 3)) fb_addr = s->st_value;
                        if(!memcmp(strtable + s->st_name, "initstack", 10)) initstack = s->st_value;
                    }
            }
        }
    } else
    if(((mz_hdr*)(data))->magic==MZ_MAGIC && ((mz_hdr*)(data))->peaddr<65536 && pehdr->magic == PE_MAGIC &&
        pehdr->file_type == PE_OPT_MAGIC_PE32PLUS) {
        if(v) printf("PE32+\r\nArchitecture: %s\r\n", pehdr->machine == IMAGE_FILE_MACHINE_ARM64 ? "AArch64" : (
            pehdr->machine == IMAGE_FILE_MACHINE_AMD64 ? "x86_64" : (
            pehdr->machine == IMAGE_FILE_MACHINE_RISCV64 ? "riscv64" : "invalid")));
        if(pehdr->machine == IMAGE_FILE_MACHINE_ARM64) { ma = 2*1024*1024-1; fa = 4095; initrd_arch[idx] = 1; } else
        if(pehdr->machine == IMAGE_FILE_MACHINE_AMD64) { ma = 4095; fa = 2*1024*1024-1; initrd_arch[idx] = 2; } else
        if(pehdr->machine == IMAGE_FILE_MACHINE_RISCV64){ma = 4095; fa = 2*1024*1024-1; initrd_arch[idx] = 3; } else
        { fprintf(stderr,"mkbootimg: %s. %s: pe_hdr.machine 0x8664, 0xAA64, 0x5064\r\n",lang[ERR_BADARCH],lang[ERR_ACCEPTVALUES]); exit(1); }
        core_size = (pehdr->entry_point-pehdr->code_base) + pehdr->text_size + pehdr->data_size;
        bss = pehdr->bss_size;
        core_addr = (int64_t)pehdr->code_base;
        entrypoint = (int64_t)pehdr->entry_point;
        if(v) printf("Entry point:  %016" LL "x ", entrypoint);
        if(entrypoint < core_addr || entrypoint > core_addr+pehdr->text_size)
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_BADENTRYP]); exit(1); }
        if(pehdr->sym_table > 0 && pehdr->numsym > 0) {
            strtable = (char *)pehdr + pehdr->sym_table + pehdr->numsym * 18 + 4;
            for(i = 0; i < pehdr->numsym; i++) {
                ps = (pe_sym*)((uint8_t *)pehdr + pehdr->sym_table + i * 18);
                name = !ps->iszero ? (char*)&ps->iszero : strtable + ps->nameoffs;
                if(!memcmp(name, "bootboot", 9)) bb_addr = (int64_t)ps->value;
                if(!memcmp(name, "environment", 12)) env_addr = (int64_t)ps->value;
                if(!memcmp(name, "mmio", 4)) mm_addr = (int64_t)ps->value;
                if(!memcmp(name, "fb", 3)) fb_addr = (int64_t)ps->value;
                if(!memcmp(name, "initstack", 10)) initstack = (int64_t)ps->value;
                i += ps->auxsyms;
            }
        }
    } else {
        if(v) printf("unknown\r\n");
        fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_INVALIDEXE]);
        exit(1);
    }
    if(v) printf("OK\r\n");
    if(mm_addr) {
        if(v) printf("mmio:         %016" LL "x ", mm_addr);
        if(!ISHH(mm_addr)) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: mmio %s\r\n",lang[ERR_BADADDR]); exit(1); }
        if(mm_addr & ma) { if(v) {   printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: mmio ");fprintf(stderr,lang[ERR_BADALIGN],ma+1);fprintf(stderr,"\r\n"); exit(1); }
        if(v) printf("OK\r\n");
    }
    if(fb_addr) {
        if(v) printf("fb:           %016" LL "x ", fb_addr);
        if(!ISHH(fb_addr)) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: fb %s\r\n",lang[ERR_BADALIGN]); exit(1); }
        if(fb_addr & fa) { if(v) {   printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: fb ");fprintf(stderr,lang[ERR_BADALIGN],fa+1);fprintf(stderr,"\r\n"); exit(1); }
        if((fb_addr >= mm_addr && fb_addr < mm_addr + 16*1024*1024) || (fb_addr + 16*1024*1024 > mm_addr && fb_addr + 16*1024*1024 <= mm_addr + 16*1024*1024))
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: mmio/fb %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
        if(v) printf("OK\r\n");
    }
    if(bb_addr) {
        if(v) printf("bootboot:     %016" LL "x ", bb_addr);
        if(!ISHH(bb_addr)) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: bootboot %s\r\n",lang[ERR_BADADDR]); exit(1); }
        if(bb_addr & 4095) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: bootboot %s\r\n",lang[ERR_PAGEALIGN]); exit(1); }
        if((bb_addr >= mm_addr && bb_addr < mm_addr + 16*1024*1024) || (bb_addr + 4096 > mm_addr && bb_addr + 4096 <= mm_addr + 16*1024*1024))
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: mmio/bootboot %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
        if((bb_addr >= fb_addr && bb_addr < fb_addr + 16*1024*1024) || (bb_addr + 4096 > fb_addr && bb_addr + 4096 <= fb_addr + 16*1024*1024))
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: fb/bootboot %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
        if(v) printf("OK\r\n");
    }
    if(env_addr) {
        if(v) printf("environment:  %016" LL "x ", env_addr);
        if(!ISHH(env_addr)) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: environment %s\r\n",lang[ERR_BADADDR]); exit(1); }
        if(env_addr & 4095) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: environment %s\r\n",lang[ERR_PAGEALIGN]); exit(1); }
        if((env_addr >= mm_addr && env_addr < mm_addr + 16*1024*1024) || (env_addr + 4096 > mm_addr && env_addr + 4096 <= mm_addr + 16*1024*1024))
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: mmio/environment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
        if((env_addr >= fb_addr && env_addr < fb_addr + 16*1024*1024) || (env_addr + 4096 > fb_addr && env_addr + 4096 <= fb_addr + 16*1024*1024))
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: fb/enviroment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
        if(env_addr == bb_addr)
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: bootboot/enviroment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
        if(v) printf("OK\r\n");
    }
    if(initstack) {
        if(v) printf("initstack:    %016" LL "x ", initstack);
        if(initstack != 1024 && initstack != 2048 && initstack != 4096 && initstack != 8192 && initstack != 16384)
            { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: initstack %s\r\n",lang[ERR_BADSIZE]); exit(1); }
        if(v) printf("OK\r\n");
    }
    if(v) printf("Load segment: %016" LL "x size %" LL "dK offs %" LL "x ", core_addr, (core_size + bss + 1024)/1024, core_ptr);
    if(!ISHH(core_addr)) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: segment %s\r\n",lang[ERR_BADADDR]); exit(1); }
    if(core_addr & 4095) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: segment %s\r\n",lang[ERR_PAGEALIGN]); exit(1); }
    if(core_size + bss > 16 * 1024 * 1024) { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: segment %s\r\n",lang[ERR_BIGSEG]); exit(1); }
    if((mm_addr >= core_addr && mm_addr < core_addr + core_size) || (mm_addr + 16*1024*1024 > core_addr && mm_addr + 16*1024*1024 <= core_addr + core_size))
        { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: mmio/segment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
    if((fb_addr >= core_addr && fb_addr < core_addr + core_size) || (fb_addr + 16*1024*1024 > core_addr && fb_addr + 16*1024*1024 <= core_addr + core_size))
        { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: fb/segment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
    /* we check for the entrypoint as the lower boundary, because it's okay if the segment is at 0xfffffffe00000 */
    if((bb_addr >= entrypoint && bb_addr < core_addr + core_size) || (bb_addr + 4096 > entrypoint && bb_addr + 4096 <= core_addr + core_size))
        { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: bootboot/segment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
    if((env_addr >= entrypoint && env_addr < core_addr + core_size) || (env_addr + 4096 > entrypoint && env_addr + 4096 <= core_addr + core_size))
        { if(v) { printf("invalid\r\n"); } fprintf(stderr,"mkbootimg: environment/segment %s\r\n",lang[ERR_ADDRCOL]); exit(1); }
    if(v) {
        if(!mm_addr && !fb_addr && !bb_addr && !env_addr)
            printf("OK\r\nComplies with BOOTBOOT Protocol Level 1, %s\r\n",lang[STATADDR]);
        else
            printf("OK\r\nComplies with BOOTBOOT Protocol Level %s2, %s\r\n",
            (!mm_addr || (mm_addr&0xFFFFFFFF)==0xf8000000) && (!fb_addr || (fb_addr&0xFFFFFFFF)==0xfc000000) &&
            (!bb_addr || (bb_addr&0xFFFFFFFF)==0xffe00000) && (!env_addr || (env_addr&0xFFFFFFFF)==0xffe01000) &&
            ((core_addr&0xFFFFFFFF)==0xffe02000) ? "1 and " : "", lang[DYNADDR]);
    }
}

/**
 * Create a ROM image of the initrd
 */
void makerom()
{
    int i, size;
    unsigned char *buf, c=0;
    FILE *f;

    size=((initrd_size[0]+32+511)/512)*512;
    if(!initrd_buf[0] || size < 1) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_NOINITRD]); exit(1); }
    buf=(unsigned char*)malloc(size+1);
    if(!buf) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(buf, 0, size+1);
    /* Option ROM header */
    buf[0]=0x55; buf[1]=0xAA; buf[2]=(initrd_size[0]+32+511)/512;
    /* asm "xor ax,ax; retf" */
    buf[3]=0x31; buf[4]=0xC0; buf[5]=0xCB;
    /* identifier, size and data */
    memcpy(buf+8,"INITRD",6);
    memcpy(buf+16,&initrd_size[0],4);
    memcpy(buf+32,initrd_buf[0],initrd_size[0]);
    /* checksum */
    for(i=0;i<size;i++) c+=buf[i];
    buf[6]=(unsigned char)((int)(256-c));
    /* write out */
    f=fopen("initrd.rom","wb");
    if(!f) { fprintf(stderr,"mkbootimg: %s %s\r\n", lang[ERR_WRITE], "initrd.rom"); exit(3); }
    fwrite(buf,size,1,f);
    fclose(f);
    printf("mkbootimg: %s %s.\r\n", "initrd.rom", lang[SAVED]);
}

/**
 * Generate an initrd ROM image into a Flashmap image area (section, partition, range whatever)
 */
int flashmapadd(char *file)
{
    unsigned char *data=NULL, *desc;
    FILE *f;
    unsigned int size=0,bs=((initrd_size[0]+511)/512)*512;
    /* see if file exists and contains a Flashmap */
    if(!file || !*file) return 0;
    f=fopen(file,"r");
    if(!f) return 0;
    fseek(f,0L,SEEK_END);
    size=(unsigned int)ftell(f);
    fseek(f,0L,SEEK_SET);
    data=(unsigned char*)malloc(size + bs);
    if(!data) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    data[0] = 0; fread(data,size,1,f);
    fclose(f);
    if(memcmp(data, "__FMAP__", 8)) { free(data); return 0; }
    if(!initrd_buf[0] || bs < 1) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_NOINITRD]); exit(1); }
    /* add a new or replace the last partition descriptor */
    desc = data + 0x38 + data[0x36] * 42;
    if(!memcmp(desc - 34, "INITRD", 7)) desc -= 42; else data[0x36]++;
    size = (*((unsigned int*)(desc - 42)) + *((unsigned int*)(desc - 38)) + 4095) & ~4095;
    memset(desc, 0, 42);
    memcpy(desc + 0, &size, 4);
    memcpy(desc + 4, &bs, 4);
    memcpy(desc + 8, "INITRD", 6);
    memcpy(data + size,initrd_buf[0],initrd_size[0]);
    if(initrd_size[0] < (int)bs) memset(data + size + initrd_size[0], 0, bs - initrd_size[0]);
    size += bs; *((unsigned int*)(data + 0x12)) = *((unsigned int*)(data + 0x3c)) = size;
    /* write out */
    f=fopen(file,"wb");
    if(!f) { fprintf(stderr,"mkbootimg: %s %s\r\n", lang[ERR_WRITE], file); exit(3); }
    fwrite(data,size,1,f);
    fclose(f);
    printf("mkbootimg: %s %s.\r\n", file, lang[SAVED]);
    return 1;
}

/**
 * Main function
 */
int main(int argc, char **argv)
{
    Elf64_Ehdr *ehdr;
    pe_hdr *pehdr;
    int i, j;
    unsigned char *data;
    char kfn[32768];
    FILE *f;
    argv = getlang(&argc, argv);
    if(argc < 3 || argv[1]==NULL || argv[2] == NULL || !strcmp(argv[1],"help")) {
        printf( "BOOTBOOT mkbootimg utility - bztsrc@gitlab\r\n BOOTBOOT Copyright (c) bzt MIT "
                "https://gitlab.com/bztsrc/bootboot\r\n%s\r\n"
                " Raspbery Pi Firmware Copyright (c) Broadcom Corp, Raspberry Pi (Trading) Ltd\r\n\r\n%s\r\n"
                "%s.\r\n\r\n",
                deflate_copyright,lang[HELP1],lang[HELP2]);
        printf( "%s:\r\n"
                "  ./mkbootimg check <kernel elf / pe>\r\n"
                "  ./mkbootimg <%s> initrd.rom\r\n"
                "  ./mkbootimg <%s> bootpart.bin\r\n"
                "  ./mkbootimg <%s> <%s>\r\n\r\n",lang[HELP3],lang[HELP4],
                lang[HELP4],lang[HELP4],lang[HELP5]);
        printf( "%s:\n"
                "  ./mkbootimg check mykernel/c/mykernel.x86_64.elf\r\n"
                "  ./mkbootimg myos.json initrd.rom\r\n"
                "  ./mkbootimg myos.json bootpart.bin\r\n"
                "  ./mkbootimg myos.json myos.img\r\n",
                lang[HELP6]);
        return 0;
    }
    if(!strcmp(argv[1], "check")) {
        data = readfileall(argv[2]);
        if(!data || read_size < 16) { fprintf(stderr,"mkbootimg: %s %s\r\n",lang[ERR_KRNL],argv[2]); exit(1); }
        parsekernel(0, data, 1);
    } else {
        t = time(NULL);
        ts = gmtime(&t);
        memset(kfn, 0, sizeof(kfn)); /* <- make valgrind happy with sprintf */
        json = (char*)readfileall(argv[1]);
        if(!json || !*json) { fprintf(stderr,"mkbootimg: %s %s\r\n",lang[ERR_JSON],argv[1]); exit(1); }
        parsejson(json);
        parseconfig();
        for(i = 0; i < NUMARCH; i++)
            if(initrd_dir[i]) {
                sprintf(kfn, "%s/%s", initrd_dir[i], kernelname);
                data = readfileall(kfn);
                if(!data || read_size < 16) { fprintf(stderr,"mkbootimg: %s %s\r\n",lang[ERR_KRNL],kfn); exit(1); }
                if(!memcmp(data + 54, "FAT1", 4) || !memcmp(data + 82, "FAT3", 4))
                    { fprintf(stderr,"mkbootimg: %s %s\r\n", lang[ERR_BADINITRDTYPE],"FAT"); exit(1); }
                parsekernel(i, data, 0);
                free(data);
                skipbytes = strlen(initrd_dir[i]) + 1;
                fs_base = NULL; fs_len = 0; fs_no = 0;
                if(rd_open) (*rd_open)(NULL);
                parsedir(initrd_dir[i], 0);
                if(rd_close) (*rd_close)();
                initrdcompress();
                initrd_buf[i] = fs_base;
                initrd_size[i] = fs_len;
                free(initrd_dir[i]);
            } else
            if(initrd_buf[i]) {
                fs_base = initrd_buf[i]; fs_len = initrd_size[i];
                if(initrd_buf[i][0] == 0x1f && initrd_buf[i][1] == 0x8b) {
                    initrduncompress(); initrd_buf[i] = fs_base; initrd_size[i] = fs_len; }
                for(j = 0, kfn[0] = 0; j < fs_len - 512; j++) {
                    ehdr=(Elf64_Ehdr *)(fs_base + j);
                    pehdr=(pe_hdr*)(fs_base + j + ((mz_hdr*)(fs_base + j))->peaddr);
                    if(((!memcmp(ehdr->e_ident,ELFMAG,SELFMAG)||!memcmp(ehdr->e_ident,"OS/Z",4)) &&
                        ehdr->e_ident[EI_CLASS]==ELFCLASS64 && ehdr->e_ident[EI_DATA]==ELFDATA2LSB) ||
                        (((mz_hdr*)(fs_base + j))->magic==MZ_MAGIC && ((mz_hdr*)(fs_base + j))->peaddr<65536 &&
                        pehdr->magic == PE_MAGIC && pehdr->file_type == PE_OPT_MAGIC_PE32PLUS)) {
                            parsekernel(i, fs_base + j, 0);
                            kfn[0] = 1;
                            break;
                        }
                }
                if(!kfn[0]) { fprintf(stderr,"mkbootimg: %s initrd #%d\r\n",lang[ERR_LOCKRNL],i+1); exit(1); }
                if(initrd_gzip) { initrdcompress(); initrd_buf[i] = fs_base; initrd_size[i] = fs_len; }
            } else
                break;
        if(initrd_arch[1] && initrd_arch[1] == initrd_arch[0]) { initrd_size[1] = 0; initrd_arch[1] = 0; }
        if(!strcmp(argv[2], "initrd.rom")) makerom(); else
        if(!strcmp(argv[2], "initrd.bin")) {
            /* write out */
            f=fopen("initrd.bin","wb");
            if(!f) { fprintf(stderr,"mkbootimg: %s %s\r\n", lang[ERR_WRITE], "initrd.bin"); exit(3); }
            fwrite(initrd_buf[0],initrd_size[0],1,f);
            fclose(f);
            printf("mkbootimg: %s %s.\r\n", "initrd.bin", lang[SAVED]);
        } else if(!flashmapadd(argv[2])) {
            esp_makepart();
            if(!strcmp(argv[2], "bootpart.bin")) {
                /* write out */
                f=fopen("bootpart.bin","wb");
                if(!f) { fprintf(stderr,"mkbootimg: %s %s\r\n", lang[ERR_WRITE], "bootpart.bin"); exit(3); }
                fwrite(esp,esp_size,1,f);
                fclose(f);
                printf("mkbootimg: %s %s.\r\n", "bootpart.bin", lang[SAVED]);
            } else {
                gpt_maketable();
                img_write(argv[2]);
                free(gpt);
            }
            free(esp);
        }
        free(kernelname);
        free(initrd_buf[0]);
        if(initrd_buf[1]) free(initrd_buf[1]);
        if(initrd_buf[2]) free(initrd_buf[2]);
        if(config) free(config);
        free(json);
    }
    return 0;
}
