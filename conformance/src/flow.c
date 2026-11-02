#include "conformance.h"

/* Control flow, for the recompiler more than the interpreter: branch-likely
 * loops, a branch into a delay slot, branch-and-link, a switch through a
 * jump table, calls through pointers, and recursion deeper than the
 * recompiled code nests on the host stack. */

static void line(const char *name, u32 in, u32 out)
{
    out_str(name);
    out_str(" ");
    out_hex32(in);
    out_str(" -> ");
    out_hex32(out);
    out_line_end();
}

/* bnel: the delay slot runs only while the loop continues. */
static u32 likely_loop(u32 n)
{
    u32 r = 0;
    __asm__ volatile(".set noreorder\n"
                     "1: addiu %0, %0, 3\n"
                     "   addiu %1, %1, -1\n"
                     "   bnel %1, $zero, 1b\n"
                     "   addiu %0, %0, 100\n"
                     ".set reorder\n"
                     : "+r"(r), "+r"(n));
    return r;
}

/* beql not taken skips its delay slot; taken runs it. */
static u32 likely_skip(u32 x)
{
    u32 r = 1;
    __asm__ volatile(".set noreorder\n"
                     "   beql %1, $zero, 1f\n"
                     "   addiu %0, %0, 10\n"
                     "   addiu %0, %0, 20\n"
                     "1: addiu %0, %0, 40\n"
                     ".set reorder\n"
                     : "+r"(r)
                     : "r"(x));
    return r;
}

/* Label 2 is both the delay slot of "b 3f" and the target of the beq. */
static u32 slot_target(u32 x)
{
    u32 r = 0;
    __asm__ volatile(".set noreorder\n"
                     "   beq %1, $zero, 2f\n"
                     "   nop\n"
                     "   b 3f\n"
                     "2: addiu %0, %0, 5\n"
                     "   addiu %0, %0, 7\n"
                     "3: addiu %0, %0, 1\n"
                     ".set reorder\n"
                     : "+r"(r)
                     : "r"(x));
    return r;
}

/* bgezal: a call through a branch, returning after its delay slot. */
static u32 branch_link(u32 x)
{
    u32 r = x;
    __asm__ volatile(".set noreorder\n"
                     "   bgezal $zero, 1f\n"
                     "   addiu %0, %0, 2\n"
                     "   b 2f\n"
                     "   addiu %0, %0, 4\n"
                     "1: jr $ra\n"
                     "   sll %0, %0, 1\n"
                     "2:\n"
                     ".set reorder\n"
                     : "+r"(r)
                     :
                     : "$31");
    return r;
}

static u32 __attribute__((noinline)) dispatch(u32 x)
{
    switch (x & 15) {
    case 0: return x + 1;
    case 1: return x * 3;
    case 2: return x ^ 0x55;
    case 3: return x - 7;
    case 4: return x << 2;
    case 5: return x >> 1;
    case 6: return ~x;
    case 7: return x | 0x100;
    case 8: return x & 0xF0F0;
    case 9: return x * x;
    case 10: return x + 0x1234;
    case 11: return x ^ 0xFFFF;
    default: return x;
    }
}

static u32 twice(u32 x) { return x * 2; }
static u32 square(u32 x) { return x * x; }
static u32 negate(u32 x) { return -x; }

static u32 __attribute__((noinline)) depth(u32 n)
{
    return n == 0 ? 0 : 3 + depth(n - 1);
}

void test_flow(void)
{
    static u32 (*const fns[])(u32) = {twice, square, negate};
    u32 i;
    for (i = 1; i < 6; i++)
        line("flow_likely_loop", i, likely_loop(i));
    for (i = 0; i < 2; i++) {
        line("flow_likely_skip", i, likely_skip(i));
        line("flow_slot_target", i, slot_target(i));
        line("flow_branch_link", i + 5, branch_link(i + 5));
    }
    for (i = 0; i < 40; i++)
        line("flow_switch", i * 7, dispatch(i * 7));
    for (i = 0; i < 9; i++)
        line("flow_pointer", i, fns[i % 3](i + 11));
    for (i = 0; i < 3; i++)
        line("flow_depth", 700 * i + 100, depth(700 * i + 100));
}
