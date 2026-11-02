/* Conformance programs for sly-rs: run instructions over chosen and
 * pseudo-random inputs and print "name inputs -> results" lines in hex
 * through the EE debug port (0x1000F180), which PCSX2 logs and sly-rs
 * captures. The two transcripts are compared line by line. */
#ifndef CONFORMANCE_H
#define CONFORMANCE_H

typedef unsigned int u32;
typedef unsigned long long u64;
typedef unsigned int u128 __attribute__((mode(TI)));

void out_str(const char *s);
void out_hex32(u32 v);
void out_hex64(u64 v);
void out_hex128(const u128 *v);
void out_line_end(void);

/* Deterministic inputs. */
u32 rnd(void);
u32 rnd_float(void);
void rnd_quad(u128 *q);

void test_fpu(void);
void test_alu(void);
void test_mmi(void);
void test_vu0(void);
void test_flow(void);

#endif
