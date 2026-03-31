/*
 * bfpp_rt_3d_math.c — Q16.16 fixed-point math library for BF++ 3D rendering.
 *
 * Format: signed 32-bit integer, upper 16 = integer, lower 16 = fraction.
 *   1.0 = 0x00010000 (65536), precision = 1/65536 ≈ 0.0000153
 *
 * All functions operate on the BF++ tape via (uint8_t *tape, int ptr).
 * Values are little-endian, 4 bytes per Q16.16 cell.
 *
 * Matrix layout: column-major 4x4 (OpenGL convention), 16 consecutive
 * Q16.16 values = 64 bytes on tape.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE  /* M_PI */
#endif
#include "bfpp_rt_3d.h"
#include <stdint.h>
#include <string.h>
#include <math.h>

/* ── Tape access helpers ─────────────────────────────────────── */

static inline int32_t tape_q16(uint8_t *tape, int addr)
{
    return (int32_t)((uint32_t)tape[addr] |
                     ((uint32_t)tape[addr+1] << 8) |
                     ((uint32_t)tape[addr+2] << 16) |
                     ((uint32_t)tape[addr+3] << 24));
}

static inline void tape_set_q16(uint8_t *tape, int addr, int32_t val)
{
    tape[addr]   =  val        & 0xFF;
    tape[addr+1] = (val >> 8)  & 0xFF;
    tape[addr+2] = (val >> 16) & 0xFF;
    tape[addr+3] = (val >> 24) & 0xFF;
}

/* ── Q16.16 constants ────────────────────────────────────────── */

#define Q16_ONE   65536
#define Q16_PI    205887       /* π × 65536 ≈ 205887.416 */
#define Q16_2PI   411775       /* 2π × 65536 ≈ 411774.832 */
#define Q16_HALF_PI 102944     /* π/2 × 65536 ≈ 102943.708 */

/* ── Sin lookup table (quarter-wave, 1024 entries) ───────────── */

static int32_t sin_table[1024];
static int sin_table_initialized = 0;

static void init_sin_table(void)
{
    if (sin_table_initialized) return;
    for (int i = 0; i < 1024; i++) {
        sin_table[i] = (int32_t)(sin(i * M_PI / 2048.0) * 65536.0);
    }
    sin_table_initialized = 1;
}

/*
 * Quarter-wave sin lookup. Input: Q16.16 angle in radians.
 * Normalize to [0, 2π), index into 4096-entry virtual table via symmetry.
 */
static int32_t q16_sin(int32_t angle)
{
    init_sin_table();

    /* Normalize angle to [0, Q16_2PI) */
    angle = angle % Q16_2PI;
    if (angle < 0) angle += Q16_2PI;

    /* Map angle to table index [0, 4096) — full circle = 4096 steps */
    /* index = angle * 4096 / Q16_2PI */
    int idx = (int)(((int64_t)angle * 4096) / Q16_2PI);
    if (idx < 0) idx = 0;
    if (idx >= 4096) idx = 4095;

    /* Quarter-wave symmetry:
     *   Q0 [0,1024):    sin_table[idx]
     *   Q1 [1024,2048): sin_table[2047-idx]
     *   Q2 [2048,3072): -sin_table[idx-2048]
     *   Q3 [3072,4096): -sin_table[4095-idx]
     */
    if (idx < 1024) {
        return sin_table[idx];
    } else if (idx < 2048) {
        return sin_table[2047 - idx];
    } else if (idx < 3072) {
        return -sin_table[idx - 2048];
    } else {
        return -sin_table[4095 - idx];
    }
}

static int32_t q16_cos(int32_t angle)
{
    return q16_sin(angle + Q16_HALF_PI);
}

/* Fixed-point multiply: (a * b) >> 16 */
static int32_t q16_mul(int32_t a, int32_t b)
{
    return (int32_t)(((int64_t)a * b) >> 16);
}

/* Fixed-point divide: (a << 16) / b */
static int32_t q16_div(int32_t a, int32_t b)
{
    if (b == 0) return 0x7FFFFFFF;
    return (int32_t)(((int64_t)a << 16) / b);
}

/* ── Scalar operations ───────────────────────────────────────── */

/* __fp_mul: tape[ptr] = tape[ptr] * tape[ptr+4] */
void bfpp_fp_mul(uint8_t *tape, int ptr)
{
    int32_t a = tape_q16(tape, ptr);
    int32_t b = tape_q16(tape, ptr + 4);
    tape_set_q16(tape, ptr, q16_mul(a, b));
}

/* __fp_div: tape[ptr] = tape[ptr] / tape[ptr+4], saturates on div-by-zero */
void bfpp_fp_div(uint8_t *tape, int ptr)
{
    int32_t a = tape_q16(tape, ptr);
    int32_t b = tape_q16(tape, ptr + 4);
    tape_set_q16(tape, ptr, q16_div(a, b));
}

/* ── Trigonometry ────────────────────────────────────────────── */

/* __fp_sin: tape[ptr] = sin(tape[ptr]), angle in Q16.16 radians */
void bfpp_fp_sin(uint8_t *tape, int ptr)
{
    int32_t angle = tape_q16(tape, ptr);
    tape_set_q16(tape, ptr, q16_sin(angle));
}

/* __fp_cos: tape[ptr] = cos(tape[ptr]), angle in Q16.16 radians */
void bfpp_fp_cos(uint8_t *tape, int ptr)
{
    int32_t angle = tape_q16(tape, ptr);
    tape_set_q16(tape, ptr, q16_cos(angle));
}

/* __fp_sqrt: tape[ptr] = sqrt(tape[ptr]) via Newton's method (4 iterations) */
void bfpp_fp_sqrt(uint8_t *tape, int ptr)
{
    int32_t x = tape_q16(tape, ptr);

    if (x <= 0) {
        tape_set_q16(tape, ptr, 0);
        return;
    }

    /* Initial guess via floating-point, then refine in fixed-point */
    int32_t guess = (int32_t)(sqrt((double)x / 65536.0) * 65536.0);
    if (guess <= 0) guess = Q16_ONE;

    /* Newton's method: guess = (guess + x/guess) / 2 */
    for (int i = 0; i < 4; i++) {
        int32_t div = q16_div(x, guess);
        guess = (guess + div) >> 1;
    }

    tape_set_q16(tape, ptr, guess);
}

/* ── Matrix helpers (internal) ───────────────────────────────── */

/* Read 4x4 matrix from tape into local array */
static void mat4_read(uint8_t *tape, int addr, int32_t *m)
{
    for (int i = 0; i < 16; i++) {
        m[i] = tape_q16(tape, addr + i * 4);
    }
}

/* Write 4x4 matrix from local array to tape */
static void mat4_write(uint8_t *tape, int addr, const int32_t *m)
{
    for (int i = 0; i < 16; i++) {
        tape_set_q16(tape, addr + i * 4, m[i]);
    }
}

/* Write identity matrix to local array */
static void mat4_set_identity(int32_t *m)
{
    memset(m, 0, 16 * sizeof(int32_t));
    m[0] = m[5] = m[10] = m[15] = Q16_ONE;
}

/* ── Matrix operations ───────────────────────────────────────── */

/*
 * __mat4_identity: write identity matrix at tape address read from tape[ptr].
 * The address is a raw integer (not Q16.16).
 */
void bfpp_mat4_identity(uint8_t *tape, int ptr)
{
    int addr = tape_q16(tape, ptr);
    memset(&tape[addr], 0, 64);
    tape_set_q16(tape, addr +  0, Q16_ONE);  /* [0][0] */
    tape_set_q16(tape, addr + 20, Q16_ONE);  /* [1][1] */
    tape_set_q16(tape, addr + 40, Q16_ONE);  /* [2][2] */
    tape_set_q16(tape, addr + 60, Q16_ONE);  /* [3][3] */
}

/*
 * __mat4_multiply: dst = A × B (standard 4x4, column-major).
 * tape[ptr+0] = dst addr, tape[ptr+4] = A addr, tape[ptr+8] = B addr.
 * Uses int64_t intermediates to avoid overflow in multiply-accumulate.
 */
void bfpp_mat4_multiply(uint8_t *tape, int ptr)
{
    int dst_addr = tape_q16(tape, ptr);
    int a_addr   = tape_q16(tape, ptr + 4);
    int b_addr   = tape_q16(tape, ptr + 8);

    int32_t a[16], b[16], dst[16];
    mat4_read(tape, a_addr, a);
    mat4_read(tape, b_addr, b);

    /* Column-major: dst[col*4+row] = sum over k of a[k*4+row] * b[col*4+k] */
    for (int col = 0; col < 4; col++) {
        for (int row = 0; row < 4; row++) {
            int64_t sum = 0;
            for (int k = 0; k < 4; k++) {
                sum += (int64_t)a[k * 4 + row] * b[col * 4 + k];
            }
            dst[col * 4 + row] = (int32_t)(sum >> 16);
        }
    }

    mat4_write(tape, dst_addr, dst);
}

/*
 * __mat4_rotate: build rotation matrix at dst.
 * tape[ptr+0] = dst addr, tape[ptr+4] = angle (Q16.16 radians),
 * tape[ptr+8] = axis (0=X, 1=Y, 2=Z, raw integer).
 */
void bfpp_mat4_rotate(uint8_t *tape, int ptr)
{
    int dst_addr = tape_q16(tape, ptr);
    int32_t angle = tape_q16(tape, ptr + 4);
    int axis = tape_q16(tape, ptr + 8);

    int32_t c = q16_cos(angle);
    int32_t s = q16_sin(angle);

    int32_t m[16];
    mat4_set_identity(m);

    switch (axis) {
    case 0: /* X-axis rotation */
        m[5]  =  c;  m[9]  = -s;
        m[6]  =  s;  m[10] =  c;
        break;
    case 1: /* Y-axis rotation */
        m[0]  =  c;  m[8]  =  s;
        m[2]  = -s;  m[10] =  c;
        break;
    case 2: /* Z-axis rotation */
        m[0]  =  c;  m[4]  = -s;
        m[1]  =  s;  m[5]  =  c;
        break;
    }

    mat4_write(tape, dst_addr, m);
}

/*
 * __mat4_translate: build translation matrix at dst.
 * tape[ptr+0] = dst addr, tape[ptr+4/8/12] = x/y/z (Q16.16).
 */
void bfpp_mat4_translate(uint8_t *tape, int ptr)
{
    int dst_addr = tape_q16(tape, ptr);
    int32_t tx = tape_q16(tape, ptr + 4);
    int32_t ty = tape_q16(tape, ptr + 8);
    int32_t tz = tape_q16(tape, ptr + 12);

    int32_t m[16];
    mat4_set_identity(m);

    /* Column-major: translation goes in column 3 (indices 12,13,14) */
    m[12] = tx;
    m[13] = ty;
    m[14] = tz;

    mat4_write(tape, dst_addr, m);
}

/*
 * __mat4_perspective: build perspective projection matrix at dst.
 * tape[ptr+0] = dst addr, tape[ptr+4] = fov (Q16.16 radians),
 * tape[ptr+8] = aspect, tape[ptr+12] = near, tape[ptr+16] = far.
 * All Q16.16 except dst addr (raw integer).
 *
 * Standard OpenGL perspective matrix (column-major):
 *   [0]  = f/aspect    [4]  = 0    [8]  = 0                  [12] = 0
 *   [1]  = 0           [5]  = f    [9]  = 0                  [13] = 0
 *   [2]  = 0           [6]  = 0    [10] = (far+near)/(near-far) [14] = 2*far*near/(near-far)
 *   [3]  = 0           [7]  = 0    [11] = -1                 [15] = 0
 *
 * where f = 1 / tan(fov/2)
 */
void bfpp_mat4_perspective(uint8_t *tape, int ptr)
{
    int dst_addr = tape_q16(tape, ptr);
    int32_t fov    = tape_q16(tape, ptr + 4);
    int32_t aspect = tape_q16(tape, ptr + 8);
    int32_t near   = tape_q16(tape, ptr + 12);
    int32_t far    = tape_q16(tape, ptr + 16);

    /* f = 1 / tan(fov/2) = cos(fov/2) / sin(fov/2) */
    int32_t half_fov = fov >> 1;
    int32_t s = q16_sin(half_fov);
    int32_t c = q16_cos(half_fov);
    int32_t f = q16_div(c, s);

    int32_t nf_diff = near - far;   /* near - far (Q16.16) */

    int32_t m[16];
    memset(m, 0, sizeof(m));

    m[0]  = q16_div(f, aspect);
    m[5]  = f;
    m[10] = q16_div(far + near, nf_diff);
    m[11] = -Q16_ONE;
    /* m[14] = 2*far*near / (near-far) */
    {
        int32_t fn = q16_mul(far, near);
        m[14] = q16_div(fn << 1, nf_diff);
    }

    mat4_write(tape, dst_addr, m);
}
