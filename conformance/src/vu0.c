#include "conformance.h"

#define CASES 40

/* vf3 = op(vf1, vf2) after ACC = vf1 * vf2 and Q, I set; report vf3,
 * status (vi16) and MAC (vi17). The vnops let the macro-mode pipeline
 * update the flags before cfc2 reads them: read at once, they would still
 * hold the previous instruction's (PCSX2 warns about exactly that). */
#define VNOPS "vnop\n\tvnop\n\tvnop\n\tvnop\n\t"

#define VU(name, text)                                                                \
    static void v_##name(const u128 *in, u128 *out, u32 *status, u32 *mac)            \
    {                                                                                 \
        u32 s, m;                                                                     \
        __asm__ volatile("lqc2 $vf1, 0(%2)\n\tlqc2 $vf2, 16(%2)\n\tlqc2 $vf3, 32(%2)\n\t" \
                         "lw $8, 48(%2)\n\tctc2 $8, $vi21\n\t"                          \
                         "vmula.xyzw $ACC, $vf1, $vf2\n\tvdiv $Q, $vf1x, $vf2y\n\tvwaitq\n\t" \
                         "ctc2 $zero, $vi16\n\t" text "\n\t" VNOPS                      \
                         "sqc2 $vf3, 0(%3)\n\tcfc2 %0, $vi16\n\tcfc2 %1, $vi17"          \
                         : "=r"(s), "=r"(m) : "r"(in), "r"(out) : "$8", "memory");        \
        *status = s;                                                                  \
        *mac = m;                                                                     \
    }

VU(vadd, "vadd.xyzw $vf3, $vf1, $vf2")
VU(vsub, "vsub.xyz $vf3, $vf1, $vf2")
VU(vmul, "vmul.xyzw $vf3, $vf1, $vf2")
VU(vmax, "vmax.xyzw $vf3, $vf1, $vf2")
VU(vmini, "vmini.yw $vf3, $vf1, $vf2")
VU(vaddx, "vaddx.xyzw $vf3, $vf1, $vf2x")
VU(vsubw, "vsubw.xz $vf3, $vf1, $vf2w")
VU(vmuly, "vmuly.xyzw $vf3, $vf1, $vf2y")
VU(vmadd, "vmadd.xyzw $vf3, $vf1, $vf2")
VU(vmsub, "vmsub.xyzw $vf3, $vf1, $vf2")
VU(vmaddz, "vmaddz.xyzw $vf3, $vf1, $vf2z")
VU(vmulq, "vmulq.xyzw $vf3, $vf1, $Q")
VU(vaddi, "vaddi.xyzw $vf3, $vf1, $I")
VU(vopmsub, "vopmula.xyz $ACC, $vf1, $vf2\n\tvopmsub.xyz $vf3, $vf2, $vf1")
VU(vmaddai, "vmaddai.xyzw $ACC, $vf1, $I\n\tvmadd.xyzw $vf3, $vf2, $vf2")
VU(vftoi0, "vftoi0.xyzw $vf3, $vf1")
VU(vftoi4, "vftoi4.xyzw $vf3, $vf1")
VU(vftoi12, "vftoi12.xyzw $vf3, $vf1")
VU(vftoi15, "vftoi15.xyzw $vf3, $vf1")
VU(vitof0, "vitof0.xyzw $vf3, $vf1")
VU(vitof4, "vitof4.xyzw $vf3, $vf1")
VU(vitof12, "vitof12.xyzw $vf3, $vf1")
VU(vitof15, "vitof15.xyzw $vf3, $vf1")
VU(vabs, "vabs.xyzw $vf3, $vf1")
VU(vmr32, "vmr32.xyzw $vf3, $vf1")
VU(vmove, "vmove.xzw $vf3, $vf1")
VU(vdiv, "vdiv $Q, $vf1z, $vf2w\n\tvwaitq\n\tcfc2 $8, $vi22\n\tqmtc2 $8, $vf3")
VU(vsqrt, "vsqrt $Q, $vf2x\n\tvwaitq\n\tcfc2 $8, $vi22\n\tqmtc2 $8, $vf3")
VU(vrsqrt, "vrsqrt $Q, $vf1y, $vf2z\n\tvwaitq\n\tcfc2 $8, $vi22\n\tqmtc2 $8, $vf3")
VU(vclipw, "ctc2 $zero, $vi18\n\tvclipw.xyz $vf1, $vf2w\n\tvclipw.xyz $vf2, $vf1w\n\t" VNOPS "cfc2 $8, $vi18\n\tqmtc2 $8, $vf3")

static const struct {
    const char *name;
    void (*f)(const u128 *, u128 *, u32 *, u32 *);
} ops[] = {
#define E(op) {#op, v_##op}
    E(vadd), E(vsub), E(vmul), E(vmax), E(vmini), E(vaddx), E(vsubw), E(vmuly), E(vmadd), E(vmsub),
    E(vmaddz), E(vmulq), E(vaddi), E(vopmsub), E(vmaddai), E(vftoi0), E(vftoi4), E(vftoi12), E(vftoi15),
    E(vitof0), E(vitof4), E(vitof12), E(vitof15), E(vabs), E(vmr32), E(vmove), E(vdiv), E(vsqrt),
    E(vrsqrt), E(vclipw),
#undef E
};

void test_vu0(void)
{
    static u128 in[4] __attribute__((aligned(16))), out __attribute__((aligned(16)));
    unsigned i, j, k;
    for (i = 0; i < sizeof(ops) / sizeof(ops[0]); i++) {
        for (j = 0; j < CASES; j++) {
            u32 *w = (u32 *)in;
            u32 status, mac;
            for (k = 0; k < 12; k++)
                w[k] = (i >= 19 && i <= 22 && k < 4) ? rnd() : rnd_float();
            w[12] = rnd_float();
            ops[i].f(in, &out, &status, &mac);
            out_str(ops[i].name);
            for (k = 0; k < 4; k++) {
                out_str(" ");
                out_hex128(&in[k]);
            }
            out_str(" -> ");
            out_hex128(&out);
            out_str(" ");
            out_hex32(status & 0xFFF);
            out_str(" ");
            out_hex32(mac & 0xFFFF);
            out_line_end();
        }
    }
}
