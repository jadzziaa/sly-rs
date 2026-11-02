#include "conformance.h"

static void put(char c)
{
    *(volatile char *)0x1000F180 = c;
}

void out_str(const char *s)
{
    while (*s)
        put(*s++);
}

void out_hex32(u32 v)
{
    int i;
    for (i = 28; i >= 0; i -= 4)
        put("0123456789abcdef"[(v >> i) & 15]);
}

void out_hex64(u64 v)
{
    out_hex32((u32)(v >> 32));
    out_hex32((u32)v);
}

void out_hex128(const u128 *v)
{
    const u32 *w = (const u32 *)v;
    out_hex32(w[3]);
    out_hex32(w[2]);
    out_hex32(w[1]);
    out_hex32(w[0]);
}

void out_line_end(void)
{
    put('\n');
}

static u32 state = 0x2545F491;

u32 rnd(void)
{
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    return state;
}

/* Floats with the exponents that matter on the PS2: zero/denormal, the
 * smallest normals, around 1.0, large, and 255 (no Inf/NaN on the PS2). */
u32 rnd_float(void)
{
    static const u32 exponents[] = {0, 1, 2, 0x67, 0x7E, 0x7F, 0x80, 0x96, 0x97, 0xFD, 0xFE, 0xFF};
    u32 r = rnd();
    u32 e = (r & 3) == 0 ? (rnd() & 0xFF) : exponents[(r >> 2) % 12];
    u32 m = rnd() & 0x7FFFFF;
    if ((r >> 8 & 7) == 0)
        m = 0;
    if ((r >> 8 & 7) == 1)
        m = 0x7FFFFF;
    return (r & 0x80000000) | (e << 23) | m;
}

void rnd_quad(u128 *q)
{
    u32 *w = (u32 *)q;
    w[0] = rnd();
    w[1] = rnd();
    w[2] = rnd();
    w[3] = rnd();
}
