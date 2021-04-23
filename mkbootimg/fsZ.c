/*
 * mkbootimg/fsZ.c
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
 * @brief FS/Z file system driver
 *
 */
#include "main.h"
#include "fsZ.h"

int fsz_secsize = FSZ_SECSIZE, fsz_max = 0;
unsigned char fsz_emptysec[FSZ_SECSIZE] = {0};

/* private functions */
static int fsz_direntcmp(const void *a, const void *b)
{
    return strcmp((char *)((FSZ_DirEnt *)a)->name,(char *)((FSZ_DirEnt *)b)->name);
}

int fsz_add_inode(char *filetype, char *mimetype)
{
    unsigned int i,j=!strcmp(filetype,FSZ_FILETYPE_SYMLINK)||!strcmp(filetype,FSZ_FILETYPE_UNION)?fsz_secsize-1024:36;
    FSZ_Inode *in;
    FSZ_DirEntHeader *hdr;
    if(fsz_max && fs_len+fsz_secsize > fsz_max) {
        fprintf(stderr,"mkbootimg: partition #%d %s\n", fs_no, lang[ERR_TOOBIG]);
        exit(1);
    }
    fs_base=realloc(fs_base,fs_len+fsz_secsize);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    memset(fs_base+fs_len,0,fsz_secsize);
    in=(FSZ_Inode *)(fs_base+fs_len);
    memcpy(in->magic,FSZ_IN_MAGIC,4);
    memcpy((char*)&in->owner,"root",5);
    in->owner.access=FSZ_READ|FSZ_WRITE|FSZ_DELETE|(filetype && (
        !strcmp(filetype,FSZ_FILETYPE_DIR) || !strcmp(filetype,FSZ_FILETYPE_UNION))? FSZ_EXEC : 0);
    if(filetype!=NULL){
        i=strlen(filetype);
        memcpy(in->filetype,filetype,i>4?4:i);
        if(!strcmp(filetype,FSZ_FILETYPE_DIR)){
            hdr=(FSZ_DirEntHeader *)(in->data.small.inlinedata);
            in->sec=hdr->fid=fs_len/fsz_secsize;
            in->flags=FSZ_IN_FLAG_INLINE;
            in->size=sizeof(FSZ_DirEntHeader);
            memcpy(in->data.small.inlinedata,FSZ_DIR_MAGIC,4);
            hdr->checksum=crc32_calc((unsigned char*)hdr + 16, in->size - 16);
        }
    }
    if(mimetype!=NULL){
        if(!strcmp(filetype,FSZ_FILETYPE_UNION)){
            for(i=1;i<j && !(mimetype[i-1]==0 && mimetype[i]==0);i++);
            i++;
        } else {
            i=strlen(mimetype);
        }
        memcpy(j==36?in->mimetype:in->data.small.inlinedata,mimetype,i>j?j:i);
        if(j!=36)
            in->size=i;
    }
    in->changedate=in->createdate=t * 1000000;
    in->modifydate=t * 1000000;
    in->checksum=crc32_calc(in->filetype,1016);
    fs_len+=fsz_secsize;
    return fs_len/fsz_secsize-1;
}

void fsz_link_inode(int inode, char *path, int toinode)
{
    unsigned int ns=0,cnt=0,l=strlen(path);
    FSZ_DirEntHeader *hdr;
    FSZ_DirEnt *ent;
    FSZ_Inode *in, *in2;
    if(toinode==0)
        toinode=((FSZ_SuperBlock *)fs_base)->rootdirfid;
    hdr=(FSZ_DirEntHeader *)(fs_base+toinode*fsz_secsize+1024);
    ent=(FSZ_DirEnt *)hdr; ent++;
    while(path[ns]!='/'&&path[ns]!=0) ns++;
    while(ent->fid!=0 && cnt<(unsigned int)((fsz_secsize-1024)/128-1)) {
        if(!strncmp((char *)(ent->name),path,ns+1)) {
            fsz_link_inode(inode,path+ns+1,ent->fid);
            return;
        }
        ent++; cnt++;
    }
    in=((FSZ_Inode *)(fs_base+toinode*fsz_secsize));
    in2=((FSZ_Inode *)(fs_base+inode*fsz_secsize));
    ent->fid=inode;
    if(l > 110) l = 110;
    memcpy(ent->name,path,l);
    if(!strncmp((char *)(((FSZ_Inode *)(fs_base+inode*fsz_secsize))->filetype),FSZ_FILETYPE_DIR,4)){
        ent->name[l]='/';
    }
    /* the format can hold 2^127 directory entries, but we only implement directories embedded in inodes here, up to 23 */
    if(hdr->numentries >= (fsz_secsize - 1024 - sizeof(FSZ_DirEntHeader)) / sizeof(FSZ_DirEnt)) {
        fprintf(stderr,"mkbootimg: partition #%d %s: %s\n", fs_no, lang[ERR_TOOMANY], path); exit(1);
    }
    hdr->numentries++;
    in->modifydate=t * 1000000;
    in->size+=sizeof(FSZ_DirEnt);
    qsort((char*)hdr+sizeof(FSZ_DirEntHeader), hdr->numentries, sizeof(FSZ_DirEnt), fsz_direntcmp);
    hdr->checksum=crc32_calc((unsigned char*)hdr + 16, in->size - 16);
    in->checksum=crc32_calc(in->filetype,1016);
    in2->numlinks++;
    in2->checksum=crc32_calc(in2->filetype,1016);
}

void fsz_add_file(char *name, unsigned char *data, unsigned long int size)
{
    FSZ_Inode *in;
    unsigned char *ptr;
    long int i,j,k,l,s=((size+fsz_secsize-1)/fsz_secsize)*fsz_secsize;
    int inode=fsz_add_inode(data[0]==0x55 && data[1]==0xAA &&
               data[3]==0xE9 && data[8]=='B' &&
               data[12]=='B'?"boot":"application","octet-stream");
    if(fsz_max && fs_len+fsz_secsize+s > fsz_max) {
        fprintf(stderr,"mkbootimg: partition #%d %s: %s\n", fs_no, lang[ERR_TOOBIG], name);
        exit(1);
    }
    fs_base=realloc(fs_base,fs_len+fsz_secsize+s);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    memset(fs_base+fs_len,0,fsz_secsize);
    in=(FSZ_Inode *)(fs_base+inode*fsz_secsize);
    in->changedate=t * 1000000;
    in->modifydate=t * 1000000;
    in->size=size;
    if(size<=(unsigned long int)fsz_secsize-1024) {
        in->sec=inode;
        in->flags=FSZ_IN_FLAG_INLINE;
        in->numblocks=0;
        memcpy(in->data.small.inlinedata,data,size);
        s=0;
    } else {
        in->sec=fs_len/fsz_secsize;
        if(size>(unsigned long int)fsz_secsize) {
            j=s/fsz_secsize;
            if(j*16>fsz_secsize){ fprintf(stderr,"mkbootimg: partition #%d %s: %s\n", fs_no, lang[ERR_TOOBIG], name); exit(1); }
            if(j*16<=fsz_secsize-1024) {
                ptr=(unsigned char*)&in->data.small.inlinedata;
                in->flags=FSZ_IN_FLAG_SD0;
                in->numblocks=0;
                l=0;
            } else {
                ptr=fs_base+size;
                in->flags=FSZ_IN_FLAG_SD1;
                in->numblocks=1;
                l=1;
            }
            k=inode+1+l;
            for(i=0;i<j;i++){
                /* no spare blocks allowed in initrd, there we must save a sector full of zeros */
                if(!fsz_max || memcmp(data+i*fsz_secsize,fsz_emptysec,fsz_secsize)) {
                    memcpy(ptr,&k,4);
                    memcpy(fs_base+size+(i+l)*fsz_secsize,data+i*fsz_secsize,
                        (unsigned long int)(i+l)*fsz_secsize>size?size%fsz_secsize:(unsigned long int)fsz_secsize);
                    k++;
                    in->numblocks++;
                } else {
                    s-=fsz_secsize;
                }
                ptr+=16;
            }
            if(in->flags==FSZ_IN_FLAG_SD1)
                size+=fsz_secsize;
        } else {
            in->flags=FSZ_IN_FLAG_DIRECT;
            if(memcmp(data,fsz_emptysec,fsz_secsize)) {
                in->numblocks=1;
                memcpy(fs_base+fs_len,data,size);
            } else {
                in->sec=0;
                in->numblocks=0;
            }
        }
    }
    if(!strncmp((char*)data+1,"ELF",3) || !strncmp((char*)data,"OS/Z",4) || !strncmp((char*)data,"CSBC",4) ||
        !strncmp((char*)data,"\000asm",4))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"executable",10);in->owner.access|=FSZ_EXEC;}
    if(!strcmp(name+strlen(name)-3,".so"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"sharedlib",9);}
    else
    if(!strcmp(name+strlen(name)-2,".h")||
       !strcmp(name+strlen(name)-2,".c")||
       !strcmp(name+strlen(name)-3,".md")||
       !strcmp(name+strlen(name)-4,".txt")||
       !strcmp(name+strlen(name)-5,".conf")
      ) {memset(in->mimetype,0,36);memcpy(in->mimetype,"plain",5);
         memcpy(in->filetype,"text",4);
        }
    else
    if(!strcmp(name+strlen(name)-3,".sh"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"shellscript",11);
         memcpy(in->filetype,"text",4);in->owner.access|=FSZ_EXEC;
        }
    else
    if(!strcmp(name+strlen(name)-4,".htm")||
       !strcmp(name+strlen(name)-5,".html")
      )
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"html",4);
         memcpy(in->filetype,"text",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".css"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"stylesheet",10);
         memcpy(in->filetype,"text",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".svg"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"svg",3);
         memcpy(in->filetype,"imag",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".gif"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"gif",3);
         memcpy(in->filetype,"imag",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".png"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"png",3);
         memcpy(in->filetype,"imag",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".jpg"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"jpeg",4);
         memcpy(in->filetype,"imag",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".bmp"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"bitmap",6);
         memcpy(in->filetype,"imag",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".sfn"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"ssfont",6);
         memcpy(in->filetype,"font",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".psf"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"pc-screen-font",14);
         memcpy(in->filetype,"font",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".ttf"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"sfnt",4);
         memcpy(in->filetype,"font",4);
        }
    else
    if(!strcmp(name+strlen(name)-4,".m3d"))
        {memset(in->mimetype,0,36);memcpy(in->mimetype,"3d-model",8);
         memcpy(in->filetype,data[1]=='d' ? "text" : "mode",4);
        }
    else {
        j=1; for(i=0;i<read_size;i++) if(data[i]<9) { j=0; break; }
        if(j) {
         memset(in->mimetype,0,36);memcpy(in->mimetype,"plain",5);
         memcpy(in->filetype,"text",4);
        }
    }
    in->checksum=crc32_calc(in->filetype,1016);
    fs_len+=s;
    fsz_link_inode(inode,name,0);
}

/*** mkbootimg interface ***/
void fsz_open(gpt_t *gpt_entry)
{
    FSZ_SuperBlock *sb;
    fs_base = realloc(fs_base, 2*fsz_secsize);
    if(!fs_base) { fprintf(stderr,"mkbootimg: %s\r\n", lang[ERR_MEM]); exit(1); }
    memset(fs_base,0,2*fsz_secsize);
    sb=(FSZ_SuperBlock *)(fs_base);
    memcpy(sb->magic,FSZ_MAGIC,4);
    sb->version_major=FSZ_VERSION_MAJOR;
    sb->version_minor=FSZ_VERSION_MINOR;
    sb->logsec=fsz_secsize==2048?0:(fsz_secsize==4096?1:2);
    sb->maxmounts=255;
    sb->currmounts=0;
    sb->createdate=sb->lastmountdate=sb->lastumountdate=t * 1000000;
    if(gpt_entry) {
        memcpy(&sb->uuid, &gpt_entry->guid, 16);
        fsz_max = (gpt_entry->last - gpt_entry->start + 1) * 512;
        sb->numsec = fsz_max / fsz_secsize;
    } else {
        memcpy(&sb->uuid, (void*)&diskguid, sizeof(guid_t));
        sb->uuid[15]--;
        fsz_max = 0;
    }
    memcpy(sb->magic2,FSZ_MAGIC,4);
    fs_len = fsz_secsize;
    /* don't use sb after this point because add inode will realloc */
    ((FSZ_SuperBlock *)(fs_base))->rootdirfid = fsz_add_inode(FSZ_FILETYPE_DIR,FSZ_MIMETYPE_DIR_ROOT);
    ((FSZ_Inode*)(fs_base+fsz_secsize))->numlinks++;
}

void fsz_add(struct stat *st, char *name, unsigned char *content, int size)
{
    int i;
    char *fn = strrchr(name, '/');
    if(!fn) fn = name; else fn++;
    if(!strcmp(fn, ".") || !strcmp(fn, "..")) return;
    if(S_ISDIR(st->st_mode)) {
        i=fsz_add_inode(FSZ_FILETYPE_DIR,NULL);
        fsz_link_inode(i,name,0);
    } else
    if(S_ISREG(st->st_mode)) {
        fsz_add_file(name,content,size);
    } else
    if(S_ISLNK(st->st_mode) && content) {
        i=fsz_add_inode(FSZ_FILETYPE_SYMLINK,(char*)content);
        fsz_link_inode(i,name,0);
    }
}

void fsz_close()
{
    FSZ_SuperBlock *sb=(FSZ_SuperBlock *)(fs_base);
    if(!sb) return;
    if(!sb->numsec) sb->numsec = fs_len / fsz_secsize;
    sb->freesec = fs_len / fsz_secsize;
    sb->checksum = crc32_calc((unsigned char *)&sb->magic,508);
}
