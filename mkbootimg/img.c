/*
 * mkbootimg/img.c
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
 * @brief Write disk image to file
 *
 */
#include "main.h"

/**
 * Assemble and write out disk image
 */
void img_write(char *fn)
{
    FILE *f, *d;
    int i, j, n, lastpercent, k;
    char key[64], *tmp, *dir, *buf;
    unsigned long int size, pos;
    size_t s;
    time_t c = 0;

    buf = malloc(1024*1024);
    if(!buf) { fprintf(stderr,"mkbootimg: %s\r\n",lang[ERR_MEM]); exit(2); }

    f=fopen(fn,"wb");
    if(!f) { fprintf(stderr,"mkbootimg: %s %s\n", lang[ERR_WRITE],fn); exit(3); }
    /* write out primary GPT table (and optional ISO9660 header) */
    fwrite(gpt,es*512,1,f);
    /* write out ESP */
    fwrite(esp,esp_size,1,f);
    fseek(f,(es+esiz)*512,SEEK_SET);
    /* write out other partitions */
    for(k = 1; k < np; k++) {
        size = 0;
        sprintf(key, "partitions.%d.%s", k, "file");
        tmp = json_get(json, key);
        if(tmp && *tmp) {
            d = fopen(tmp, "rb");
            free(tmp);
            if(d) {
                while((s = fread(buf, 1, 1024*1024, d)) != 0) {
                    fwrite(buf, 1, s, f);
                    size += s;
                    if(c > t + 1) {
                        pos = ftell(f);
                        n = pos * 100L / (tsize + 1);
                        if(n != lastpercent) {
                            lastpercent = n;
                            printf("\rmkbootimg: %s [",lang[WRITING]);
                            for(i = 0; i < 20; i++) printf(i < n/5 ? "#" : " ");
                            printf("] %3d%% ", n);
                            fflush(stdout);
                        }
                    } else
                        time(&c);
                }
                fclose(d);
            }
        } else {
            sprintf(key, "partitions.%d.%s", k, "directory");
            dir = json_get(json, key);
            if(dir && *dir) {
                fs_base = NULL; fs_len = 0; fs_no = k + 1;
                sprintf(key, "partitions.%d.%s", k, "driver");
                tmp = json_get(json, key);
                if(!tmp || !*tmp) {
                    sprintf(key, "partitions.%d.%s", k, "type");
                    tmp = json_get(json, key);
                }
                if(tmp && *tmp) {
                    rd_open = NULL; rd_add = NULL; rd_close = NULL;
                    for(i = 0; fsdrv[i].name && fsdrv[i].add; i++)
                        if(!strcmp(tmp, fsdrv[i].name)) { rd_open = fsdrv[i].open; rd_add = fsdrv[i].add; rd_close = fsdrv[i].close; break; }
                    free(tmp);
                    if(rd_add) {
                        skipbytes = strlen(dir) + 1;
                        if(rd_open) (*rd_open)((gpt_t*)(gpt + 1024 + k * 128));
                        parsedir(dir, 0);
                        if(rd_close) (*rd_close)();
                    } else {
                        fprintf(stderr,"mkbootimg: partition #%d %s. %s:\r\n", np+1, lang[ERR_TYPE], lang[ERR_ACCEPTVALUES]);
                        for(i = 0; fsdrv[i].name; i++)
                            if(fsdrv[i].add) {
                                fprintf(stderr,"  \"%08X-%04X-%04X-%02X%02X-",fsdrv[i].type.Data1,fsdrv[i].type.Data2,
                                    fsdrv[i].type.Data3, fsdrv[i].type.Data4[0],fsdrv[i].type.Data4[1]);
                                for(j = 2; j < 8; j++) fprintf(stderr,"%02X",fsdrv[i].type.Data4[j]);
                                fprintf(stderr,"\" / \"%s\"\r\n",fsdrv[i].name);
                            }
                        exit(1);
                    }
                }
                free(dir);
                if(fs_base && fs_len) {
                    if(gpt_parts[k] < (unsigned long int)fs_len) {
                        fprintf(stderr,"mkbootimg: partition #%d %s.\r\n", k+1,lang[ERR_PARTSIZE]);
                        exit(2);
                    }
                    fwrite(fs_base, fs_len, 1, f);
                    free(fs_base);
                    size += fs_len;
                }
            }
        }
        fseek(f,gpt_parts[k] - size,SEEK_CUR);
    }
    /* write out backup GPT table */
    fseek(f,tsize-63*512,SEEK_SET);
    fwrite(gpt+1024,62*512,1,f);
    fwrite(gpt2,512,1,f);
    fclose(f);
    printf("\r\x1b[K\r");
    printf("mkbootimg: %s %s.\r\n", fn, lang[SAVED]);
    free(buf);
}
