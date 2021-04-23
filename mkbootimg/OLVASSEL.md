BOOTBOOT Bootolható Lemezkép Készítő
====================================

Általános leírásért lásd a [BOOTBOOT Protokoll](https://gitlab.com/bztsrc/bootboot)t.

Ez egy minden az egyben, többplatformos, függőség nélküli lemezkép kreáló (na jó, zlib kell neki, de az statikusan bele van
forgatva). Egy lemezkonfigurációt kell megadni neki JSON-ben, és létrehozza az ESP FAT boot partíciót a szükséges betöltő
fájlokkal, GPT táblával, PMBR-el, stb. Továbbá képes létrehozni az induló memórialemezképet egy könyvtár tartalmából. Támogatott
fájlrendszerek:

| Formátum | Initrd | Partíció | Specifikáció, forrás                            |
|----------|--------|----------|-------------------------------------------------|
| `jamesm` | ✔Yes   | ✗No      | [James Molloy oktatóanyagok](http://jamesmolloy.co.uk/tutorial_html/8.-The%20VFS%20and%20the%20initrd.html) |
| `cpio`   | ✔Yes   | ✗No      | [wikipédia](https://en.wikipedia.org/wiki/Cpio) |
| `tar`    | ✔Yes   | ✔Yes     | [wikipédia](https://wiki.osdev.org/USTAR)       |
| `echfs`  | ✔Yes   | ✔Yes     | [spec](https://gitlab.com/bztsrc/bootboot/blob/binaries/specs/echfs.md), [forrás](https://github.com/echfs/echfs) |
| `FS/Z`   | ✔Yes   | ✔Yes     | [spec](https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/fsz.pdf), [forrás](https://gitlab.com/bztsrc/bootboot/blob/master/mkbootimg/fsZ.h) |
| `boot`   | ✗No    | ✔Yes     | [spec](https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/efifat.pdf) (csak ESP, 8+3 nevek) |
| `fat`    | ✗No    | ✔Yes     | [spec](https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/vfat.pdf) (csak nem-ESP, LFN-el) |
| `minix`  | ✗No    | ✔Yes     | [V2 spec](https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/minix.pdf), [V3 forrás](https://github.com/Stichting-MINIX-Research-Foundation/minix/tree/master/minix/fs/mfs) (V3 támogatott, de csak V2-höz van spec) |
| `ext2`   | ✗No    | ✔Yes     | [spec](https://gitlab.com/bztsrc/bootboot/raw/binaries/specs/ext2.pdf), [dokumentáció](https://www.nongnu.org/ext2-doc/ext2.html) |
| `lean`   | ✗No    | ✔Yes     | [V0.6 spec](http://freedos-32.sourceforge.net/lean/specification.php), [V0.7 spec](http://www.fysnet.net/leanfs/specification.php) |

A kód úgy lett megírva, hogy könnyű legyen bővíteni.

A kigenerált képet leellenőriztem fdisk-el, valamint a gdisk verify funkciójával. A FAT partíció tesztelve lett fsck.vfat-al
és UEFI förmverrel, továbbá Raspberry Pi-n. Az ISO9660-es rész iat-vel (ISO9660 Analyzer Tool) és Linux mounttal lett tesztelve.

Működési módok
--------------

```
$ ./mkbootimg
BOOTBOOT mkbootimg utility - bztsrc@gitlab
 BOOTBOOT Copyright (c) bzt MIT https://gitlab.com/bztsrc/bootboot
 deflate 1.2.11 Copyright 1995-2017 Jean-loup Gailly and Mark Adler
 Raspbery Pi Firmware Copyright (c) Broadcom Corp, Raspberry Pi (Trading) Ltd

Ellenőrzi, hogy az ELF vagy PE futtatható BOOTBOOT kompatíbilis-e, illetve
hibrid indító lemez képet vagy Option ROM képet generál a hobbi OS-edhez.

Használat:
  ./mkbootimg check <kernel elf / pe>
  ./mkbootimg <konfigurációs json> initrd.rom
  ./mkbootimg <konfigurációs json> bootpart.bin
  ./mkbootimg <konfigurációs json> <kimeneti lemezkép neve>

Példák:
  ./mkbootimg check mykernel/mykernel.x86_64.elf
  ./mkbootimg myos.json initrd.rom
  ./mkbootimg myos.json bootpart.bin
  ./mkbootimg myos.json myos.img
```

Ha az első paraméter `check` (ellenőrzés), akkor a második egy kernel fájlnév. A parancs ellenőrizni fogja a futtathatót,
hogy megfelel-e a BOOTBOOT-nak, részletesen kijelzi a hibákat, és ha átment az ellenőrzésen, megadja, milyen BOOTBOOT
Protokoll szintű betöltő kell a betöltéséhez.

Egyébként az első paraméter a konfigurációs JSON fájl. Ha a második paraméter `initrd.rom`, akkor BIOS Option ROM-ot generál
a megadott initrd könyvtár tartalmából. Ha `bootpart.bin`, akkor a boot partíció képét menti le (és csakis a partíció képét).
Minden más fájlnévre egy teljes lemezképet hoz létre GPT-vel.

Az eszköz többnyelvű. Automatikusan detektálja az operációs rendszered nyelvét, és ha van szótára hozzá, akkor azt használja.
Ez felülbírálható parancssorból a `-l <nyelv>` kapcsolóval, mint első paraméterrel (minden működési mód esetén megadható).
A nyelvkód két karakteres, és az alapértelmezett az `en`. A magyarhoz `-l hu`-t kell megadni.

Konfiguráció
------------

A JSON egyszerű és rugalmas, többféle variációt is elfogad. A legfelső szinten lehet megadni a lemezre vonatkozó paramétereket.

### Legfelső szint

| Mező       | Típus    | Leírás                                                                              |
|------------|----------|-------------------------------------------------------------------------------------|
| diskguid   | GUID     | opcionális, a lemez GUID-ja. Ha nincs megadva, vagy csupa nulla, akkor generálódik  |
| disksize   | szám     | opcionális, a lemezkép mérete Megabájtban. Ha nincs megadva, kiszámolja             |
| align      | szám     | opcionális, partíció igazítás Kilobájtban. Nullával szektorméretre igazít           |
| iso9660    | logikai  | opcionális, generáljon-e ISO9660 Boot Katalógust a lemezképbe. Alapból ne, false    |
| config     | fájlnév  | a BOOTBOOT konfigurációs fájl. Ebből olvassa ki a kernel fájlnevét                  |
| initrd     | struktúra| az induló lemezkép definícíója, lásd alább                                          |
| partitions | tömb     | a partícíók definíciói, lásd alább                                                  |

Példa:
```
{
    "diskguid": "00000000-0000-0000-0000-000000000000",
    "disksize": 512,
    "align": 1024,
    "iso9660": true,
    "config": "boot/sys/config",
    "initrd": { "type": "tar", "gzip": true, "directory": "boot" },
    "partitions": [
        { "type": "boot", "size": 16 },
        { "type": "ext4", "size": 128, "name": "Linux Exchange" },
        { "type": "ntfs", "size": 128, "name": "Windows Exchange" },
        { "type": "Microsoft basic data", "size": 32, "name": "MyOS usr", "file": "usrpart.bin" },
        { "type": "00000000-0000-0000-0000-000000000000", "size": 32, "name": "MyOS var", "file": "varpart.bin" }
    ]
}
```

### Initrd

| Mező       | Típus    | Leírás                                                                              |
|------------|----------|-------------------------------------------------------------------------------------|
| gzip       | logikai  | opcionális, tömörítse-e az induló memórialemezképet, alapértelmezetten igen, true   |
| type       | sztring  | az induló memórialemezkép formátuma. Érvénytelen esetén listázza a lehetőségeket    |
| file       | fájlnév  | a használandó lemezkép fájlneve                                                     |
| directory  | mappa    | mappa elérési útja, a tarmalmából fogja generálni az induló memórialemezképet       |
| file       | tömb     | többarchitektúrás lemezképekhez                                                     |
| directory  | tömb     | többarchitektúrás lemezképekhez                                                     |

A `file` és a `directory` kölcsönösen kizárja egymást. Mindkettő lehet sztring (ha csak egy architektúrához generálunk),
vagy tömb (egy elem minden architektúrához). Jelenleg három támogatott, azaz minden tömb maximum három elemű lehet.
Hogy melyik architektúrát jelenti, azt az dönti el, hogy a mabbában vagy lemezképben milyen architektúrájú kernel található.
A `type` típust csak `directory` esetén kötelező megadni.

Példák:
```
    "initrd": { "file": "initrd.bin" },
    "initrd": { "type": "tar", "gzip": 0, "directory": "boot" },
    "initrd": { "gzip": true, "file": [ "initrd-x86.bin", "initrd-arm.bin", "initrd-rv64.bin" ] },
    "initrd": { "type": "cpio", "gzip": true, "directory": [ "boot/arm", "boot/x86", "boot/riscv64" ] },
```

### Partíciók

Kicsit szokatlan, a legelső elem különbözik a többitől. Az a boot partíciót definiálja, ezért eltérő típusokat
használ, és a `file` / `directory` valamint a `name` nem használható, mivel az a partíció mindig dinamikusan generált,
fix "EFI System Partition" névvel. Ugyanezért a `size` méret megadása kötelező az első (boot) partíciónál.

| Mező       | Típus    | Leírás                                                                              |
|------------|----------|-------------------------------------------------------------------------------------|
| size       | szám     | opcionális, a partíció mérete Megabájtban. Ha nincs megadva, kiszámolja             |
| file       | fájlnév  | opcionális, a használandó partíciókép elérési útja                                  |
| directory  | mappa    | opcionális, mappa elérési útja, a tarmalmából fogja generálni a partícióképet       |
| driver     | sztring  | opcionális, ha a paríció típusa nem határozná meg egyértelműen a formátumot         |
| type       | sztring  | a partíció formátuma. Érvénytelen esetén listázza a lehetőségeket                   |
| name       | sztring  | UTF-8 partíciónév, korlátozva a 32 és 65535 közötti UNICODE kódpontokra (BMP)       |

Az első elem esetén a `type` lehetséges értékei: `boot` (vagy explicit `fat16` és `fat32`). Csak 8+3 fájlneveket generál.
A parancs igyekszik kényelmesen kezelni ezt, ha lehet FAT16-ot választva, helytakarékosság miatt. A boot partíció
minimális mérete 8 Megabájt. Bár mind a lemezkép készítő, mind a BOOTBOOT betöltő képes lenne kezelni kissebb méretet,
néhány UEFI förmver helytelenül FAT12-nek hiszi, ha túl kevés kluszter van a fájlrendszeren. Ha a partíció mérete meghaladja
a 128 Megabájtot, akkor automatikusan FAT32-t választ. Ha nem használsz `iso9660`-t, akkor kissebb méretű is lehet, de
legalább 33 Megabájt (ez a FAT32 minimális mérete). Ugyanakkor `iso9660` használata esetén garantálni kell, hogy minden
kluszter 2048 bájtos címen kezdődjön, amit 4 szektor per kluszterrel a legegyszerűbb elérni. Itt is ugyanaz a probléma merül
fel, mind a lemezkép készítő, mind a BOOTBOOT betöltők képesek lennének kevessebb kluszterrel is használni a FAT32-t, de
néhány UEFI förmver nem, és hibásan FAT16-nak látná. Hogy ezt elkerüljük a minimális kluszterszámmal, az ISO9960 és FAT32
együttes használata esetén a partíció minimális mérete 128 Megabájt (128\*1024\*1024/512/4 = 65536, ami pont eggyel több,
mint ami még 16 bitbe belefér).

A többi (a másodiktól kezdve) bejegyzés esetén a `type` vagy egy GUID, vagy egy az előre definiált aliaszok közül. Itt a
`fat` meghajtó csakis a kluszterek száma alapján dönt, hogy FAT16 vagy FAT32 legyen-e, és hosszú fájlneveket is generál.
Érvénytelen sztring esetén a parancs listázza az összes lehetséges értéket.

Példa:
```
mkbootimg: partition #2 nincs érvényes type típusa. Lehetséges értékek:
  "65706154-4120-6372-6968-766520465320" / "tar"
  "5A2F534F-0000-5346-2F5A-000000000000" / "FS/Z"
  "6A898CC3-1DD2-11B2-99A6-080020736631" / "ZFS"
  "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" / "ntfs"
  "0FC63DAF-8483-4772-8E79-3D69D8477DE4" / "ext4"
  "516E7CB6-6ECF-11D6-8FF8-00022D09712B" / "ufs"
  "C91818F9-8025-47AF-89D2-F030D7000C2C" / "p9"
  "D3BFE2DE-3DAF-11DF-BA40-E3A556D89593" / "Intel Fast Flash"
  "21686148-6449-6E6F-744E-656564454649" / "BIOS boot"
     ...
  "77719A0C-A4A0-11E3-A47E-000C29745A24" / "VMware Virsto"
  "9198EFFC-31C0-11DB-8F78-000C2911D1B8" / "VMware Reserved"
  "824CC7A0-36A8-11E3-890A-952519AD3F61" / "OpenBSD data"
  "CEF5A9AD-73BC-4601-89F3-CDEEEEE321A1" / "QNX6 file system"
  "C91818F9-8025-47AF-89D2-F030D7000C2C" / "Plan 9 partition"
  "5B193300-FC78-40CD-8002-E86C45580B47" / "HiFive Unleashed FSBL"
  "2E54B353-1271-4842-806F-E436D6AF6985" / "HiFive Unleashed BBL"
  ...vagy bármilyen nem csupa nulla GUID ilyen formátumban "%08X-%04X-%04X-%04X-%12X"
```

Ha a `file` meg van adva, akkor a partíció fel lesz tölve a fájl tartalmával. Ha a `size` méret nincs megadva, vagy
kissebb, mint a fájl mérete, akkor a fájl mérete lesz a partíció mérete. Ha mindkettő meg van adva, és a `size` nagyobb,
akkor a kölönbséget nullákkal tölti fel. A partíció mérete mindig `align` Kilobájt többszöröse lesz. 1024 megadásával
a partíciók 1 Megabájtos címekre lesznek igazítva. Az első bejegyzés esetén csak a `size` használható, a `file` nem.
Alternatívaként esetleg használható a `directory` a `file` helyett, amennyiben a `type`-nál megadott típushoz van
fájlrendszer meghajtó implementálva. Ekkor a megadott mappa tartalmából generálódik a partíció tartalma. Mivel nem feltétlenül
van egy-az-egyhez megfeleltetés a partíció típus és a fájlrendszer típus között, ezért használható a `driver` az utóbbi
explicit megadására. Erre csak a `directory` direktíva használata esetén lehet szükség. Példák:
```
    { "type": "5A2F534F-8664-5346-2F5A-000075737200", "driver": "FS/Z",  "size": 32, "name": "usr",  "directory": "myusr" },
    { "type": "Linux home",                           "driver": "minix", "size": 32, "name": "home", "directory": "myhome" },
    { "type": "Microsoft basic data",                 "driver": "fat",   "size": 32, "name": "data", "directory": "mydata" },
```

Végezetül a `name` egy sima UTF-8 sztring, a partíció neve. Maximális hossza 35 karakter. Az első partíciónál nem használható.

Újabb fájlrendszerek hozzáadása
-------------------------------

Ezeket az fs registry listázza, az `fs.h` fájlban. Szabadon hozzáadhatsz új típusokat. Azoknál a fájlrendszereknél,
amiket indító memórialemezképhez vagy partícióképhez is szeretnél használni, implementálni kell három funkciót, például:

```
void somefs_open(gpt_t *gpt_entry);
void somefs_add(struct stat *st, char *name, int pathlen, unsigned char *content, int size);
void somefs_close();
```

Az első, az "open" akkor hívódik, amikor egy új fájlrendszert kell létrehozni. Itt a `gpt_entry` mutató NULL, ha memórialemezkép
kreáláshoz hívódik a meghajtó. Ahogy a megadott mappát rekurzívan bejárja, minden almappa és fájl esetén meghívódik az "add". Ez
hozzá kell adja a fájlt vagy mappát a fájlrendszer képéhez. Az `st` a stat struktúra, `name` a fájl neve teljes elérési úttal,
a `content` és a `size` pedig a fájl tartalma, illetve szimbolikus hivatkozások esetén a mutatott elérési út. Végezetül amikor a
bejárásnak vége, a "close" hívódik meg, hogy lezárja és véglegesítse a lemezképet. Ezek közül csak az "add" a kötelező, a másik
kettő opcionális.

Ezek a funkciók elérnek két globális változót, az `fs_base`-t és `fs_len`-t, amik a lemezkép memóriabeli bufferét jelölik
(ebből következik, hogy a partíciók mérete pár gigabájt lehet, amennyi szabad memória van a gépedben). Ha hibát kell jelenteni,
az `fs_no` változó tartalmazza annak a partíciónak a számát, amihez a meghajtó éppen generál.

Ezen függvények hiányában, a fájlrendszer továbbra is használható a partíciók `type` mezőjében, de ekkor csak a GPT bejegyzést
hozza létre, magát a partíció tartalmát nem. A `driver` mezőben csak olyan fájlrendszer típus adható meg, ami rendelkezik ezekkel
a funkciókkal.

A beépített binárisok naprakészen tartása
-----------------------------------------

Hogy ne legyen függősége, a lemezkép készítő minden szükséges binárist tartalmaz. Ha ezek frissülnek, akkor le kell törölni
a data.c fájlt, amit a `make` parancs újragenerál. Ha hiányol fájlokat, akkor a `aarch64-rpi` mappában kell kiadni a `make getfw`
parancsot, ami letölti a legfrissebb Raspberry Pi förmver fájlokat. Utánna már menni fog a `make` ebben a könyvtárban.

