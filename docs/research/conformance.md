---
type: Finding
title: The conformance program against PCSX2
description: Our own PS2 test program (FPU, integer, MMI, unaligned loads and stores, VU0 macro mode) run in the interpreter and in PCSX2's recompiler and interpreter - which differences were our bugs and fixed, which are PCSX2's, and why PCSX2 cannot settle the float and VU0 questions.
tags: [finding, conformance, pcsx2, float, mmi, vu0, phase-3]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T21:26:58Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T21:27:06Z }
sources:
  - id: program
    resource: conformance/
    title: The conformance program, its build and recording scripts, and the three transcripts
  - id: test
    resource: crates/sly-verify/tests/conformance.rs
    title: The conformance test
  - id: pcsx2
    resource: PCSX2 v2.9.152 (pcsx2-qt), USA BIOS v2.20 (2005-06-20), 2026-11-02
    title: PCSX2 runs of conformance.elf, recompiler, interpreter and full FPU mode
  - id: float
    resource: crates/ps2-core/src/float.rs
    title: The PS2 float rules as ps2-core implements them
---

# The conformance program against PCSX2

Recorded on 2026-11-02.[^program][^pcsx2] The verification is of the
comparison itself: the transcripts and counts below were produced and checked
in this session. It does not verify the float rules, which stay unverified
(see [Floats](#floats)).

## The program

`conformance/` holds a small PS2 program of our own, built with the ps2dev
toolchain (`cargo xtask conformance-build`, podman image
`ps2dev/ps2dev:v2.0.0`, reproducible to the byte). It prints one line per
case to the EE debug console (`KPUTCHAR`, 0x1000F180):
`name inputs -> results`, in hex. 7,290 lines, 156 operations:

| Group | Operations | Cases |
| --- | --- | --- |
| FPU | 14 arithmetic ops, `cvt.w.s`, `c.eq/lt/le.s` | 160 each, edge values then random |
| Integer | `mult`/`div`/`madd` families including pipeline 1; `lwl`/`lwr`/`ldl`/`ldr`/`swl`/`swr`/`sdl`/`sdr` at every offset | 64 + random; 8 offsets |
| MMI | 70 lane operations, 19 HI/LO operations | 24 each |
| VU0 macro | 30 operations, reporting the result vector, status and MAC | 40 each |

`conformance/record-pcsx2.sh` (`cargo xtask conformance-record`) runs it in
PCSX2 inside `gamescope --backend headless`, so no window opens, with a data
folder of its own: the user's PCSX2 settings are neither read nor changed.
The float settings are pinned to PCSX2 2.9's defaults: FPU and VU0 round
toward zero (mode 3), FPU division to nearest (mode 0), denormals are zero,
overflow clamping on, extra overflow and full mode off.

## Results

### Our bugs, fixed

PCSX2's recompiler and interpreter agreed with each other and against us:

| Operation | Was | Now |
| --- | --- | --- |
| `plzcw` | upper 64 bits of `rd` taken from `rs` | left alone |
| `phmadh`, `phmsbh` | LO/HI words 1 and 3 left alone | the pair's first product (`p1`, `p3`, `p5`, `p7`); inverted by `phmsbh` |
| `pdivbw` | remainder after division by zero cut to 16 bits | the whole dividend, as `div` does |

The `phmadh` words were checked by hand, not only against the transcript:
LO word 1 = 0xB648 × 0xCC58 = 0x0EE008C0, HI word 1 = 0x3E54 × 0xA1A3 =
0xE906837C, as recorded.

### PCSX2's bugs, ours kept

The two PCSX2 modes disagree, and we side with one of them:

- `lwr` at offsets 1-3: the recompiler zeroes the upper 32 bits of `rt`;
  the interpreter keeps them, and so do we. Offset 0 sign-extends in all
  three.
- `pmaddw`, `pmsubw`: the interpreter differs in 16 cases each. With
  `rt = 0` it still adds 1 to HI:LO, where the recompiler and we leave it
  unchanged.

### Bugs in the test program, fixed

- VU0 flags were read with `cfc2` straight after the operation, before the
  macro-mode pipeline updates them. PCSX2 warns "Possible old value used in
  COP2 code". Four `vnop`s now separate them.
- `plzcw` writes only half of `rd`, and the test never set the other half.
  `rd` now starts as the second input.
- The load buffer's bytes never had bit 31 set, so sign extension went
  untested. It now starts at 0x7E.

### Floats

PCSX2 computes floats with x86 SSE in a host rounding mode, then clamps. It is
not a reference for the PS2's own rules[^float] (no Inf/NaN, no guard bits,
rounding per unit). Excluding inputs with exponent 255, which PCSX2 reads as
Inf/NaN, there are 2,388 FPU lines. Of those, 65 differ from the recompiler,
270 from the interpreter and 123 from the recompiler's full FPU mode.

| Operations | Against the recompiler | Reading |
| --- | --- | --- |
| `c.eq/lt/le.s`, `cvt.w.s`, `neg.s`, `mul.s`, `madd.s`, `madda.s`, `div.s`, `sqrt.s` | identical | agree |
| `abs.s`, `max.s`, `min.s` | the recompiler flushes denormal results to zero | host artefact: its interpreter keeps the bits, as we do |
| `add.s`, `sub.s`, `msub.s` | one ulp apart in 19 cases | **unresolved**: we drop the smaller operand's bits below the larger's last place (no guard bits); PCSX2 rounds the exact sum |
| `rsqrt.s` | one ulp apart in 20 cases | **unresolved**: rounding of the combined operation |

The interpreter differs more because it rounds division and square root
toward zero, where the recompiler follows its FPU-division setting.

For VU0, PCSX2's two modes agree on the result vectors, but both use the same
host arithmetic, so their agreement is not independent evidence. The
remaining differences in the result vectors fall into three groups, all
**unresolved**:

- `vdiv`, `vsqrt`, `vrsqrt`: PCSX2 chops; we round to nearest, as for the
  FPU. `vmulq` differs as a consequence, through Q.
- `vadd`, `vsub`, `vmadd`/`vmsub` and their broadcast forms: the guard-bit
  question again. **Settled for the VUs, 2026-11-03**: sly-rs now follows
  PCSX2's model there (the exact result toward zero, no guard bits), as the
  user decided for speed. Result vectors now agree with PCSX2's interpreter
  in 1,142 of 1,200 VU0 lines (916 before; every `vadd`, `vaddi`, `vaddx`
  and `vsub`), and with its recompiler in 982 (827). The EE's FPU keeps the
  guard-bit rule.
- Status and MAC flags: PCSX2's are not a reference. After a `vmula` whose
  product overflows, PCSX2 reports no overflow bits in MAC. The VUs now
  keep only zero and sign flags, as PCSX2 does; Sly reads no others (its
  microcode tests Z and S with `FSAND`, its EE code compares the whole MAC
  word to 0xF0).

## What follows

- PCSX2 settles integer, MMI and load/store behaviour. The test holds every
  such line to PCSX2's recompiler or interpreter, and the whole output to
  `conformance/expected.txt`.[^test]
- It does not settle float or VU0 behaviour. The float rules in `ps2-core`
  stay as written and unverified. Settling them needs results from a real
  console (for example the ps2autotests suites run on hardware), or a
  reference whose float model was itself checked on hardware.
- For phase 4, PCSX2 cannot judge a float disagreement between interpreted
  and recompiled code. The two share `ps2-core`, so the lockstep must agree
  bit for bit regardless.

[^program]: The conformance program, its build and recording scripts, and the three transcripts.
[^pcsx2]: PCSX2 runs of conformance.elf, recompiler, interpreter and full FPU mode.
[^float]: The PS2 float rules as ps2-core implements them.
[^test]: The conformance test.
