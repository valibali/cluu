/*
 * mkbootimg/lang.h
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
 * @brief Multilanguage support
 *
 */

enum {
    ERR_MEM = 0,
    ERR_INITRDIMG,
    ERR_NOINITRD,
    ERR_NOINITRDTYPE,
    ERR_BADINITRDTYPE,
    ERR_INITRDTYPE,
    ERR_ACCEPTVALUES,
    ERR_NOCONF,
    ERR_BIGCONF,
    ERR_NOPART,
    ERR_NOPARTSIZE,
    ERR_BADARCH,
    ERR_MORESEG,
    ERR_BADENTRYP,
    ERR_INVALIDEXE,
    ERR_BADADDR,
    ERR_BADALIGN,
    ERR_PAGEALIGN,
    ERR_ADDRCOL,
    ERR_BADSIZE,
    ERR_BIGSEG,
    ERR_WRITE,
    ERR_LOCKRNL,
    ERR_KRNL,
    ERR_JSON,
    ERR_TYPE,
    ERR_GUIDFMT,
    ERR_NONAME,
    ERR_PARTIMG,
    ERR_ST2ALIGN,
    ERR_PARTSIZE,
    ERR_NOSIZE,
    ERR_TOOBIG,
    ERR_TOOMANY,
    STATADDR,
    DYNADDR,
    HELP1,
    HELP2,
    HELP3,
    HELP4,
    HELP5,
    HELP6,
    WRITING,
    SAVED,
    /* must be the last */
    NUMTEXTS
};

#define NUMLANGS         3

extern char *dict[NUMLANGS][NUMTEXTS + 1], **lang;

