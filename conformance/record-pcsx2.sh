#!/bin/sh
# Records PCSX2's output for a conformance program:
#
#   ./record-pcsx2.sh OUT [recompiler|interpreter|full] [ee|iop]
#
# - ee: conformance.elf's EE console (KPUTCHAR) lines, from "conformance N"
#   to "done".
# - iop: iop/iopload.elf loading iop/iopconf.irx from host:; the module's
#   IOP console lines, from "iop conformance N" to "iop done", without the
#   BIOS loader's own messages.
#
# PCSX2 runs inside `gamescope --backend headless`, so no window opens, with
# a data folder of its own in a temporary directory: the user's PCSX2
# settings are neither read nor changed. The BIOS is $BIOS, by default the
# USA BIOS image in ../oracle/ps2-bios. "interpreter" runs the EE on PCSX2's interpreter
# instead of its recompiler, a second opinion on recompiler quirks; "full"
# is the recompiler with its full FPU mode. The float settings are pinned to
# PCSX2 2.9's defaults so a transcript says what it was made with.
set -eu
out=$(realpath -m "$1")
mode=${2:-recompiler}
here=$(dirname "$(realpath "$0")")
program=${3:-ee}
bios=${BIOS:-$(ls "$here"/../oracle/ps2-bios/ps2-*a-*.bin | head -n 1)}
case $program in
    ee) elf=conformance.elf files="$here/conformance.elf" first='^conformance [0-9]*$' last='^done$' ;;
    iop) elf=iopload.elf files="$here/iop/iopload.elf $here/iop/iopconf.irx" first='^iop conformance [0-9]*$' last='^iop done$' ;;
    *) echo "program is ee or iop" >&2; exit 2 ;;
esac
case $mode in
    recompiler) ee=true full=false ;;
    interpreter) ee=false full=false ;;
    full) ee=true full=true ;;
    *) echo "mode is recompiler, interpreter or full" >&2; exit 2 ;;
esac

work=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$work"' EXIT
data=$work/PCSX2
mkdir -p "$data/inis" "$data/bios"
cp "$bios" "$data/bios/"
for f in $files; do cp "$f" "$work/"; done
cat > "$data/inis/PCSX2.ini" <<EOF
[UI]
SetupWizardIncomplete = false
ConfirmShutdown = false
# Without SettingsVersion PCSX2 takes the file for an old one and waits on a
# dialog nobody can see.
SettingsVersion = 1
[Folders]
Bios = bios
Logs = logs
[Filenames]
BIOS = $(basename "$bios")
[EmuCore]
EnableFastBoot = true
HostFs = true
[EmuCore/CPU]
FPU.DenormalsAreZero = true
FPU.Roundmode = 3
FPUDiv.DenormalsAreZero = true
FPUDiv.Roundmode = 0
VU0.DenormalsAreZero = true
VU0.Roundmode = 3
[EmuCore/CPU/Recompiler]
EnableEE = $ee
EnableVU0 = true
fpuOverflow = true
fpuExtraOverflow = false
fpuFullMode = $full
vu0Overflow = true
vu0ExtraOverflow = false
vu0SignOverflow = false
vu0Underflow = false
[Logging]
EnableSystemConsole = false
EnableFileLogging = true
EnableTimestamps = false
EnableEEConsole = true
EnableIOPConsole = true
EOF

# gamescope gives PCSX2 its own Xwayland; without WAYLAND_DISPLAY PCSX2
# and Qt use that instead of the desktop's compositor.
(cd "$work" && exec gamescope --backend headless -w 640 -h 480 -- \
    env -u WAYLAND_DISPLAY QT_QPA_PLATFORM=xcb pcsx2-qt -nogui -batch -datapath "$work" \
    -logfile "$work/pcsx2.log" -elf "$work/$elf") > "$work/stdout" 2>&1 &
pid=$!

# The program never shuts the machine down: stop once it prints its last line.
i=0
until grep -q "$last" "$work/pcsx2.log" 2>/dev/null; do
    i=$((i + 1))
    if [ $i -gt 600 ] || ! kill -0 "$pid" 2>/dev/null; then
        echo "PCSX2 did not finish; its output:" >&2
        tail -n 20 "$work/stdout" "$work/pcsx2.log" >&2 2>/dev/null || true
        exit 1
    fi
    sleep 0.5
done

# Keep the program's lines only: PCSX2 interleaves its own warnings, the
# BIOS's loader prints, and the EE and IOP consoles share the log.
if [ "$program" = ee ]; then
    sed -n "/$first/,/$last/p" "$work/pcsx2.log" |
        grep -E '^(conformance [0-9]+|done|[a-z0-9._]+ [0-9a-f ]+-> [0-9a-f ]+)$' > "$out"
else
    sed -n "/$first/,/$last/p" "$work/pcsx2.log" | grep -v -E '^(ee: |loadmodule: )' > "$out"
fi
echo "$(wc -l < "$out") lines from PCSX2 ($mode, $program) in $out"
