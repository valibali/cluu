/*
 * mkbootimg/esp.c
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
 * @brief Generate EFI System Partition
 * See https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/efifat.pdf
 *
 */
#include "main.h"
#include "data.h"

char *initrdnames[NUMARCH+1] = { "INITRD", "AARCH64", "X86_64", "RISCV64" };
int nextcluster = 3, lastcluster = 0, bpc, esp_size, esp_bbs = 0;
unsigned char *esp, *data;
uint16_t *fat16_1 = NULL, *fat16_2;
uint32_t *fat32_1 = NULL, *fat32_2;

/**
 * Add a FAT directory entry
 */
unsigned char *esp_adddirent(unsigned char *ptr, char *name, int type, int cluster, int size)
{
    int i, j;
    memset(ptr, ' ', 11);
    if(name[0] == '.') memcpy((char*)ptr, name, strlen(name));
    else
        for(i = j = 0; j < 11 && name[i]; i++, j++) {
            if(name[i] >= 'a' && name[i] <= 'z') ptr[j] = name[i] - ('a' - 'A');
            else if(name[i] == '.') { j = 7; continue; }
            else ptr[j] = name[i];
        }
    ptr[0xB] = type;
    i = (ts->tm_hour << 11) | (ts->tm_min << 5) | (ts->tm_sec/2);
    ptr[0xE] = ptr[0x16] = i & 0xFF; ptr[0xF] = ptr[0x17] = (i >> 8) & 0xFF;
    i = ((ts->tm_year+1900-1980) << 9) | ((ts->tm_mon+1) << 5) | (ts->tm_mday);
    ptr[0x10] = ptr[0x12] = ptr[0x18] = i & 0xFF; ptr[0x11] = ptr[0x13] = ptr[0x19] = (i >> 8) & 0xFF;
    ptr[0x1A] = cluster & 0xFF; ptr[0x1B] = (cluster >> 8) & 0xFF;
    ptr[0x14] = (cluster >> 16) & 0xFF; ptr[0x15] = (cluster >> 24) & 0xFF;
    ptr[0x1C] = size & 0xFF; ptr[0x1D] = (size >> 8) & 0xFF;
    ptr[0x1E] = (size >> 16) & 0xFF; ptr[0x1F] = (size >> 24) & 0xFF;
    return ptr + 32;
}

/**
 * Create a directory
 */
unsigned char *esp_mkdir(unsigned char *ptr, char *directory, int parent)
{
    unsigned char *ptr2 = data + nextcluster * bpc;
    ptr = esp_adddirent(ptr, directory, 0x10, nextcluster, 0);
    if(fat16_1) fat16_1[nextcluster] = fat16_2[nextcluster] = 0xFFFF;
    else fat32_1[nextcluster] = fat32_2[nextcluster] = 0x0FFFFFFF;
    ptr2 = esp_adddirent(ptr2, ".", 0x10, nextcluster, 0);
    ptr2 = esp_adddirent(ptr2, "..", 0x10, parent, 0);
    lastcluster = nextcluster;
    nextcluster++;
    return ptr2;
}

/**
 * Add a file to the boot partition
 */
unsigned char *esp_addfile(unsigned char *ptr, char *name, unsigned char *content, int size)
{
    unsigned char *ptr2 = data + nextcluster * bpc;
    int i;
    ptr = esp_adddirent(ptr, name, 0, nextcluster, size);
    if(content && size) {
        memcpy(ptr2, content, size);
        for(i = 0; i < ((size + bpc-1) & ~(bpc-1)); i += bpc, nextcluster++) {
            if(fat16_1) fat16_1[nextcluster] = fat16_2[nextcluster] = nextcluster+1;
            else fat32_1[nextcluster] = fat32_2[nextcluster] = nextcluster+1;
        }
        if(fat16_1) fat16_1[nextcluster-1] = fat16_2[nextcluster-1] = 0xFFFF;
        else fat32_1[nextcluster-1] = fat32_2[nextcluster-1] = 0x0FFFFFFF;
    }
    return ptr;
}

/**
 * Add a compressed file to the boot partition
 */
unsigned char *esp_addzfile(unsigned char *ptr, char *name, unsigned char *content, int size, unsigned long int len)
{
    unsigned char *buf = malloc(len);
    if(!buf) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    if(uncompress(buf,&len,content,size) == Z_OK && len > 0)
        ptr = esp_addfile(ptr, name, buf, len);
    free(buf);
    return ptr;
}

/**
 * Create EFI System Partition with FAT16 or FAT32
 */
void esp_makepart()
{
    unsigned char *rootdir, *ptr;
    int i = (initrd_size[0] + 2047 + initrd_size[1] + 2047 + 1024*1024-1)/1024/1024 + 3, spf, boot = 0;

    if(boot_size < i) boot_size = i;
    /* we must force 16M at least, because if FAT16 has too few clusters, some UEFI thinks it's FAT12... */
    if(boot_size < 8) boot_size = 8;
    if(boot_fat == 16 && boot_size >= 128) boot_fat = 32;
    /* we must force 128M, because if FAT32 has too few clusters, some UEFI thinks it's FAT16... */
    i = (iso9660 ? 128 : 33);
    if(boot_fat == 32 && boot_size < i) boot_size = i;
    esp_size = boot_size*1024*1024;

    esp = malloc(esp_size);
    if(!esp) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(2); }
    memset(esp, 0, esp_size);
    /* Volume Boot Record */
    memcpy(esp, binary_boot_bin, 512);
    esp[0x1FE]=0x55; esp[0x1FF]=0xAA;
    /* use 4 sectors per cluster to ensure data is always 2048 bytes aligned for ISO9660, that's
     * a good default for FAT16 anyway. For FAT32 we only use 1 sector per cluster if no ISO9660
     * requested and the disk size is small */
    esp[0xC] = 2; esp[0x10] = 2; esp[0x15] = 0xF8; esp[0x1FE] = 0x55; esp[0x1FF] = 0xAA;
    esp[0x18] = 0x20; esp[0x1A] = 0x40;
    i = (esp_size + 511) / 512; if(i < 65536) memcpy(esp + 0x13, &i, 2); else memcpy(esp + 0x20, &i, 4);
    if(boot_fat == 16) {
        esp[0xD] = 4; esp[0xE] = 4; esp[0x12] = 2;
        bpc = esp[0xD] * 512;
        spf = ((esp_size/bpc)*2 + 511) / 512;
        esp[0x16] = spf & 0xFF; esp[0x17] = (spf >> 8) & 0xFF;
        esp[0x24] = 0x80; esp[0x26] = 0x29; esp[0x27] = 0xB0; esp[0x28] = 0x07; esp[0x29] = 0xB0; esp[0x2A] = 0x07;
        memcpy(esp + 0x2B, "EFI System FAT16   ", 19);
        rootdir = esp + (spf*esp[0x10]+esp[0xE]) * 512;
        data = rootdir + ((((esp[0x12]<<8)|esp[0x11])*32 - 4096) & ~2047);
        fat16_1 = (uint16_t*)(&esp[esp[0xE] * 512]);
        fat16_2 = (uint16_t*)(&esp[(esp[0xE]+spf) * 512]);
        fat16_1[0] = fat16_2[0] = 0xFFF8; fat16_1[1] = fat16_2[1] = 0xFFFF;
    } else {
        esp[0xD] = iso9660 || boot_size >= 128 ? 4 : 1; esp[0xE] = 8;
        bpc = esp[0xD] * 512;
        spf = ((esp_size/bpc)*4) / 512 - 8;
        esp[0x24] = spf & 0xFF; esp[0x25] = (spf >> 8) & 0xFF; esp[0x26] = (spf >> 16) & 0xFF; esp[0x27] = (spf >> 24) & 0xFF;
        esp[0x2C] = 2; esp[0x30] = 1; esp[0x32] = 6; esp[0x40] = 0x80;
        esp[0x42] = 0x29; esp[0x43] = 0xB0; esp[0x44] = 0x07; esp[0x45] = 0xB0; esp[0x46] = 0x07;
        memcpy(esp + 0x47, "EFI System FAT32   ", 19);
        memcpy(esp + 0x200, "RRaA", 4); memcpy(esp + 0x3E4, "rrAa", 4);
        for(i = 0; i < 8; i++) esp[0x3E8 + i] = 0xFF;
        esp[0x3FE] = 0x55; esp[0x3FF] = 0xAA;
        rootdir = esp + (spf*esp[0x10]+esp[0xE]) * 512;
        data = rootdir - 2*bpc;
        fat32_1 = (uint32_t*)(&esp[esp[0xE] * 512]);
        fat32_2 = (uint32_t*)(&esp[(esp[0xE]+spf) * 512]);
        fat32_1[0] = fat32_2[0] = fat32_1[2] = fat32_2[2] = 0x0FFFFFF8; fat32_1[1] = fat32_2[1] = 0x0FFFFFFF;
    }
    /* label in root directory */
    rootdir = esp_adddirent(rootdir, ".", 8, 0, 0);
    memcpy(rootdir - 32, "EFI System ", 11);
    /* add contents */
    for(i = 0; i < NUMARCH && initrd_arch[i]; i++)
        boot |= (1 << (initrd_arch[i] - 1));

    /* add loader's directory with config and initrds */
    ptr = esp_mkdir(rootdir, "BOOTBOOT", 0); rootdir += 32;
    ptr = esp_addfile(ptr, "CONFIG", (unsigned char*)config, strlen(config));
    if(!initrd_arch[1]) {
        ptr = esp_addfile(ptr, initrdnames[0], initrd_buf[0], initrd_size[0]);
    } else {
        for(i = 0; i < NUMARCH && initrd_arch[i]; i++) {
            ptr = esp_addfile(ptr, initrdnames[(int)initrd_arch[i]], initrd_buf[i], initrd_size[i]);
        }
    }
    /* add loader code */
    if(boot & (1 << 6)) {
        /* additional platform */
    }
    if(boot & (1 << 5)) {
        /* additional platform */
    }
    if(boot & (1 << 4)) {
        /* additional platform */
    }
    if(boot & (1 << 3)) {
        /* additional platform */
    }
    if(boot & (1 << 2)) {
        /*** Risc-V 64 Microchip Icicle ***/
        /* start and end address has to be added to the GPT too in a special partition */
        bbp_start = ((data + nextcluster * bpc)-esp) / 512;
        rootdir = esp_addzfile(rootdir, "PAYLOAD.BIN", binary_bootboot_rv64, sizeof(binary_bootboot_rv64), sizeof_bootboot_rv64);
        bbp_end = (((data + nextcluster * bpc)-esp) / 512) - 1;
    }
    if(boot & (1 << 1)) {
        /*** x86 PC (BIOS) ***/
        /* start address has to be saved in PMBR too */
        esp_bbs = ((data + nextcluster * bpc)-esp) / 512;
        memcpy(esp + 0x1B0, &esp_bbs, 4);
        rootdir = esp_addzfile(rootdir, "BOOTBOOT.BIN", binary_bootboot_bin, sizeof(binary_bootboot_bin), sizeof_bootboot_bin);
        /*** x86 PC (UEFI) ***/
        ptr = esp_mkdir(rootdir, "EFI", 0); rootdir += 32;
        ptr = esp_mkdir(ptr, "BOOT", lastcluster);
        ptr = esp_addzfile(ptr, "BOOTX64.EFI", binary_bootboot_efi, sizeof(binary_bootboot_efi), sizeof_bootboot_efi);
    }
    if(boot & (1 << 0)) {
        /*** Raspberry Pi ***/
        ptr = esp_addzfile(rootdir, "KERNEL8.IMG", binary_bootboot_img, sizeof(binary_bootboot_img), sizeof_bootboot_img);
        ptr = esp_addzfile(ptr, "BOOTCODE.BIN", binary_bootcode_bin, sizeof(binary_bootcode_bin), sizeof_bootcode_bin);
        ptr = esp_addzfile(ptr, "FIXUP.DAT", binary_fixup_dat, sizeof(binary_fixup_dat), sizeof_fixup_dat);
        ptr = esp_addzfile(ptr, "START.ELF", binary_start_elf, sizeof(binary_start_elf), sizeof_start_elf);
        ptr = esp_addzfile(ptr, "LICENCE.BCM", binary_LICENCE_broadcom, sizeof(binary_LICENCE_broadcom), sizeof_LICENCE_broadcom);
    }
    /* update fields in FS Information Sector */
    if(boot_fat == 32) {
        nextcluster -= 2;
        i = ((esp_size - (spf*esp[0x10]+esp[0xE]) * 512)/bpc) - nextcluster;
        esp[0x3E8] = i & 0xFF; esp[0x3E9] = (i >> 8) & 0xFF;
        esp[0x3EA] = (i >> 16) & 0xFF; esp[0x3EB] = (i >> 24) & 0xFF;
        esp[0x3EC] = nextcluster & 0xFF; esp[0x3ED] = (nextcluster >> 8) & 0xFF;
        esp[0x3EE] = (nextcluster >> 16) & 0xFF; esp[0x3EF] = (nextcluster >> 24) & 0xFF;
        /* copy backup boot sectors */
        memcpy(esp + (esp[0x32]*512), esp, 1024);
    }
}

