/*
 * mkbootimg/gpt.c
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
 * @brief Generate GUID Partitioning Table (and optionally ISO9660 for hybrid images)
 *
 */
#include "main.h"

guid_t efiguid = { 0xC12A7328, 0xF81F, 0x11D2, { 0xBA,0x4B,0x00,0xA0,0xC9,0x3E,0xC9,0x3B} };
guid_t bbpguid = { 0x21686148, 0x6449, 0x6E6F, { 0x74,0x4E,0x65,0x65,0x64,0x45,0x46,0x49} };
unsigned char *gpt = NULL, gpt2[512];
unsigned long int gpt_parts[248];
int np = 0, bbp_start = 0, bbp_end = 0;

/**
 * Set integers in byte arrays
 */
int getint(unsigned char *ptr) { return (unsigned char)ptr[0]+(unsigned char)ptr[1]*256+(unsigned char)ptr[2]*256*256+ptr[3]*256*256*256; }
void setint(int val, unsigned char *ptr) { memcpy(ptr,&val,4); }
void setinte(int val, unsigned char *ptr) { char *v=(char*)&val; memcpy(ptr,&val,4); ptr[4]=v[3]; ptr[5]=v[2]; ptr[6]=v[1]; ptr[7]=v[0]; }

/**
 * Create a GPT
 */
void gpt_maketable()
{
    int i, j, k, gs = 63*512;
    unsigned long int size, ps, total, l;
    unsigned char *iso, *p;
    uint16_t *u;
    char isodate[17], key[64], *tmp, *name;
    guid_t typeguid;
    FILE *f;

    disk_align = disk_align >= 1 ? disk_align * 1024 : 512;
    es = disk_align/512 > 128 ? disk_align/512 : 128;
    esiz = ((esp_size + disk_align - 1) & ~(disk_align - 1))/512;
    tsize = (unsigned long int)disk_size * 1024UL * 1024UL;
    total = (2*es + esiz) * 512;
    memset(gpt_parts, 0, sizeof(gpt_parts));
    for(np = 1; np < 248; np++) {
        /* get type, either fsname or GUID */
        sprintf(key, "partitions.%d.%s", np, "type");
        tmp = json_get(json, key);
        if(!tmp || !*tmp) break;
        getguid(tmp, &typeguid);
        for(i = 0; fsdrv[i].name; i++)
            if(fsdrv[i].type.Data1 && !strcmp(tmp, fsdrv[i].name)) { memcpy(&typeguid, &fsdrv[i].type, sizeof(guid_t)); break; }
        free(tmp);
        /* if there's still no type GUID */
        if(!typeguid.Data1 && !typeguid.Data2 && !typeguid.Data3 && !typeguid.Data4[0]) {
            fprintf(stderr,"mkbootimg: partition #%d %s. %s:\r\n", np+1, lang[ERR_TYPE], lang[ERR_ACCEPTVALUES]);
            for(i = 0; fsdrv[i].name; i++)
                if(fsdrv[i].type.Data1) {
                    fprintf(stderr,"  \"%08X-%04X-%04X-%02X%02X-",fsdrv[i].type.Data1,fsdrv[i].type.Data2,fsdrv[i].type.Data3,
                        fsdrv[i].type.Data4[0],fsdrv[i].type.Data4[1]);
                    for(j = 2; j < 8; j++) fprintf(stderr,"%02X",fsdrv[i].type.Data4[j]);
                    fprintf(stderr,"\" / \"%s\"\r\n",fsdrv[i].name);
                }
            fprintf(stderr,"  ...%s \"%%08X-%%04X-%%04X-%%04X-%%12X\"\r\n",lang[ERR_GUIDFMT]);
            exit(1);
        }
        /* partition's name */
        sprintf(key, "partitions.%d.%s", np, "name");
        tmp = json_get(json, key);
        if(!tmp || !*tmp) { fprintf(stderr,"mkbootimg: partition #%d %s\r\n", np+1, lang[ERR_NONAME]); exit(1); }
        free(tmp);
        /* size and/or image file's size */
        sprintf(key, "partitions.%d.%s", np, "size");
        tmp = json_get(json, key); if(tmp) { size = atoi(tmp) * 1024UL * 1024UL; } else { size = 0; } free(tmp);
        sprintf(key, "partitions.%d.%s", np, "file");
        tmp = json_get(json, key);
        if(!tmp || !*tmp) ps = 0; else {
            f = fopen(tmp, "rb");
            if(!f) { fprintf(stderr,"mkbootimg: partition #%d %s %s\r\n",np+1,lang[ERR_PARTIMG],tmp); exit(1); }
            fseek(f, 0L, SEEK_END);
            ps = ftell(f);
            fclose(f); free(tmp);
        }
        gpt_parts[np] = ((size > ps ? size : ps) + disk_align-1) & ~(disk_align-1);
        total += gpt_parts[np];
    }
    if(total > tsize) tsize = total;

    gpt = malloc(es*512);
    if(!gpt) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(1); }
    memset(gpt,0,es*512);

    /* MBR stage 1 loader */
    if(esp_bbs) {
        esp_bbs += es;
        setint(esp_bbs, esp + 0x1B0);
        /* set it in FAT32 backup too */
        if(!esp[0x16] && !esp[0x17])
            setint(esp_bbs, esp + (esp[0x32]*512) + 0x1B0);
        memcpy(gpt, esp, 3);
        memcpy(gpt + 0x78, esp + 0x78, 0x1B8 - 0x78);
    }
    gpt[0x1FE]=0x55; gpt[0x1FF]=0xAA;
    /* WinNT disk id */
    memcpy(gpt+0x1B8, &diskguid.Data1, 4);
    /* generate PMBR partitioning table */
    j=0x1C0;
    /* MBR, EFI System Partition / boot partition. */
    /* Don't use 0xEF as type, that's just plain stupidity in the spec. There are two firmware:
     * 1) GPT-aware: it will read the ESP from the GPT
     * 2) non-GPT-aware: it won't recognize neither GPT ESP, nor type 0xEF either...
     * So just use ids for plain FAT partitions, that way we have chance for backward compatibility,
     * and also Raspberry Pi doesn't work otherwise. */
    gpt[j-2]=0x80;                              /* bootable flag */
    setint(es+1,gpt+j);                         /* start CHS */
    gpt[j+2]=boot_fat == 16 ? 0xE : 0xC;        /* type, LBA FAT16 (0xE) or FAT32 (0xC) */
    setint(esiz+es,gpt+j+4);                    /* end CHS */
    setint(es,gpt+j+6);                         /* start LBA */
    setint(esiz,gpt+j+10);                      /* number of sectors */
    j+=16;
    /* MBR, protective GPT entry */
    /* according to the spec, it should cover the entire disk, but
     * 1) that's just stupid, you can't do that with larger disks
     * 2) all partitioning tools are compaining about overlaping partitions... */
    setint(1,gpt+j);                            /* start CHS */
    gpt[j+2]=0xEE;                              /* type */
    setint(gs/512+1,gpt+j+4);                   /* end CHS */
    setint(1,gpt+j+6);                          /* start LBA */
    setint(gs/512,gpt+j+10);                    /* number of sectors */
    p = gpt + 512;

    /* GPT header */
    memcpy(p,"EFI PART",8);                     /* magic */
    setint(1,p+10);                             /* revision */
    setint(92,p+12);                            /* size */
    setint(1,p+24);                             /* primary LBA */
    setint(tsize/512UL-1,p+32);                 /* secondary LBA */
    setint(64,p+40);                            /* first usable LBA */
    setint(tsize/512UL-1,p+48);                 /* last usable LBA */
    memcpy(p+56,&diskguid,sizeof(guid_t));      /* disk UUID */
    setint(2,p+72);                             /* partitioning table LBA */
    setint(248,p+80);                           /* number of entries */
    setint(128,p+84);                           /* size of one entry */
    p += 512;

    /* GPT, EFI System Partition (ESP, /boot) */
    l = esiz+es-1;
    memcpy(p, &efiguid, sizeof(guid_t));        /* entry type */
    diskguid.Data1++;
    memcpy(p+16, &diskguid, sizeof(guid_t));    /* partition UUID */
    setint(es,p+32);                            /* start LBA */
    setint(l,p+40);                             /* end LBA */
    name = "EFI System Partition";              /* name */
    for(i = 0; name[i]; i++) p[56+i*2] = name[i];
    p += 128;

    /* BIOS BOOT Partition (needed for Risc-V64 Icicle firmware, not mounted, binary blob) */
    if(bbp_start && bbp_end && bbp_start <= bbp_end) {
        /* it would have been more fortunate if Microchip had choosen its own Microchip boot partition type guid */
        memcpy(p, &bbpguid, sizeof(guid_t));        /* entry type */
        diskguid.Data1++;
        memcpy(p+16, &diskguid, sizeof(guid_t));    /* partition UUID */
        setint(bbp_start,p+32);                     /* start LBA */
        setint(bbp_end,p+40);                       /* end LBA */
        name = "BOOTBOOT RISC-V";                   /* name */
        for(i = 0; name[i]; i++) p[56+i*2] = name[i];
        p += 128;
    }

    /* add user defined partitions */
    for(k = 1; k < np; k++) {
        sprintf(key, "partitions.%d.%s", k, "type");
        tmp = json_get(json, key);
        if(!tmp || !*tmp) break;
        getguid(tmp, &typeguid);
        for(i = 0; fsdrv[i].name; i++)
            if(fsdrv[i].type.Data1 && !strcmp(tmp, fsdrv[i].name)) { memcpy(&typeguid, &fsdrv[i].type, sizeof(guid_t)); break; }
        free(tmp);
        memcpy(p, &typeguid, sizeof(guid_t));       /* entry type */
        diskguid.Data1++;
        memcpy(p+16, &diskguid, sizeof(guid_t));    /* partition UUID */
        setint(l+1,p+32);                           /* start LBA */
        l += gpt_parts[k] / 512;
        setint(l,p+40);                             /* end LBA */
        sprintf(key, "partitions.%d.%s", k, "name"); tmp = name = json_get(json, key);
        u = (uint16_t*)(p+56);
        for(i = 0; i < 35 && *name; name++, i++) {  /* name, utf8 to unicode16 */
            u[i] = *name;
            if((*name & 128) != 0) {
                if(!(*name & 32)) { u[i] = ((*name & 0x1F)<<6)|(name[1] & 0x3F); name += 1; } else
                if(!(*name & 16)) { u[i] = ((*name & 0xF)<<12)|((name[1] & 0x3F)<<6)|(name[2] & 0x3F); name += 2; } else
                if(!(*name & 8)) { u[i] = ((*name & 0x7)<<18)|((name[1] & 0x3F)<<12)|((name[2] & 0x3F)<<6)|(name[3] & 0x3F); name += 3; }
                else u[i] = 0;
            }
        }
        free(tmp);
        p += 128;
    }

    /* calculate checksums */
    /* partitioning table */
    setint(crc32(0,gpt+1024,gpt[512+80]*128),gpt+512+88);
    /* header */
    i=getint(gpt+512+12);   /* size of header */
    setint(crc32(0,gpt+512,i),gpt+512+16);
    /* secondary header */
    memcpy(gpt2, gpt+512, 512);
    i=getint(gpt+512+32);
    setint(getint(gpt+512+24),gpt2+32);         /* secondary lba */
    setint(i,gpt2+24);                          /* primary lba */

    setint((tsize-gs)/512,gpt2+72);             /* partition lba */
    i=getint(gpt+512+12);                       /* size of header */
    setint(0,gpt2+16);                          /* calculate with zero */
    setint(crc32(0,gpt2,i),gpt2+16);

    /* ISO9660 cdrom image part */
    if(iso9660) {
        /* from the UEFI spec section 12.3.2.1 ISO-9660 and El Torito
          "...A Platform ID of 0xEF indicates an EFI System Partition. The Platform ID is in either the Section
          Header Entry or the Validation Entry of the Booting Catalog as defined by the “El Torito”
          specification. EFI differs from “El Torito” “no emulation” mode in that it does not load the “no
          emulation” image into memory and jump to it. EFI interprets the “no emulation” image as an EFI
          system partition."
         * so we must record the ESP in the Boot Catalog, that's how UEFI locates it */
        if(esp_bbs%4!=0) {
            /* this should never happen, but better check */
            fprintf(stderr,"mkbootimg: %s (LBA %d, offs %x)\n",lang[ERR_ST2ALIGN], esp_bbs, esp_bbs*512);
            exit(3);
        }
        sprintf((char*)&isodate, "%04d%02d%02d%02d%02d%02d00",
            ts->tm_year+1900,ts->tm_mon+1,ts->tm_mday,ts->tm_hour,ts->tm_min,ts->tm_sec);
        iso = gpt + 16*2048;
        /* 16th sector: Primary Volume Descriptor */
        iso[0]=1;   /* Header ID */
        memcpy(&iso[1], "CD001", 5);
        iso[6]=1;   /* version */
        for(i=8;i<72;i++) iso[i]=' ';
        memcpy(&iso[40], "BOOTBOOT_CD", 11);   /* Volume Identifier */
        setinte((65536+esp_size+2047)/2048, &iso[80]);
        iso[120]=iso[123]=1;        /* Volume Set Size */
        iso[124]=iso[127]=1;        /* Volume Sequence Number */
        iso[129]=iso[130]=8;        /* logical blocksize (0x800) */
        iso[156]=0x22;              /* root directory recordsize */
        setinte(20, &iso[158]);     /* root directory LBA */
        setinte(2048, &iso[166]);   /* root directory size */
        iso[174]=ts->tm_year;       /* root directory create date */
        iso[175]=ts->tm_mon+1;
        iso[176]=ts->tm_mday;
        iso[177]=ts->tm_hour;
        iso[178]=ts->tm_min;
        iso[179]=ts->tm_sec;
        iso[180]=0;                 /* timezone UTC (GMT) */
        iso[181]=2;                 /* root directory flags (0=hidden,1=directory) */
        iso[184]=1;                 /* root directory number */
        iso[188]=1;                 /* root directory filename length */
        for(i=190;i<813;i++) iso[i]=' ';    /* Volume data */
        memcpy(&iso[318], "BOOTBOOT <HTTPS://GITLAB.COM/BZTSRC/BOOTBOOT>", 45);
        memcpy(&iso[446], "MKBOOTIMG", 9);
        memcpy(&iso[574], "BOOTABLE OS", 11);
        for(i=702;i<813;i++) iso[i]=' ';    /* file descriptors */
        memcpy(&iso[813], &isodate, 16);    /* volume create date */
        memcpy(&iso[830], &isodate, 16);    /* volume modify date */
        for(i=847;i<863;i++) iso[i]='0';    /* volume expiration date */
        for(i=864;i<880;i++) iso[i]='0';    /* volume shown date */
        iso[881]=1;                         /* filestructure version */
        for(i=883;i<1395;i++) iso[i]=' ';   /* file descriptors */
        /* 17th sector: Boot Record Descriptor */
        iso[2048]=0;    /* Header ID */
        memcpy(&iso[2049], "CD001", 5);
        iso[2054]=1;    /* version */
        memcpy(&iso[2055], "EL TORITO SPECIFICATION", 23);
        setinte(19, &iso[2048+71]);         /* Boot Catalog LBA */
        /* 18th sector: Volume Descritor Terminator */
        iso[4096]=0xFF; /* Header ID */
        memcpy(&iso[4097], "CD001", 5);
        iso[4102]=1;    /* version */
        /* 19th sector: Boot Catalog */
        /* --- BIOS, Validation Entry + Initial/Default Entry --- */
        iso[6144]=1;    /* Header ID, Validation Entry */
        iso[6145]=0;    /* Platform 80x86 */
        iso[6172]=0xaa; /* magic bytes */
        iso[6173]=0x55;
        iso[6174]=0x55;
        iso[6175]=0xaa;
        iso[6176]=0x88; /* Bootable, Initial/Default Entry */
        iso[6182]=4;    /* Sector Count */
        setint(es/4, &iso[6184]);  /* Boot Record LBA */
        /* --- UEFI, Final Section Header Entry + Section Entry --- */
        iso[6208]=0x91; /* Header ID, Final Section Header Entry */
        iso[6209]=0xEF; /* Platform EFI */
        iso[6210]=1;    /* Number of entries */
        iso[6240]=0x88; /* Bootable, Section Entry */
        setint(es/4, &iso[6248]);  /* ESP Start LBA */
        /* 20th sector: Root Directory */
        /* . */
        iso[8192]=0x21 + 1;          /* recordsize */
        setinte(20, &iso[8194]);     /* LBA */
        setinte(2048, &iso[8202]);   /* size */
        iso[8210]=ts->tm_year;       /* date */
        iso[8211]=ts->tm_mon+1;
        iso[8212]=ts->tm_mday;
        iso[8213]=ts->tm_hour;
        iso[8214]=ts->tm_min;
        iso[8215]=ts->tm_sec;
        iso[8216]=0;                 /* timezone UTC (GMT) */
        iso[8217]=2;                 /* flags (0=hidden,1=directory) */
        iso[8220]=1;                 /* serial */
        iso[8224]=1;                 /* filename length */
        iso[8225]=0;                 /* filename '.' */
        /* .. */
        iso[8226]=0x21 + 1;          /* recordsize */
        setinte(20, &iso[8228]);     /* LBA */
        setinte(2048, &iso[8236]);   /* size */
        iso[8244]=ts->tm_year;       /* date */
        iso[8245]=ts->tm_mon+1;
        iso[8246]=ts->tm_mday;
        iso[8247]=ts->tm_hour;
        iso[8248]=ts->tm_min;
        iso[8249]=ts->tm_sec;
        iso[8250]=0;                 /* timezone UTC (GMT) */
        iso[8251]=2;                 /* flags (0=hidden,1=directory) */
        iso[8254]=1;                 /* serial */
        iso[8258]=1;                 /* filename length */
        iso[8259]='\001';            /* filename '..' */
        /* BOOTBOOT.TXT */
        iso[8260]=0x21+14;           /* recordsize */
        setinte(21, &iso[8262]);     /* LBA */
        setinte(133, &iso[8270]);    /* size */
        iso[8278]=ts->tm_year;       /* date */
        iso[8279]=ts->tm_mon+1;
        iso[8280]=ts->tm_mday;
        iso[8281]=ts->tm_hour;
        iso[8282]=ts->tm_min;
        iso[8283]=ts->tm_sec;
        iso[8284]=0;                 /* timezone UTC (GMT) */
        iso[8285]=0;                 /* flags (0=hidden,1=directory) */
        iso[8288]=1;                 /* serial */
        iso[8292]=14;                /* filename length */
        memcpy(&iso[8293], "BOOTBOOT.TXT;1", 14);
        /* 21th sector: contents of BOOTBOOT.TXT */
        memcpy(&iso[10240], "BOOTBOOT hybrid GPT / CDROM Image\r\n\r\nBootable as\r\n"
            " - CDROM (El Torito, UEFI)\r\n"
            " - USB stick (BIOS, UEFI)\r\n"
            " - SD card (Raspberry Pi 3+)", 133);
    }
}
