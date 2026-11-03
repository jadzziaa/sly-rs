#!/bin/sh
# Builds iopconf.irx (the IOP module) and iopload.elf (the EE program that
# loads it) with the ps2dev toolchain, following ps2sdk's
# samples/Makefile.iopglobal and Makefile.eeglobal. Run in the
# ps2dev/ps2dev:v2.0.0 container: cargo xtask conformance-build.
set -e
IOPCC=mipsel-none-elf-gcc
IOPFLAGS="-D_IOP -fno-builtin -G0 -Os -Wall -msoft-float -mno-explicit-relocs -I$PS2SDK/iop/include -I$PS2SDK/common/include -I."
mkdir -p obj
printf '%s\n' '#include "irx_imports.h"' > obj/build-imports.c
cat imports.lst >> obj/build-imports.c
$IOPCC $IOPFLAGS -fno-toplevel-reorder -c obj/build-imports.c -o obj/imports.o
$IOPCC $IOPFLAGS -c iopconf.c -o obj/iopconf.o
$IOPCC $IOPFLAGS -T"$PS2SDK/iop/startup/linkfile" -o obj/iopconf.notiopmod.elf obj/iopconf.o obj/imports.o -nostdlib -dc -r
mipsel-none-elf-strip --strip-unneeded --remove-section=.pdr --remove-section=.comment --remove-section=.mdebug.abi32 --remove-section=.gnu.attributes -o obj/iopconf.stripped.elf obj/iopconf.notiopmod.elf
iopfixup --rb --irx1 --allow-zero-text -o iopconf.irx obj/iopconf.stripped.elf

EECC=mips64r5900el-ps2-elf-gcc
$EECC -D_EE -G0 -O1 -Wall -I$PS2SDK/ee/include -I$PS2SDK/common/include -c iopload.c -o obj/iopload.o
$EECC -T"$PS2SDK/ee/startup/linkfile" -O1 -o iopload.elf obj/iopload.o -L"$PS2SDK/ee/lib" -Wl,-zmax-page-size=128
mips64r5900el-ps2-elf-strip iopload.elf
rm -rf obj
