/*
 * bfpp_rt_3d_software.c — BF++ software rasterizer fallback
 *
 * Architecture:
 *   When OpenGL is unavailable, this module provides the same 3D intrinsic
 *   API via software rasterization. It writes directly to tape[fb_offset]
 *   (RGB24, row-major), then the existing 8-thread FB pipeline presents it.
 *
 *   Rendering pipeline:
 *     1. Vertex transform (MVP matrix multiply)          — single-threaded
 *     2. Perspective divide → NDC                        — single-threaded
 *     3. Viewport transform → screen coords              — single-threaded
 *     4. Strip-parallel rasterization (N worker threads)  — multi-threaded
 *        Each thread owns a horizontal strip of the framebuffer + z-buffer.
 *        Every triangle is submitted to all strip threads; each clips the
 *        bounding box to its row range. Non-overlapping writes — no atomics.
 *        Thread count = min(sysconf(_SC_NPROCESSORS_ONLN), 8).
 *     5. Per-pixel Blinn-Phong shading with interpolated normals
 *     6. Depth test against float z-buffer
 *
 *   Vertex format: 6 floats per vertex (pos.xyz, normal.xyz).
 *   All numeric values from BF++ tape are Q16.16 fixed-point in 32-bit
 *   cells (divide by 65536.0f to get float). Little-endian byte order.
 *
 *   SIMD: On x86_64, the inner rasterization loop evaluates 4 pixels at
 *   once via SSE edge function tests.
 */

#include "bfpp_rt_3d.h"
#include "bfpp_fb_pipeline.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <math.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>

#ifdef __x86_64__
#include <immintrin.h>
#endif

/* ── Q16.16 helpers ──────────────────────────────────────────── */

static inline int32_t tape_read_i32(const uint8_t *tape, int offset)
{
    int32_t v;
    memcpy(&v, tape + offset, 4);
    return v;
}

static inline uint32_t tape_read_u32(const uint8_t *tape, int offset)
{
    uint32_t v;
    memcpy(&v, tape + offset, 4);
    return v;
}

static inline float tape_read_q16(const uint8_t *tape, int offset)
{
    return (float)tape_read_i32(tape, offset) / 65536.0f;
}

/* ── Math helpers ────────────────────────────────────────────── */

/* Column-major 4x4 matrix × vec4 */
static void mat4_mul_vec4(const float m[16], const float v[4], float out[4])
{
    for (int i = 0; i < 4; i++) {
        out[i] = m[i]*v[0] + m[i+4]*v[1] + m[i+8]*v[2] + m[i+12]*v[3];
    }
}

/* Extract upper-left 3x3 from a 4x4 column-major matrix.
 * For rotation+uniform-scale transforms this IS the normal matrix
 * (skip true inverse-transpose for performance). */
static void compute_normal_matrix(const float model[16], float nm[9])
{
    nm[0] = model[0]; nm[1] = model[1]; nm[2] = model[2];
    nm[3] = model[4]; nm[4] = model[5]; nm[5] = model[6];
    nm[6] = model[8]; nm[7] = model[9]; nm[8] = model[10];
}

/* 3x3 matrix × vec3 */
static void mat3_mul_vec3(const float m[9], const float v[3], float out[3])
{
    out[0] = m[0]*v[0] + m[3]*v[1] + m[6]*v[2];
    out[1] = m[1]*v[0] + m[4]*v[1] + m[7]*v[2];
    out[2] = m[2]*v[0] + m[5]*v[1] + m[8]*v[2];
}

static float vec3_dot(const float a[3], const float b[3])
{
    return a[0]*b[0] + a[1]*b[1] + a[2]*b[2];
}

static void vec3_normalize(float v[3])
{
    float len = sqrtf(v[0]*v[0] + v[1]*v[1] + v[2]*v[2]);
    if (len > 1e-8f) {
        float inv = 1.0f / len;
        v[0] *= inv;
        v[1] *= inv;
        v[2] *= inv;
    }
}

static float clampf(float x, float lo, float hi)
{
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}

static int mini(int a, int b) { return a < b ? a : b; }
static int maxi(int a, int b) { return a > b ? a : b; }

/* ── Identity matrix ─────────────────────────────────────────── */

static void mat4_identity(float m[16])
{
    memset(m, 0, 16 * sizeof(float));
    m[0] = m[5] = m[10] = m[15] = 1.0f;
}

/* ── Software rasterizer state ───────────────────────────────── */

static struct {
    int width, height;
    uint8_t *tape;
    int fb_offset;
    float *zbuf;            /* depth buffer (width * height floats) */

    /* Current transforms */
    float mvp[16];          /* model-view-projection matrix */
    float model[16];        /* model matrix (for normals) */
    float normal_mat[9];    /* 3x3 normal matrix = transpose(inverse(model)) */

    /* Lighting */
    struct {
        float pos[3];
        float color[3];
        float intensity;
        int active;
    } lights[4];
    int num_lights;

    /* Material */
    float color[3];         /* object color */
    float ambient[3];       /* ambient color */

    /* Strip-parallel threading */
    pthread_t sw_threads[8];
    int sw_thread_count;
    atomic_int sw_strips_remaining;
    atomic_int sw_running;
    pthread_mutex_t sw_mutex;
    pthread_cond_t sw_frame_cv;
    pthread_cond_t sw_done_cv;

    /* Per-strip triangle data (shared, read-only during rasterization) */
    float *sw_screen;       /* transformed screen coords: 3 floats per vertex */
    float *sw_wnormals;     /* world-space normals: 3 floats per vertex */
    float *sw_wpos;         /* world-space positions: 3 floats per vertex */
    float *sw_w_clip;       /* clip-space W per vertex */
    int sw_tri_count;       /* number of triangles to rasterize */

    /* Per-strip bounds */
    struct {
        int y_start, y_end;
    } sw_strips[8];
} sw;

/* Forward declaration for strip worker thread */
static void *sw_strip_worker(void *arg);

/* ── Init / Cleanup ──────────────────────────────────────────── */

void bfpp_sw_init(int width, int height, uint8_t *tape, int fb_offset)
{
    memset(&sw, 0, sizeof(sw));
    sw.width     = width;
    sw.height    = height;
    sw.tape      = tape;
    sw.fb_offset = fb_offset;

    sw.zbuf = (float *)malloc((size_t)width * (size_t)height * sizeof(float));
    if (!sw.zbuf) {
        fprintf(stderr, "bfpp_sw: failed to allocate z-buffer\n");
        abort();
    }

    /* Default transforms: identity */
    mat4_identity(sw.mvp);
    mat4_identity(sw.model);
    compute_normal_matrix(sw.model, sw.normal_mat);

    /* Default material: white object, dim ambient */
    sw.color[0] = sw.color[1] = sw.color[2] = 1.0f;
    sw.ambient[0] = sw.ambient[1] = sw.ambient[2] = 0.1f;

    /* Default light: white, overhead, active */
    sw.lights[0].pos[0] = 0.0f;
    sw.lights[0].pos[1] = 5.0f;
    sw.lights[0].pos[2] = 5.0f;
    sw.lights[0].color[0] = 1.0f;
    sw.lights[0].color[1] = 1.0f;
    sw.lights[0].color[2] = 1.0f;
    sw.lights[0].intensity = 1.0f;
    sw.lights[0].active    = 1;
    sw.num_lights = 1;

    /* Strip-parallel threading: determine thread count, compute strip bounds */
    int ncpu = (int)sysconf(_SC_NPROCESSORS_ONLN);
    if (ncpu < 1) ncpu = 1;
    if (ncpu > 8) ncpu = 8;
    sw.sw_thread_count = ncpu;

    int rows_per_strip = height / ncpu;
    for (int i = 0; i < ncpu; i++) {
        sw.sw_strips[i].y_start = i * rows_per_strip;
        sw.sw_strips[i].y_end   = (i == ncpu - 1) ? height : (i + 1) * rows_per_strip;
    }

    atomic_init(&sw.sw_running, 1);
    atomic_init(&sw.sw_strips_remaining, 0);
    pthread_mutex_init(&sw.sw_mutex, NULL);
    pthread_cond_init(&sw.sw_frame_cv, NULL);
    pthread_cond_init(&sw.sw_done_cv, NULL);

    sw.sw_screen   = NULL;
    sw.sw_wnormals = NULL;
    sw.sw_wpos     = NULL;
    sw.sw_w_clip   = NULL;
    sw.sw_tri_count = 0;

    for (int i = 0; i < ncpu; i++) {
        pthread_create(&sw.sw_threads[i], NULL, sw_strip_worker, (void *)(intptr_t)i);
    }
}

void bfpp_sw_cleanup(void)
{
    /* Signal strip workers to exit */
    atomic_store(&sw.sw_running, 0);
    pthread_mutex_lock(&sw.sw_mutex);
    pthread_cond_broadcast(&sw.sw_frame_cv);
    pthread_mutex_unlock(&sw.sw_mutex);

    for (int i = 0; i < sw.sw_thread_count; i++) {
        pthread_join(sw.sw_threads[i], NULL);
    }

    pthread_mutex_destroy(&sw.sw_mutex);
    pthread_cond_destroy(&sw.sw_frame_cv);
    pthread_cond_destroy(&sw.sw_done_cv);

    free(sw.zbuf);
    sw.zbuf = NULL;
}

/* ── State setters (public API: read from tape) ──────────────── */

/*
 * bfpp_sw_set_mvp: tape[ptr+0..+60] = 16 Q16.16 values (column-major 4x4)
 */
void bfpp_sw_set_mvp(uint8_t *tape, int ptr)
{
    for (int i = 0; i < 16; i++) {
        sw.mvp[i] = tape_read_q16(tape, ptr + i * 4);
    }
}

/*
 * bfpp_sw_set_light: tape layout:
 *   ptr+0:  light index (uint32)
 *   ptr+4:  pos.x  (Q16.16)
 *   ptr+8:  pos.y  (Q16.16)
 *   ptr+12: pos.z  (Q16.16)
 *   ptr+16: color.r (Q16.16)
 *   ptr+20: color.g (Q16.16)
 *   ptr+24: color.b (Q16.16)
 *   ptr+28: intensity (Q16.16)
 */
void bfpp_sw_set_light(uint8_t *tape, int ptr)
{
    int idx = (int)tape_read_u32(tape, ptr);
    if (idx < 0 || idx >= 4) return;

    sw.lights[idx].pos[0]   = tape_read_q16(tape, ptr + 4);
    sw.lights[idx].pos[1]   = tape_read_q16(tape, ptr + 8);
    sw.lights[idx].pos[2]   = tape_read_q16(tape, ptr + 12);
    sw.lights[idx].color[0] = tape_read_q16(tape, ptr + 16);
    sw.lights[idx].color[1] = tape_read_q16(tape, ptr + 20);
    sw.lights[idx].color[2] = tape_read_q16(tape, ptr + 24);
    sw.lights[idx].intensity = tape_read_q16(tape, ptr + 28);
    sw.lights[idx].active    = 1;

    if (idx >= sw.num_lights)
        sw.num_lights = idx + 1;
}

/*
 * bfpp_sw_set_color: tape[ptr+0..+8] = r, g, b (Q16.16)
 */
void bfpp_sw_set_color(uint8_t *tape, int ptr)
{
    sw.color[0] = tape_read_q16(tape, ptr);
    sw.color[1] = tape_read_q16(tape, ptr + 4);
    sw.color[2] = tape_read_q16(tape, ptr + 8);
}

/* ── Internal setters (called by future dispatch layer) ──────── */

static void sw_set_model(const float model[16])
{
    memcpy(sw.model, model, 16 * sizeof(float));
    compute_normal_matrix(sw.model, sw.normal_mat);
}

static void sw_set_ambient(float r, float g, float b)
{
    sw.ambient[0] = r;
    sw.ambient[1] = g;
    sw.ambient[2] = b;
}

/* ── Clear ───────────────────────────────────────────────────── */

/*
 * bfpp_sw_clear: tape layout:
 *   ptr+0: r (Q16.16, only low byte used after conversion)
 *   ptr+4: g (Q16.16)
 *   ptr+8: b (Q16.16)
 *
 * Fills the framebuffer with the given color and resets the z-buffer.
 */
void bfpp_sw_clear(uint8_t *tape, int ptr)
{
    /* Read clear color from tape (Q16.16 → clamp to [0,255]) */
    int ri = (int)(tape_read_q16(tape, ptr)     * 255.0f + 0.5f);
    int gi = (int)(tape_read_q16(tape, ptr + 4) * 255.0f + 0.5f);
    int bi = (int)(tape_read_q16(tape, ptr + 8) * 255.0f + 0.5f);
    uint8_t r = (uint8_t)(ri < 0 ? 0 : (ri > 255 ? 255 : ri));
    uint8_t g = (uint8_t)(gi < 0 ? 0 : (gi > 255 ? 255 : gi));
    uint8_t b = (uint8_t)(bi < 0 ? 0 : (bi > 255 ? 255 : bi));

    /* Fill framebuffer with RGB triple */
    uint8_t *fb = sw.tape + sw.fb_offset;
    int pixels = sw.width * sw.height;

    if (r == g && g == b) {
        /* Uniform color: memset all 3 channels at once */
        memset(fb, r, (size_t)pixels * 3);
    } else {
        /* Build one row, memcpy to remaining rows */
        for (int x = 0; x < sw.width; x++) {
            fb[x * 3 + 0] = r;
            fb[x * 3 + 1] = g;
            fb[x * 3 + 2] = b;
        }
        int row_bytes = sw.width * 3;
        for (int y = 1; y < sw.height; y++) {
            memcpy(fb + y * row_bytes, fb, (size_t)row_bytes);
        }
    }

    /* Reset z-buffer to far plane */
    for (int i = 0; i < pixels; i++) {
        sw.zbuf[i] = 1e30f;
    }
}

/* ── Per-pixel Blinn-Phong shading ───────────────────────────── */

static void shade_pixel(const float normal[3], const float world_pos[3],
                        uint8_t *out_r, uint8_t *out_g, uint8_t *out_b)
{
    float n[3] = { normal[0], normal[1], normal[2] };
    vec3_normalize(n);

    /* Accumulate lighting: start with ambient */
    float lit[3] = {
        sw.ambient[0] * sw.color[0],
        sw.ambient[1] * sw.color[1],
        sw.ambient[2] * sw.color[2]
    };

    /* View direction: assume camera at origin looking down -Z */
    float view[3] = { -world_pos[0], -world_pos[1], -world_pos[2] };
    vec3_normalize(view);

    for (int i = 0; i < sw.num_lights; i++) {
        if (!sw.lights[i].active) continue;

        /* Light direction (from surface to light) */
        float L[3] = {
            sw.lights[i].pos[0] - world_pos[0],
            sw.lights[i].pos[1] - world_pos[1],
            sw.lights[i].pos[2] - world_pos[2]
        };
        vec3_normalize(L);

        /* Diffuse: N·L */
        float NdotL = vec3_dot(n, L);
        if (NdotL < 0.0f) NdotL = 0.0f;

        /* Blinn-Phong specular: N·H where H = normalize(L + V) */
        float H[3] = { L[0] + view[0], L[1] + view[1], L[2] + view[2] };
        vec3_normalize(H);
        float NdotH = vec3_dot(n, H);
        if (NdotH < 0.0f) NdotH = 0.0f;
        float spec = NdotH * NdotH;
        spec *= spec;       /* ^4 */
        spec *= spec;       /* ^8 */
        spec *= spec;       /* ^16 — moderate shininess */

        float intensity = sw.lights[i].intensity;
        for (int c = 0; c < 3; c++) {
            float lc = sw.lights[i].color[c] * intensity;
            lit[c] += sw.color[c] * lc * NdotL;    /* diffuse */
            lit[c] += lc * spec * 0.5f;             /* specular (half-strength) */
        }
    }

    /* Clamp and output */
    *out_r = (uint8_t)(clampf(lit[0], 0.0f, 1.0f) * 255.0f + 0.5f);
    *out_g = (uint8_t)(clampf(lit[1], 0.0f, 1.0f) * 255.0f + 0.5f);
    *out_b = (uint8_t)(clampf(lit[2], 0.0f, 1.0f) * 255.0f + 0.5f);
}

/* ── Triangle rasterization (scalar path) ────────────────────── */

/* Edge function: (b-a) × (p-a) — positive means p is on the left side */
static float edge_func(float ax, float ay, float bx, float by, float px, float py)
{
    return (bx - ax) * (py - ay) - (by - ay) * (px - ax);
}

/*
 * Rasterize a single triangle.
 *
 * screen[i] = { sx, sy, depth }  (3 floats per vertex, screen-space)
 * normals[i] = { nx, ny, nz }    (3 floats per vertex, world-space transformed)
 * world[i] = { wx, wy, wz }     (3 floats per vertex, world-space position)
 * w_clip[i] = clip-space W       (for perspective-correct interpolation)
 */
static void rasterize_triangle(const float screen[9],
                               const float normals[9],
                               const float world[9],
                               const float w_clip[3])
{
    float sx0 = screen[0], sy0 = screen[1], sz0 = screen[2];
    float sx1 = screen[3], sy1 = screen[4], sz1 = screen[5];
    float sx2 = screen[6], sy2 = screen[7], sz2 = screen[8];

    /* Bounding box, clamped to viewport */
    int min_x = maxi(0,             (int)floorf(fminf(sx0, fminf(sx1, sx2))));
    int max_x = mini(sw.width - 1,  (int)ceilf(fmaxf(sx0, fmaxf(sx1, sx2))));
    int min_y = maxi(0,             (int)floorf(fminf(sy0, fminf(sy1, sy2))));
    int max_y = mini(sw.height - 1, (int)ceilf(fmaxf(sy0, fmaxf(sy1, sy2))));

    if (min_x > max_x || min_y > max_y) return;

    /* Total area (2x signed area of triangle) */
    float area = edge_func(sx0, sy0, sx1, sy1, sx2, sy2);
    if (fabsf(area) < 1e-6f) return;   /* degenerate triangle */
    float inv_area = 1.0f / area;

    /* Reciprocal W for perspective-correct interpolation */
    float inv_w0 = 1.0f / w_clip[0];
    float inv_w1 = 1.0f / w_clip[1];
    float inv_w2 = 1.0f / w_clip[2];

    uint8_t *fb = sw.tape + sw.fb_offset;
    int stride = sw.width * 3;

#ifdef __x86_64__
    /* ── SSE4 path: evaluate 4 pixels at once ────────────────── */

    /* Edge equation increments for stepping +1 in X */
    float de0_dx = sy1 - sy2;
    float de1_dx = sy2 - sy0;
    float de2_dx = sy0 - sy1;

    for (int y = min_y; y <= max_y; y++) {
        float py = (float)y + 0.5f;

        /* Edge values at (min_x + 0.5, py) */
        float px_start = (float)min_x + 0.5f;
        float e0_start = edge_func(sx1, sy1, sx2, sy2, px_start, py);
        float e1_start = edge_func(sx2, sy2, sx0, sy0, px_start, py);
        float e2_start = edge_func(sx0, sy0, sx1, sy1, px_start, py);

        /* SSE: 4 consecutive X offsets */
        __m128 e0_base = _mm_set_ps(e0_start + 3*de0_dx, e0_start + 2*de0_dx,
                                     e0_start + de0_dx, e0_start);
        __m128 e1_base = _mm_set_ps(e1_start + 3*de1_dx, e1_start + 2*de1_dx,
                                     e1_start + de1_dx, e1_start);
        __m128 e2_base = _mm_set_ps(e2_start + 3*de2_dx, e2_start + 2*de2_dx,
                                     e2_start + de2_dx, e2_start);

        __m128 four_de0 = _mm_set1_ps(4.0f * de0_dx);
        __m128 four_de1 = _mm_set1_ps(4.0f * de1_dx);
        __m128 four_de2 = _mm_set1_ps(4.0f * de2_dx);

        __m128 zero = _mm_setzero_ps();

        int x = min_x;
        for (; x + 3 <= max_x; x += 4) {
            /* Inside test: all edges must have same sign as total area */
            __m128 inside;
            if (area > 0) {
                __m128 c0 = _mm_cmpge_ps(e0_base, zero);
                __m128 c1 = _mm_cmpge_ps(e1_base, zero);
                __m128 c2 = _mm_cmpge_ps(e2_base, zero);
                inside = _mm_and_ps(c0, _mm_and_ps(c1, c2));
            } else {
                __m128 c0 = _mm_cmple_ps(e0_base, zero);
                __m128 c1 = _mm_cmple_ps(e1_base, zero);
                __m128 c2 = _mm_cmple_ps(e2_base, zero);
                inside = _mm_and_ps(c0, _mm_and_ps(c1, c2));
            }

            int mask = _mm_movemask_ps(inside);
            if (mask) {
                /* At least one pixel is inside — process individually */
                float e0_arr[4], e1_arr[4], e2_arr[4];
                _mm_storeu_ps(e0_arr, e0_base);
                _mm_storeu_ps(e1_arr, e1_base);
                _mm_storeu_ps(e2_arr, e2_base);

                for (int k = 0; k < 4; k++) {
                    if (!(mask & (1 << k))) continue;

                    int px = x + k;
                    float w0 = e0_arr[k] * inv_area;
                    float w1 = e1_arr[k] * inv_area;
                    float w2 = 1.0f - w0 - w1;

                    /* Perspective-correct interpolation */
                    float persp = 1.0f / (w0*inv_w0 + w1*inv_w1 + w2*inv_w2);

                    /* Depth interpolation */
                    float depth = w0*sz0 + w1*sz1 + w2*sz2;

                    /* Z-test */
                    int zidx = y * sw.width + px;
                    if (depth >= sw.zbuf[zidx]) continue;
                    sw.zbuf[zidx] = depth;

                    /* Interpolate normal (perspective-correct) */
                    float pc0 = w0 * inv_w0 * persp;
                    float pc1 = w1 * inv_w1 * persp;
                    float pc2 = w2 * inv_w2 * persp;

                    float norm[3] = {
                        normals[0]*pc0 + normals[3]*pc1 + normals[6]*pc2,
                        normals[1]*pc0 + normals[4]*pc1 + normals[7]*pc2,
                        normals[2]*pc0 + normals[5]*pc1 + normals[8]*pc2
                    };

                    /* Interpolate world position */
                    float wpos[3] = {
                        world[0]*pc0 + world[3]*pc1 + world[6]*pc2,
                        world[1]*pc0 + world[4]*pc1 + world[7]*pc2,
                        world[2]*pc0 + world[5]*pc1 + world[8]*pc2
                    };

                    /* Shade and write */
                    uint8_t r, g, b;
                    shade_pixel(norm, wpos, &r, &g, &b);
                    int fb_idx = y * stride + px * 3;
                    fb[fb_idx + 0] = r;
                    fb[fb_idx + 1] = g;
                    fb[fb_idx + 2] = b;
                }
            }

            e0_base = _mm_add_ps(e0_base, four_de0);
            e1_base = _mm_add_ps(e1_base, four_de1);
            e2_base = _mm_add_ps(e2_base, four_de2);
        }

        /* Scalar tail for remaining pixels */
        float e0_scalar = e0_start + (float)(x - min_x) * de0_dx;
        float e1_scalar = e1_start + (float)(x - min_x) * de1_dx;
        float e2_scalar = e2_start + (float)(x - min_x) * de2_dx;

        for (; x <= max_x; x++) {
            int inside_scalar;
            if (area > 0)
                inside_scalar = (e0_scalar >= 0 && e1_scalar >= 0 && e2_scalar >= 0);
            else
                inside_scalar = (e0_scalar <= 0 && e1_scalar <= 0 && e2_scalar <= 0);

            if (inside_scalar) {
                float w0 = e0_scalar * inv_area;
                float w1 = e1_scalar * inv_area;
                float w2 = 1.0f - w0 - w1;

                float persp = 1.0f / (w0*inv_w0 + w1*inv_w1 + w2*inv_w2);
                float depth = w0*sz0 + w1*sz1 + w2*sz2;

                int zidx = y * sw.width + x;
                if (depth < sw.zbuf[zidx]) {
                    sw.zbuf[zidx] = depth;

                    float pc0 = w0 * inv_w0 * persp;
                    float pc1 = w1 * inv_w1 * persp;
                    float pc2 = w2 * inv_w2 * persp;

                    float norm[3] = {
                        normals[0]*pc0 + normals[3]*pc1 + normals[6]*pc2,
                        normals[1]*pc0 + normals[4]*pc1 + normals[7]*pc2,
                        normals[2]*pc0 + normals[5]*pc1 + normals[8]*pc2
                    };
                    float wpos[3] = {
                        world[0]*pc0 + world[3]*pc1 + world[6]*pc2,
                        world[1]*pc0 + world[4]*pc1 + world[7]*pc2,
                        world[2]*pc0 + world[5]*pc1 + world[8]*pc2
                    };

                    uint8_t r, g, b;
                    shade_pixel(norm, wpos, &r, &g, &b);
                    int fb_idx = y * stride + x * 3;
                    fb[fb_idx + 0] = r;
                    fb[fb_idx + 1] = g;
                    fb[fb_idx + 2] = b;
                }
            }

            e0_scalar += de0_dx;
            e1_scalar += de1_dx;
            e2_scalar += de2_dx;
        }
    }

#else
    /* ── Scalar path (non-x86_64) ────────────────────────────── */

    float de0_dx = sy1 - sy2;
    float de1_dx = sy2 - sy0;
    float de2_dx = sy0 - sy1;

    for (int y = min_y; y <= max_y; y++) {
        float py = (float)y + 0.5f;
        float px_start = (float)min_x + 0.5f;

        float e0 = edge_func(sx1, sy1, sx2, sy2, px_start, py);
        float e1 = edge_func(sx2, sy2, sx0, sy0, px_start, py);
        float e2 = edge_func(sx0, sy0, sx1, sy1, px_start, py);

        for (int x = min_x; x <= max_x; x++) {
            int inside;
            if (area > 0)
                inside = (e0 >= 0 && e1 >= 0 && e2 >= 0);
            else
                inside = (e0 <= 0 && e1 <= 0 && e2 <= 0);

            if (inside) {
                float w0 = e0 * inv_area;
                float w1 = e1 * inv_area;
                float w2 = 1.0f - w0 - w1;

                float persp = 1.0f / (w0*inv_w0 + w1*inv_w1 + w2*inv_w2);
                float depth = w0*sz0 + w1*sz1 + w2*sz2;

                int zidx = y * sw.width + x;
                if (depth < sw.zbuf[zidx]) {
                    sw.zbuf[zidx] = depth;

                    float pc0 = w0 * inv_w0 * persp;
                    float pc1 = w1 * inv_w1 * persp;
                    float pc2 = w2 * inv_w2 * persp;

                    float norm[3] = {
                        normals[0]*pc0 + normals[3]*pc1 + normals[6]*pc2,
                        normals[1]*pc0 + normals[4]*pc1 + normals[7]*pc2,
                        normals[2]*pc0 + normals[5]*pc1 + normals[8]*pc2
                    };
                    float wpos[3] = {
                        world[0]*pc0 + world[3]*pc1 + world[6]*pc2,
                        world[1]*pc0 + world[4]*pc1 + world[7]*pc2,
                        world[2]*pc0 + world[5]*pc1 + world[8]*pc2
                    };

                    uint8_t r, g, b;
                    shade_pixel(norm, wpos, &r, &g, &b);
                    int fb_idx = y * stride + x * 3;
                    fb[fb_idx + 0] = r;
                    fb[fb_idx + 1] = g;
                    fb[fb_idx + 2] = b;
                }
            }

            e0 += de0_dx;
            e1 += de1_dx;
            e2 += de2_dx;
        }
    }
#endif
}

/* ── Strip-clamped triangle rasterization ────────────────────── */

/*
 * rasterize_triangle_strip: same as rasterize_triangle but clamps the
 * bounding box to [y_clip_min, y_clip_max). Each strip thread calls this
 * with its own row range, guaranteeing non-overlapping framebuffer writes.
 */
static void rasterize_triangle_strip(const float screen[9],
                                     const float normals[9],
                                     const float world[9],
                                     const float w_clip[3],
                                     int y_clip_min, int y_clip_max)
{
    float sx0 = screen[0], sy0 = screen[1], sz0 = screen[2];
    float sx1 = screen[3], sy1 = screen[4], sz1 = screen[5];
    float sx2 = screen[6], sy2 = screen[7], sz2 = screen[8];

    /* Bounding box, clamped to viewport AND strip range */
    int min_x = maxi(0,             (int)floorf(fminf(sx0, fminf(sx1, sx2))));
    int max_x = mini(sw.width - 1,  (int)ceilf(fmaxf(sx0, fmaxf(sx1, sx2))));
    int min_y = maxi(y_clip_min,    (int)floorf(fminf(sy0, fminf(sy1, sy2))));
    int max_y = mini(y_clip_max - 1,(int)ceilf(fmaxf(sy0, fmaxf(sy1, sy2))));

    if (min_x > max_x || min_y > max_y) return;

    /* Total area (2x signed area of triangle) */
    float area = edge_func(sx0, sy0, sx1, sy1, sx2, sy2);
    if (fabsf(area) < 1e-6f) return;   /* degenerate triangle */
    float inv_area = 1.0f / area;

    /* Reciprocal W for perspective-correct interpolation */
    float inv_w0 = 1.0f / w_clip[0];
    float inv_w1 = 1.0f / w_clip[1];
    float inv_w2 = 1.0f / w_clip[2];

    uint8_t *fb = sw.tape + sw.fb_offset;
    int stride = sw.width * 3;

    /* Edge equation increments for stepping +1 in X */
    float de0_dx = sy1 - sy2;
    float de1_dx = sy2 - sy0;
    float de2_dx = sy0 - sy1;

    for (int y = min_y; y <= max_y; y++) {
        float py = (float)y + 0.5f;
        float px_start = (float)min_x + 0.5f;

        float e0 = edge_func(sx1, sy1, sx2, sy2, px_start, py);
        float e1 = edge_func(sx2, sy2, sx0, sy0, px_start, py);
        float e2 = edge_func(sx0, sy0, sx1, sy1, px_start, py);

        for (int x = min_x; x <= max_x; x++) {
            int inside;
            if (area > 0)
                inside = (e0 >= 0 && e1 >= 0 && e2 >= 0);
            else
                inside = (e0 <= 0 && e1 <= 0 && e2 <= 0);

            if (inside) {
                float w0 = e0 * inv_area;
                float w1 = e1 * inv_area;
                float w2 = 1.0f - w0 - w1;

                float persp = 1.0f / (w0*inv_w0 + w1*inv_w1 + w2*inv_w2);
                float depth = w0*sz0 + w1*sz1 + w2*sz2;

                int zidx = y * sw.width + x;
                if (depth < sw.zbuf[zidx]) {
                    sw.zbuf[zidx] = depth;

                    float pc0 = w0 * inv_w0 * persp;
                    float pc1 = w1 * inv_w1 * persp;
                    float pc2 = w2 * inv_w2 * persp;

                    float norm[3] = {
                        normals[0]*pc0 + normals[3]*pc1 + normals[6]*pc2,
                        normals[1]*pc0 + normals[4]*pc1 + normals[7]*pc2,
                        normals[2]*pc0 + normals[5]*pc1 + normals[8]*pc2
                    };
                    float wpos[3] = {
                        world[0]*pc0 + world[3]*pc1 + world[6]*pc2,
                        world[1]*pc0 + world[4]*pc1 + world[7]*pc2,
                        world[2]*pc0 + world[5]*pc1 + world[8]*pc2
                    };

                    uint8_t r, g, b;
                    shade_pixel(norm, wpos, &r, &g, &b);
                    int fb_idx = y * stride + x * 3;
                    fb[fb_idx + 0] = r;
                    fb[fb_idx + 1] = g;
                    fb[fb_idx + 2] = b;
                }
            }

            e0 += de0_dx;
            e1 += de1_dx;
            e2 += de2_dx;
        }
    }
}

/* ── Strip worker thread ─────────────────────────────────────── */

static void *sw_strip_worker(void *arg)
{
    int strip_id = (int)(intptr_t)arg;

    while (atomic_load(&sw.sw_running)) {
        /* Wait for frame signal */
        pthread_mutex_lock(&sw.sw_mutex);
        while (atomic_load(&sw.sw_strips_remaining) == 0 && atomic_load(&sw.sw_running))
            pthread_cond_wait(&sw.sw_frame_cv, &sw.sw_mutex);
        pthread_mutex_unlock(&sw.sw_mutex);

        if (!atomic_load(&sw.sw_running)) break;

        int y0 = sw.sw_strips[strip_id].y_start;
        int y1 = sw.sw_strips[strip_id].y_end;

        /* Rasterize all triangles, clipped to this strip */
        for (int t = 0; t < sw.sw_tri_count; t++) {
            const float *scr = &sw.sw_screen[t * 9];
            const float *nrm = &sw.sw_wnormals[t * 9];
            const float *wps = &sw.sw_wpos[t * 9];
            const float *wcl = &sw.sw_w_clip[t * 3];
            rasterize_triangle_strip(scr, nrm, wps, wcl, y0, y1);
        }

        /* Signal completion */
        if (atomic_fetch_sub(&sw.sw_strips_remaining, 1) == 1) {
            pthread_mutex_lock(&sw.sw_mutex);
            pthread_cond_signal(&sw.sw_done_cv);
            pthread_mutex_unlock(&sw.sw_mutex);
        }
    }
    return NULL;
}

/* ── Triangle draw: public API ───────────────────────────────── */

/*
 * bfpp_sw_draw_triangles: tape layout:
 *   ptr+0:  vertex_count (uint32)
 *   ptr+4:  index_count  (uint32)
 *   ptr+8:  vertex_offset (uint32, byte offset into tape for vertex data)
 *   ptr+12: index_offset  (uint32, byte offset into tape for index data)
 *
 * Vertex format: 6 Q16.16 values per vertex (pos.xyz, normal.xyz) = 24 bytes
 * Index format:  uint32 per index (not Q16.16)
 *
 * Algorithm per triangle:
 *   1. Transform position by MVP → clip space
 *   2. Perspective divide → NDC
 *   3. Viewport transform → screen coords
 *   4. Transform normal by normal_mat → world-space normal
 *   5. Transform position by model mat → world-space position (for lighting)
 *   6. Rasterize with Blinn-Phong shading
 */
void bfpp_sw_draw_triangles(uint8_t *tape, int ptr)
{
    uint32_t vertex_count  = tape_read_u32(tape, ptr);
    uint32_t index_count   = tape_read_u32(tape, ptr + 4);
    uint32_t vertex_offset = tape_read_u32(tape, ptr + 8);
    uint32_t index_offset  = tape_read_u32(tape, ptr + 12);

    if (index_count < 3) return;

    /* Convert all vertices from Q16.16 to float (6 floats per vertex) */
    float *verts = (float *)malloc((size_t)vertex_count * 6 * sizeof(float));
    if (!verts) return;

    for (uint32_t i = 0; i < vertex_count; i++) {
        uint32_t base = vertex_offset + i * 24;    /* 6 Q16.16 values × 4 bytes */
        for (int j = 0; j < 6; j++) {
            verts[i * 6 + j] = tape_read_q16(tape, (int)(base + (uint32_t)j * 4));
        }
    }

    /* Read index buffer */
    uint32_t *indices = (uint32_t *)malloc((size_t)index_count * sizeof(uint32_t));
    if (!indices) { free(verts); return; }

    for (uint32_t i = 0; i < index_count; i++) {
        indices[i] = tape_read_u32(tape, (int)(index_offset + i * 4));
    }

    float half_w = (float)sw.width  * 0.5f;
    float half_h = (float)sw.height * 0.5f;

    /* ── Phase 1: Transform all vertices → build per-triangle arrays ── */

    uint32_t max_tris = index_count / 3;
    float *t_screen   = (float *)malloc((size_t)max_tris * 9 * sizeof(float));
    float *t_wnormals = (float *)malloc((size_t)max_tris * 9 * sizeof(float));
    float *t_wpos     = (float *)malloc((size_t)max_tris * 9 * sizeof(float));
    float *t_w_clip   = (float *)malloc((size_t)max_tris * 3 * sizeof(float));
    if (!t_screen || !t_wnormals || !t_wpos || !t_w_clip) {
        free(t_screen); free(t_wnormals); free(t_wpos); free(t_w_clip);
        free(verts); free(indices);
        return;
    }

    int tri_out = 0;
    for (uint32_t t = 0; t + 2 < index_count; t += 3) {
        uint32_t i0 = indices[t + 0];
        uint32_t i1 = indices[t + 1];
        uint32_t i2 = indices[t + 2];

        if (i0 >= vertex_count || i1 >= vertex_count || i2 >= vertex_count)
            continue;

        float screen[9];   /* 3 vertices × { sx, sy, depth } */
        float wnormals[9]; /* 3 vertices × { nx, ny, nz } in world space */
        float wpos[9];     /* 3 vertices × { wx, wy, wz } in world space */
        float w_clip[3];   /* clip-space W per vertex */

        const uint32_t tri_idx[3] = { i0, i1, i2 };

        int clipped = 0;
        for (int v = 0; v < 3; v++) {
            const float *vert = &verts[tri_idx[v] * 6];
            float pos[4] = { vert[0], vert[1], vert[2], 1.0f };
            float norm[3] = { vert[3], vert[4], vert[5] };

            /* MVP transform → clip space */
            float clip[4];
            mat4_mul_vec4(sw.mvp, pos, clip);

            /* Near-plane clip: skip triangles behind camera */
            if (clip[3] <= 0.0f) { clipped = 1; break; }

            w_clip[v] = clip[3];

            /* Perspective divide → NDC [-1,1] */
            float inv_w = 1.0f / clip[3];
            float ndc_x = clip[0] * inv_w;
            float ndc_y = clip[1] * inv_w;
            float ndc_z = clip[2] * inv_w;

            /* Viewport transform: NDC → screen */
            screen[v*3 + 0] = (ndc_x + 1.0f) * half_w;
            screen[v*3 + 1] = (1.0f - ndc_y) * half_h;
            screen[v*3 + 2] = ndc_z;

            /* World-space normal via normal matrix */
            mat3_mul_vec3(sw.normal_mat, norm, &wnormals[v*3]);

            /* World-space position via model matrix */
            float wp[4];
            mat4_mul_vec4(sw.model, pos, wp);
            wpos[v*3 + 0] = wp[0];
            wpos[v*3 + 1] = wp[1];
            wpos[v*3 + 2] = wp[2];
        }

        if (clipped) continue;

        /* Store transformed triangle for strip workers */
        memcpy(&t_screen[tri_out * 9],   screen,   9 * sizeof(float));
        memcpy(&t_wnormals[tri_out * 9], wnormals, 9 * sizeof(float));
        memcpy(&t_wpos[tri_out * 9],     wpos,     9 * sizeof(float));
        memcpy(&t_w_clip[tri_out * 3],   w_clip,   3 * sizeof(float));
        tri_out++;
    }

    free(verts);
    free(indices);

    if (tri_out == 0) {
        free(t_screen); free(t_wnormals); free(t_wpos); free(t_w_clip);
        return;
    }

    /* ── Phase 2: Dispatch to strip workers ───────────────────── */

    sw.sw_screen   = t_screen;
    sw.sw_wnormals = t_wnormals;
    sw.sw_wpos     = t_wpos;
    sw.sw_w_clip   = t_w_clip;
    sw.sw_tri_count = tri_out;

    /* Signal all strip workers */
    pthread_mutex_lock(&sw.sw_mutex);
    atomic_store(&sw.sw_strips_remaining, sw.sw_thread_count);
    pthread_cond_broadcast(&sw.sw_frame_cv);
    pthread_mutex_unlock(&sw.sw_mutex);

    /* Wait for all strips to complete */
    pthread_mutex_lock(&sw.sw_mutex);
    while (atomic_load(&sw.sw_strips_remaining) > 0)
        pthread_cond_wait(&sw.sw_done_cv, &sw.sw_mutex);
    pthread_mutex_unlock(&sw.sw_mutex);

    /* Cleanup per-frame buffers */
    free(t_screen);
    free(t_wnormals);
    free(t_wpos);
    free(t_w_clip);
    sw.sw_screen   = NULL;
    sw.sw_wnormals = NULL;
    sw.sw_wpos     = NULL;
    sw.sw_w_clip   = NULL;
    sw.sw_tri_count = 0;
}

/* ── Present ─────────────────────────────────────────────────── */

/*
 * bfpp_sw_present: signal the FB pipeline to present the current frame.
 * Tape args are unused (present takes no parameters from BF++ tape).
 */
void bfpp_sw_present(uint8_t *tape, int ptr)
{
    (void)tape;
    (void)ptr;
    bfpp_fb_request_flush();
}
