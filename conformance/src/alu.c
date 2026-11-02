#include "conformance.h"

#define CASES 48

static const u32 edges[] = {0, 1, 0xFFFFFFFF, 0x80000000, 0x7FFFFFFF, 2, 0xFFFFFFFE, 0x10000};

static u32 pick(unsigned j, unsigned k)
{
    return j < 64 ? edges[(j >> (3 * k)) & 7] : rnd();
}

/* mult/multu/div/divu/madd/madd1...: results rd, lo, hi. */
#define MULDIV(name, text)                                                       \
    static void m_##name(u32 a, u32 b, u64 out[3])                               \
    {                                                                            \
        u64 rd, lo, hi;                                                          \
        __asm__ volatile("mthi $zero\n\tmtlo $zero\n\tmthi1 $zero\n\tmtlo1 $zero\n\t" \
                         "addiu $8, $zero, 7\n\tmthi $8\n\tmtlo $8\n\tmthi1 $8\n\tmtlo1 $8\n\t" \
                         text "\n\t"                                             \
                         "mflo %1\n\tmfhi %2\n\tmflo1 $9\n\tmfhi1 $10\n\tdaddu %1, %1, $9\n\tdaddu %2, %2, $10" \
                         : "=r"(rd), "=r"(lo), "=r"(hi)                          \
                         : "r"(a), "r"(b)                                        \
                         : "$8", "$9", "$10", "hi", "lo");                       \
        out[0] = rd;                                                             \
        out[1] = lo;                                                             \
        out[2] = hi;                                                             \
    }

MULDIV(mult, "mult %0, %3, %4")
MULDIV(multu, "multu %0, %3, %4")
MULDIV(div, "div $zero, %3, %4\n\tdaddu %0, $zero, $zero")
MULDIV(divu, "divu $zero, %3, %4\n\tdaddu %0, $zero, $zero")
MULDIV(madd, "madd %0, %3, %4")
MULDIV(maddu, "maddu %0, %3, %4")
MULDIV(mult1, "mult1 %0, %3, %4")
MULDIV(div1, "div1 $zero, %3, %4\n\tdaddu %0, $zero, $zero")
MULDIV(madd1, "madd1 %0, %3, %4")

static const struct {
    const char *name;
    void (*f)(u32, u32, u64 *);
} muldiv[] = {
    {"mult", m_mult}, {"multu", m_multu}, {"div", m_div}, {"divu", m_divu},
    {"madd", m_madd}, {"maddu", m_maddu}, {"mult1", m_mult1}, {"div1", m_div1}, {"madd1", m_madd1},
};

static unsigned char buffer[32] __attribute__((aligned(16)));

static void fill(void)
{
    unsigned i;
    for (i = 0; i < sizeof(buffer); i++)
        buffer[i] = 0x7E + i; /* words with bit 31 set, to see sign extension */
}

#define UNALIGNED_LOAD(name, text)                                              \
    static u64 l_##name(unsigned offset, u64 old)                               \
    {                                                                           \
        u64 r = old;                                                            \
        __asm__ volatile(text " %0, 0(%1)" : "+r"(r) : "r"(buffer + offset) : "memory"); \
        return r;                                                               \
    }

UNALIGNED_LOAD(lwl, "lwl")
UNALIGNED_LOAD(lwr, "lwr")
UNALIGNED_LOAD(ldl, "ldl")
UNALIGNED_LOAD(ldr, "ldr")

#define UNALIGNED_STORE(name, text)                                            \
    static void s_##name(unsigned offset, u64 value)                           \
    {                                                                          \
        __asm__ volatile(text " %0, 0(%1)" : : "r"(value), "r"(buffer + offset) : "memory"); \
    }

UNALIGNED_STORE(swl, "swl")
UNALIGNED_STORE(swr, "swr")
UNALIGNED_STORE(sdl, "sdl")
UNALIGNED_STORE(sdr, "sdr")

void test_alu(void)
{
    unsigned i, j, k;
    for (i = 0; i < sizeof(muldiv) / sizeof(muldiv[0]); i++) {
        for (j = 0; j < 64 + CASES; j++) {
            u32 a = pick(j, 0), b = pick(j, 1);
            u64 out[3];
            muldiv[i].f(a, b, out);
            out_str(muldiv[i].name);
            out_str(" ");
            out_hex32(a);
            out_str(" ");
            out_hex32(b);
            out_str(" -> ");
            out_hex64(out[0]);
            out_str(" ");
            out_hex64(out[1]);
            out_str(" ");
            out_hex64(out[2]);
            out_line_end();
        }
    }
    for (k = 0; k < 8; k++) {
        static const char *names[] = {"lwl", "lwr", "ldl", "ldr"};
        u64 (*loads[])(unsigned, u64) = {l_lwl, l_lwr, l_ldl, l_ldr};
        for (i = 0; i < 4; i++) {
            fill();
            out_str(names[i]);
            out_str(" ");
            out_hex32(k);
            out_str(" -> ");
            out_hex64(loads[i](k, 0xA5A5A5A5C3C3C3C3ULL));
            out_line_end();
        }
    }
    for (k = 0; k < 8; k++) {
        static const char *names[] = {"swl", "swr", "sdl", "sdr"};
        void (*stores[])(unsigned, u64) = {s_swl, s_swr, s_sdl, s_sdr};
        for (i = 0; i < 4; i++) {
            u64 *q = (u64 *)buffer;
            fill();
            stores[i](k, 0x0123456789ABCDEFULL);
            out_str(names[i]);
            out_str(" ");
            out_hex32(k);
            out_str(" -> ");
            out_hex64(q[0]);
            out_str(" ");
            out_hex64(q[1]);
            out_line_end();
        }
    }
}
