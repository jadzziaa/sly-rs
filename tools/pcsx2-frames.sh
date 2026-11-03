#!/bin/sh
# Saves the frames PCSX2's software renderer displays, for comparing
# pictures with sly-rs:
#
#   tools/pcsx2-frames.sh DISC OUT FIRST [COUNT]
#
# Frames FIRST to FIRST+COUNT-1 (counted in fields from power-on, COUNT
# defaults to 1) are written to OUT as PCSX2 writes them: each display read
# circuit's framebuffer as a PNG (`*_f<frame>_fr<circuit>_<FBP>_<PSM>.png`),
# the GS privileged registers at each V-sync (`*_vsync_gs_reg.txt`), and
# each draw's registers and vertices.
# They are pictures of the game: keep OUT outside the repository.
#
# DISC may also be a GS dump (`.gs`), such as `sly-rs run --gs-dump`
# writes: PCSX2 then draws exactly the packets sly-rs's GS was sent. Its
# frames count from the dump's start: frame k is what the display shows at
# the dump's k+1-th V-sync, which `sly-rs gs-replay DUMP --frames DIR`
# writes as DIR/frame<k>.png (not the replay's end, which holds part of the
# next field). Compare a pair with `tools/imgcmp.py`, whole directories
# with `tools/framecmp.py`.
#
# With SAVE_RT=1, PCSX2 also saves the framebuffer after every draw in
# those frames (`*_rt*`), to find the first draw that differs. INFO=0 leaves
# out each draw's registers and vertices (long ranges of pictures only).
# STEP=N saves every Nth frame of the range only (FIRST, FIRST+N, ...
# below FIRST+COUNT).
#
# PRESS=BUTTON@FRAME,... presses pad 1's buttons (cross, circle, square,
# triangle, start, select, up, down, left, right; gsdump saves a GS dump
# of the next frame, as PCSX2's hotkey does, into OUT) once PCSX2 has saved that
# frame or a later one (tools/pcsx2-press.py; it needs python-xlib), so
# a press lands on the first saved frame at or after FRAME, give or take
# the moment it takes to notice.
#
# CARD=IMAGE puts a copy of a memory card image in slot 1 (the image itself
# is not changed). Without one, Sly 1 stops at its "no memory card" dialog,
# and PCSX2's own new cards are unformatted, so Sly asks to format them:
# `sly-rs format-card IMAGE` writes a formatted one.
#
# PCSX2 runs inside `gamescope --backend headless` (no window opens) with a
# data folder of its own in a temporary directory, no memory card unless
# CARD is given, the software renderer at native resolution, and the USA
# BIOS image in ../oracle/ps2-bios unless $BIOS says otherwise.
set -eu
disc=$(realpath "$1")
out=$(realpath -m "$2")
first=$3
count=${4:-1}
here=$(dirname "$(realpath "$0")")
bios=${BIOS:-$(ls "$here"/../oracle/ps2-bios/ps2-*a-*.bin | head -n 1)}

work=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$work"' EXIT
data=$work/PCSX2
mkdir -p "$data/inis" "$data/bios" "$data/memcards" "$out"
cp "$bios" "$data/bios/"
slot1=false
if [ -n "${CARD:-}" ]; then
    cp "$CARD" "$data/memcards/Mcd001.ps2"
    slot1=true
fi
cat > "$data/inis/PCSX2.ini" <<INI
[UI]
SetupWizardIncomplete = false
ConfirmShutdown = false
SettingsVersion = 1
[Folders]
Bios = bios
Logs = logs
MemoryCards = memcards
[Filenames]
BIOS = $(basename "$bios")
[EmuCore]
EnableFastBoot = true
[MemoryCards]
Slot1_Enable = $slot1
Slot1_Type = File
Slot1_Filename = Mcd001.ps2
Slot2_Enable = false
[Pad1]
Type = DualShock2
Cross = Keyboard/Return
Circle = Keyboard/Backspace
Square = Keyboard/Z
Triangle = Keyboard/A
Start = Keyboard/Space
Select = Keyboard/Tab
Up = Keyboard/Up
Down = Keyboard/Down
Left = Keyboard/Left
Right = Keyboard/Right
[Hotkeys]
GSDumpSingleFrame = Keyboard/F8
[EmuCore/GS]
GSDumpCompression = 0
Renderer = 13
upscale_multiplier = 1
DumpGSData = true
SaveFrame = true
SaveRT = $([ "${SAVE_RT:-0}" = 1 ] && echo true || echo false)
SaveInfo = $([ "${INFO:-1}" = 1 ] && echo true || echo false)
SaveDrawStart = 0
SaveDrawCount = -1
SaveFrameStart = $first
SaveFrameCount = $count
SaveFrameBy = ${STEP:-1}
HWDumpDirectory = $out
SWDumpDirectory = $out
INI

# PCSX2 and the presser run inside gamescope, on its Xwayland.
cat > "$work/inside.sh" <<'SH'
#!/bin/sh
work=$1 disc=$2 out=$3 press=$4 here=$5
unset WAYLAND_DISPLAY
[ -z "$press" ] || python3 -I "$here/pcsx2-press.py" "$out" "$press" &
exec env QT_QPA_PLATFORM=xcb pcsx2-qt -nogui -batch -datapath "$work" \
    -logfile "$work/pcsx2.log" -- "$disc"
SH
(cd "$work" && exec gamescope --backend headless -w 640 -h 480 -- \
    sh "$work/inside.sh" "$work" "$disc" "$out" "${PRESS:-}" "$here") > "$work/stdout" 2>&1 &
pid=$!

# Stop once the last frame asked for has its registers (or, with INFO=0,
# which writes none, its picture).
step=${STEP:-1}
end=$((first + (count - 1) / step * step))
last=$(printf 'f%05d_vsync' $end)
[ "${INFO:-1}" = 1 ] || last=$(printf 'f%05d_fr' $end)
i=0
until ls "$out" 2>/dev/null | grep -q "$last"; do
    i=$((i + 1))
    if [ $i -gt 2400 ] || ! kill -0 "$pid" 2>/dev/null; then
        echo "PCSX2 did not get there; its output:" >&2
        tail -n 20 "$work/stdout" "$work/pcsx2.log" >&2 2>/dev/null || true
        exit 1
    fi
    sleep 0.5
done
# GS dumps taken with the gsdump press.
for dump in "$work"/snaps/*.gs "$work"/snaps/*/*.gs; do
    [ -e "$dump" ] && cp "$dump" "$out/"
done
echo "$(ls "$out" | grep -c '_fr[0-9]_.*png') framebuffers from PCSX2 in $out"
