TARGET = mkbootimg

CC = gcc
CFLAGS = -Wall -Wextra -ansi -pedantic
SRCS = $(filter-out bin2h.c data.c,$(wildcard *.c)) data.c

ifneq ("$(wildcard /bin/*.exe)","")
CFLAGS += -Wno-long-long
LIBDIRS = -static -static-libgcc
ZIPNAME = ../$(TARGET)-Win.zip
else
ifneq ("$(wildcard /Applications/*)","")
CFLAGS += -Wno-long-long
ZIPNAME = ../$(TARGET)-MacOSX.zip
CFLAGS += -DMACOSX
else
ZIPNAME = ../$(TARGET)-$(shell uname -s).zip
endif
endif
OBJS = $(SRCS:.c=.o)
INCBIN = ../dist/boot.bin ../dist/bootboot.bin ../dist/bootboot.efi ../dist/bootboot.img ../dist/bootboot.rv64 ../aarch64-rpi/LICENCE.broadcom ../aarch64-rpi/bootcode.bin ../aarch64-rpi/fixup.dat ../aarch64-rpi/start.elf

all: data.c $(TARGET)

../aarch64-rpi/start.elf:
	make -C ../aarch64-rpi getfw

data.c:
	$(CC) bin2h.c -o bin2h
	./bin2h $(INCBIN) >data.c
	@rm bin2h

zlib.o: zlib.c zlib.h
	$(CC) $(CFLAGS) -Wno-implicit-fallthrough zlib.c -c -o zlib.o

%: %.c
	$(CC) $(CFLAGS) $< -c $@

$(TARGET): $(OBJS)
	$(CC) $(OBJS) -o $(TARGET) $(LIBDIRS) $(LIBS)

clean:
	@rm $(TARGET)* *.o 2>/dev/null || true

