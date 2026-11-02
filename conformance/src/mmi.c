#include "conformance.h"

#define CASES 24

typedef void (*mmi_fn)(const u128 *, const u128 *, u128 *);

/* rd = op(rs, rt) */
#define MMI3(op)                                                                 \
    static void t_##op(const u128 *a, const u128 *b, u128 *r)                    \
    {                                                                            \
        __asm__ volatile("lq $8, 0(%0)\n\tlq $9, 0(%1)\n\t" #op " $10, $8, $9\n\tsq $10, 0(%2)" \
                         : : "r"(a), "r"(b), "r"(r) : "$8", "$9", "$10", "memory"); \
    }
/* rd = op(rt), or plzcw rd = op(rs); rd starts as the second input, so
 * any part of rd the operation leaves alone shows. */
#define MMI2(op)                                                                 \
    static void t_##op(const u128 *a, const u128 *b, u128 *r)                    \
    {                                                                            \
        __asm__ volatile("lq $8, 0(%0)\n\tlq $10, 0(%1)\n\t" #op " $10, $8\n\tsq $10, 0(%2)" \
                         : : "r"(a), "r"(b), "r"(r) : "$8", "$10", "memory");     \
    }
/* rd = op(rt, sa) with sa from the low bits of the second input */
#define MMISA(op, sa)                                                            \
    static void t_##op##_##sa(const u128 *a, const u128 *b, u128 *r)             \
    {                                                                            \
        (void)b;                                                                 \
        __asm__ volatile("lq $8, 0(%0)\n\t" #op " $10, $8, " #sa "\n\tsq $10, 0(%1)" \
                         : : "r"(a), "r"(r) : "$8", "$10", "memory");             \
    }

MMI3(paddw) MMI3(psubw) MMI3(pcgtw) MMI3(pmaxw) MMI3(paddh) MMI3(psubh) MMI3(pcgth) MMI3(pmaxh)
MMI3(paddb) MMI3(psubb) MMI3(pcgtb) MMI3(paddsw) MMI3(psubsw) MMI3(pextlw) MMI3(ppacw) MMI3(paddsh)
MMI3(psubsh) MMI3(pextlh) MMI3(ppach) MMI3(paddsb) MMI3(psubsb) MMI3(pextlb) MMI3(ppacb) MMI3(pceqw)
MMI3(pminw) MMI3(padsbh) MMI3(pceqh) MMI3(pminh) MMI3(pceqb) MMI3(padduw) MMI3(psubuw) MMI3(pextuw)
MMI3(padduh) MMI3(psubuh) MMI3(pextuh) MMI3(paddub) MMI3(psubub) MMI3(pextub) MMI3(pinth) MMI3(pcpyld)
MMI3(pand) MMI3(pxor) MMI3(pinteh) MMI3(pcpyud) MMI3(por) MMI3(pnor) MMI3(psllvw) MMI3(psrlvw) MMI3(psravw)
MMI2(pext5) MMI2(ppac5) MMI2(pabsw) MMI2(pabsh) MMI2(pexeh) MMI2(prevh) MMI2(pexew) MMI2(prot3w)
MMI2(pexch) MMI2(pcpyh) MMI2(pexcw) MMI2(plzcw)
MMISA(psllh, 3) MMISA(psrlh, 7) MMISA(psrah, 15) MMISA(psllw, 1) MMISA(psrlw, 17) MMISA(psraw, 31)

#define QFSRV(n)                                                                 \
    static void t_qfsrv_##n(const u128 *a, const u128 *b, u128 *r)               \
    {                                                                            \
        __asm__ volatile("lq $8, 0(%0)\n\tlq $9, 0(%1)\n\tmtsab $zero, " #n "\n\tqfsrv $10, $8, $9\n\tsq $10, 0(%2)" \
                         : : "r"(a), "r"(b), "r"(r) : "$8", "$9", "$10", "memory"); \
    }
QFSRV(0) QFSRV(5) QFSRV(15)

static const struct {
    const char *name;
    mmi_fn f;
} ops[] = {
#define E(op) {#op, t_##op}
    E(paddw), E(psubw), E(pcgtw), E(pmaxw), E(paddh), E(psubh), E(pcgth), E(pmaxh),
    E(paddb), E(psubb), E(pcgtb), E(paddsw), E(psubsw), E(pextlw), E(ppacw), E(paddsh),
    E(psubsh), E(pextlh), E(ppach), E(paddsb), E(psubsb), E(pextlb), E(ppacb), E(pceqw),
    E(pminw), E(padsbh), E(pceqh), E(pminh), E(pceqb), E(padduw), E(psubuw), E(pextuw),
    E(padduh), E(psubuh), E(pextuh), E(paddub), E(psubub), E(pextub), E(pinth), E(pcpyld),
    E(pand), E(pxor), E(pinteh), E(pcpyud), E(por), E(pnor), E(psllvw), E(psrlvw), E(psravw),
    E(pext5), E(ppac5), E(pabsw), E(pabsh), E(pexeh), E(prevh), E(pexew), E(prot3w),
    E(pexch), E(pcpyh), E(pexcw), E(plzcw),
    E(psllh_3), E(psrlh_7), E(psrah_15), E(psllw_1), E(psrlw_17), E(psraw_31),
    E(qfsrv_0), E(qfsrv_5), E(qfsrv_15),
#undef E
};

/* HI/LO forms: set HI and LO, run, report rd, HI, LO. */
#define HILO(op, text)                                                           \
    static void h_##op(const u128 *in, u128 *out)                                \
    {                                                                            \
        __asm__ volatile("lq $8, 0(%0)\n\tlq $9, 16(%0)\n\tlq $11, 32(%0)\n\tlq $12, 48(%0)\n\t" \
                         "pmthi $11\n\tpmtlo $12\n\taddiu $10, $zero, 0\n\t" text "\n\t" \
                         "pmfhi $11\n\tpmflo $12\n\tsq $10, 0(%1)\n\tsq $11, 16(%1)\n\tsq $12, 32(%1)" \
                         : : "r"(in), "r"(out) : "$8", "$9", "$10", "$11", "$12", "hi", "lo", "memory"); \
    }

HILO(pmultw, "pmultw $10, $8, $9")
HILO(pmultuw, "pmultuw $10, $8, $9")
HILO(pmaddw, "pmaddw $10, $8, $9")
HILO(pmadduw, "pmadduw $10, $8, $9")
HILO(pmsubw, "pmsubw $10, $8, $9")
HILO(pmulth, "pmulth $10, $8, $9")
HILO(pmaddh, "pmaddh $10, $8, $9")
HILO(pmsubh, "pmsubh $10, $8, $9")
HILO(phmadh, "phmadh $10, $8, $9")
HILO(phmsbh, "phmsbh $10, $8, $9")
HILO(pdivw, "pdivw $8, $9")
HILO(pdivuw, "pdivuw $8, $9")
HILO(pdivbw, "pdivbw $8, $9")
HILO(pmfhl_lw, "pmfhl.lw $10")
HILO(pmfhl_uw, "pmfhl.uw $10")
HILO(pmfhl_slw, "pmfhl.slw $10")
HILO(pmfhl_lh, "pmfhl.lh $10")
HILO(pmfhl_sh, "pmfhl.sh $10")
HILO(pmthl_lw, "pmthl.lw $8")

static const struct {
    const char *name;
    void (*f)(const u128 *, u128 *);
} hilo[] = {
#define E(op) {#op, h_##op}
    E(pmultw), E(pmultuw), E(pmaddw), E(pmadduw), E(pmsubw), E(pmulth), E(pmaddh), E(pmsubh),
    E(phmadh), E(phmsbh), E(pdivw), E(pdivuw), E(pdivbw), E(pmfhl_lw), E(pmfhl_uw), E(pmfhl_slw),
    E(pmfhl_lh), E(pmfhl_sh), E(pmthl_lw),
#undef E
};

void test_mmi(void)
{
    static u128 a __attribute__((aligned(16))), b __attribute__((aligned(16))), r __attribute__((aligned(16)));
    static u128 in[4] __attribute__((aligned(16))), out[3] __attribute__((aligned(16)));
    unsigned i, j, k;
    for (i = 0; i < sizeof(ops) / sizeof(ops[0]); i++) {
        for (j = 0; j < CASES; j++) {
            rnd_quad(&a);
            rnd_quad(&b);
            if (j == 0)
                b = a;
            ops[i].f(&a, &b, &r);
            out_str(ops[i].name);
            out_str(" ");
            out_hex128(&a);
            out_str(" ");
            out_hex128(&b);
            out_str(" -> ");
            out_hex128(&r);
            out_line_end();
        }
    }
    for (i = 0; i < sizeof(hilo) / sizeof(hilo[0]); i++) {
        for (j = 0; j < CASES; j++) {
            for (k = 0; k < 4; k++)
                rnd_quad(&in[k]);
            if (j == 1)
                in[1] = 0; /* division by zero */
            hilo[i].f(in, out);
            out_str(hilo[i].name);
            for (k = 0; k < 4; k++) {
                out_str(" ");
                out_hex128(&in[k]);
            }
            out_str(" ->");
            for (k = 0; k < 3; k++) {
                out_str(" ");
                out_hex128(&out[k]);
            }
            out_line_end();
        }
    }
}
