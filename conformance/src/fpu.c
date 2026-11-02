#include "conformance.h"

#define CASES 160

/* fd = op(fs, ft) */
#define FPU2(op, text)                                                   \
    static u32 f_##op(u32 a, u32 b)                                      \
    {                                                                    \
        u32 r;                                                           \
        __asm__ volatile("mtc1 %1, $f0\n\tmtc1 %2, $f1\n\t" text         \
                         "\n\tmfc1 %0, $f2"                              \
                         : "=r"(r)                                       \
                         : "r"(a), "r"(b)                                \
                         : "$f0", "$f1", "$f2");                         \
        return r;                                                        \
    }

FPU2(add, "add.s $f2, $f0, $f1")
FPU2(sub, "sub.s $f2, $f0, $f1")
FPU2(mul, "mul.s $f2, $f0, $f1")
FPU2(div, "div.s $f2, $f0, $f1")
FPU2(max, "max.s $f2, $f0, $f1")
FPU2(min, "min.s $f2, $f0, $f1")
FPU2(rsqrt, "rsqrt.s $f2, $f0, $f1")
FPU2(sqrt, "sqrt.s $f2, $f1")
FPU2(abs, "abs.s $f2, $f0")
FPU2(neg, "neg.s $f2, $f0")
FPU2(cvtw, "cvt.w.s $f2, $f0")
FPU2(cvts, "cvt.s.w $f2, $f0")
/* Accumulator: (a * b) then + or - b * a2 (a2 = a + b bits mixed). */
FPU2(madd, "mula.s $f0, $f1\n\tmadd.s $f2, $f1, $f0")
FPU2(msub, "adda.s $f0, $f1\n\tmsub.s $f2, $f1, $f0")
FPU2(madda, "mula.s $f0, $f1\n\tmadda.s $f0, $f0\n\tadd.s $f2, $f0, $f0\n\tsuba.s $f0, $f1\n\tmsuba.s $f1, $f1\n\tmula.s $f0, $f0")

static u32 compare(int which, u32 a, u32 b)
{
    u32 r;
    switch (which) {
    case 0:
        __asm__ volatile("mtc1 %1, $f0\n\tmtc1 %2, $f1\n\tc.eq.s $f0, $f1\n\tcfc1 %0, $31" : "=r"(r) : "r"(a), "r"(b) : "$f0", "$f1");
        break;
    case 1:
        __asm__ volatile("mtc1 %1, $f0\n\tmtc1 %2, $f1\n\tc.lt.s $f0, $f1\n\tcfc1 %0, $31" : "=r"(r) : "r"(a), "r"(b) : "$f0", "$f1");
        break;
    default:
        __asm__ volatile("mtc1 %1, $f0\n\tmtc1 %2, $f1\n\tc.le.s $f0, $f1\n\tcfc1 %0, $31" : "=r"(r) : "r"(a), "r"(b) : "$f0", "$f1");
        break;
    }
    return (r >> 23) & 1;
}

static const struct {
    const char *name;
    u32 (*f)(u32, u32);
} ops[] = {
    {"add.s", f_add}, {"sub.s", f_sub}, {"mul.s", f_mul}, {"div.s", f_div},
    {"max.s", f_max}, {"min.s", f_min}, {"rsqrt.s", f_rsqrt}, {"sqrt.s", f_sqrt},
    {"abs.s", f_abs}, {"neg.s", f_neg}, {"cvt.w.s", f_cvtw}, {"cvt.s.w", f_cvts},
    {"madd.s", f_madd}, {"msub.s", f_msub}, {"madda.s", f_madda},
};

/* Inputs worth pinning: ones, the limits, exponent 255, denormals, and
 * pairs that show rounding and the adder's missing guard bits. */
static const u32 fixed[][2] = {
    {0x3F800000, 0x3F800000}, {0x3F800000, 0x30800000}, {0x3F800000, 0xB0800000},
    {0x3DCCCCCD, 0x40400000}, {0x7F7FFFFF, 0x40000000}, {0x7F800000, 0x3F800000},
    {0xFF800000, 0x3F800000}, {0x7FFFFFFF, 0x7FFFFFFF}, {0x00000001, 0x3F800000},
    {0x00800000, 0x3F000000}, {0x3F800000, 0x00000000}, {0x00000000, 0x00000000},
    {0x80000000, 0x00000000}, {0x4B800001, 0x3F800000}, {0x3F800001, 0xBF800000},
    {0x4F000000, 0x4F000000}, {0xCF000000, 0x3F800000}, {0x5F000000, 0x3F800000},
};

void test_fpu(void)
{
    unsigned i, j;
    for (i = 0; i < sizeof(ops) / sizeof(ops[0]); i++) {
        for (j = 0; j < CASES; j++) {
            u32 a, b;
            if (j < sizeof(fixed) / sizeof(fixed[0])) {
                a = fixed[j][0];
                b = fixed[j][1];
            } else {
                a = rnd_float();
                b = rnd_float();
            }
            out_str(ops[i].name);
            out_str(" ");
            out_hex32(a);
            out_str(" ");
            out_hex32(b);
            out_str(" -> ");
            out_hex32(ops[i].f(a, b));
            out_line_end();
        }
    }
    for (i = 0; i < 3; i++) {
        for (j = 0; j < CASES; j++) {
            u32 a = j < 18 ? fixed[j][0] : rnd_float();
            u32 b = j < 18 ? fixed[j][1] : rnd_float();
            out_str(i == 0 ? "c.eq.s " : i == 1 ? "c.lt.s " : "c.le.s ");
            out_hex32(a);
            out_str(" ");
            out_hex32(b);
            out_str(" -> ");
            out_hex32(compare(i, a, b));
            out_line_end();
        }
    }
}
