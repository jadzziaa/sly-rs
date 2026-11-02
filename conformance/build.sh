#!/bin/sh
# Builds conformance.elf with the ps2dev toolchain, using ps2sdk's own EE
# compiler and linker flags (samples/Makefile.eeglobal). Run in the
# ps2dev/ps2dev:v2.0.0 container: cargo xtask conformance-build.
set -e
CC=mips64r5900el-ps2-elf-gcc
CFLAGS="-D_EE -G0 -O1 -Wall -fno-strict-aliasing -I$PS2SDK/ee/include -I$PS2SDK/common/include"
mkdir -p obj
for f in src/*.c; do
    $CC $CFLAGS -c "$f" -o "obj/$(basename "$f" .c).o"
done
$CC -T"$PS2SDK/ee/startup/linkfile" -O1 -o conformance.elf obj/*.o -L"$PS2SDK/ee/lib" -Wl,-zmax-page-size=128
mips64r5900el-ps2-elf-strip conformance.elf
rm -rf obj
