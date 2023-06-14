BOOTBOOT Minta Bootolható Lemezkép Fájlok
=========================================

Általános leírásért lásd a [BOOTBOOT Protokoll](https://gitlab.com/bztsrc/bootboot)t.

- disk-rpi.img.gz: minta lemezkép AArch64-hez RaspberryPi 3-on és 4-en
- disk-x86.img.gz: minta lemezkép x86_64-hez (CDROM, BIOS, UEFI)
- initrd.rom.gz: minta initrd ROM kép (beágyazott BIOS rendszerekhez)
- coreboot-x86.rom.gz: minta coreboot ROM kép BOOTBOOT payload-al PC-re

Mielőtt használhatnád a lemezképeket, ki kell csomagolni őket a `gzip -d` paranccsal. A lemezképeket az [mkbootimg](https://gitlab.com/bztsrc/bootboot/tree/master/mkbootimg)
paranccsal hoztam létre, és a kiírásukhoz fizikai lemezre az [USBImager](https://bztsrc.gitlab.io/usbimager)-t vagy a `dd` parancsot javaslom.

A disk-x86.img egy speciális hibrid lemezkép, amit átnevezhetsz disk-x86.iso-ra és kiégetheted egy CDROM-ra; vagy bebootolhatod
USB pendrávjról is BIOS valamint UEFI gépeken egyaránt.

A disk-rpi.img egy (Class 10) SD kártyára írható, és Raspberry Pi 3-on és 4-en bootolható.

A lemezképekben mindössze egy boot partíció található. Az `fdisk` paranccsal szabadon hozzáadhatsz még partíciókat az izlésednek
megfelelően, vagy csak módosítsd az mkbootimg.json fájlt és adj hozzá rekordokat a `partitions` tömbhöz.

Fordítás
--------

Először is mozgasd át ezt az egész `images` mappát a helyi repód master ága alá.
Lásd mkbootimg.json. Nézz bele a Makefile-ba is, az elején fogsz látni konfigurálható változókat.

- PLATFORM: vagy "x86" vagy "rpi", ez választja ki, melyik lemezképet generálja
- OVMF: a EFI firmware elérési útja

Aztán csak futtasd a `make` parancsot.

A coreboot-*.rom fordításához [coreboot fordító környezet](https://gitlab.com/bztsrc/bootboot/tree/master/x86_64-cb) szükséges.

Tesztelés
---------

Hogy kipróbáld a BOOTBOOT-ot qemu-ban, használd a következő parancsokat:
```
make rom
```
Ez betölti a minta kernelt ROM-ból (lemez nélküli boot tesztelése BIOS Boot Spec alapján).
```
make bios
```
Ez betölti a minta kernelt lemezről (BIOS-al).
```
make cdrom
```
Ez El Torito "nem emulált" CDROM-ról tölti be a minta kernelt (BIOS-al).
```
make efi
```
Ez betölti a kernelt lemezről, UEFI használatával. Kell hozzá a TianoCode BIOS képfájl, amit a Makefile elején kell megadni.
```
make eficdrom
```
Ez betölti a kernelt CDROM-ról, UEFI használatával.
```
make grubcdrom
```
Ez grub-mkrescue hívásával hoz létre egy cdrom lemezképet, majd Multiboot-al betölti a BOOTBOOT-ot.
```
make linux
```
Ez betölti a minta kernelt úgy, hogy a BOOTBOOT-ot [Linux/x86 Boot Protocol](https://www.kernel.org/doc/html/latest/x86/boot.html)-al
indítja.
```
make sdcard
```
Ez "raspi3" gépet emulálva tölti be a minta kernelt SD kártya meghajtóról (kell hozzá a qemu-system-aarch64).
```
make coreboot
```
BOOTBOOT tesztelése mint coreboot payload (nincs BIOS se UEFI). PLATFORM=x86 esetén PC-t emulál, egyébként ARM64-et.
```
make bochs
```
Tesztelés bochs-al (BIOS-al).
