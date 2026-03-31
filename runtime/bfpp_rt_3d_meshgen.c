/*
 * bfpp_rt_3d_meshgen.c — Mesh generators for the BF++ 3D subsystem.
 *
 * Each generator writes vertex data (pos xyz + normal xyz, Q16.16)
 * and uint32 index data to tape at an address read from tape[ptr].
 *
 * Output layout at tape[addr]:
 *   [0..3]  vertex_count  (uint32)
 *   [4..7]  index_count   (uint32)
 *   [8..]   vertex data   (vertex_count * 24 bytes)
 *   [after] index data    (index_count * 4 bytes)
 */

#include "bfpp_rt_3d.h"
#include <stdint.h>
#include <string.h>
#include <math.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* ── Tape helpers ───────────────────────────────────────────── */

static inline int32_t tape_q16(uint8_t *tape, int addr) {
    int32_t v;
    memcpy(&v, tape + addr, 4);
    return v;
}

static inline void tape_set_q16(uint8_t *tape, int addr, int32_t val) {
    memcpy(tape + addr, &val, 4);
}

static inline int32_t float_to_q16(float f) {
    return (int32_t)(f * 65536.0f);
}

static inline void tape_set_u32(uint8_t *tape, int addr, uint32_t val) {
    memcpy(tape + addr, &val, 4);
}

static inline uint32_t tape_get_u32(uint8_t *tape, int addr) {
    uint32_t v;
    memcpy(&v, tape + addr, 4);
    return v;
}

/* Write one vertex (pos + normal) at byte offset, return next offset. */
static inline int emit_vert(uint8_t *tape, int off,
                            float px, float py, float pz,
                            float nx, float ny, float nz) {
    tape_set_q16(tape, off +  0, float_to_q16(px));
    tape_set_q16(tape, off +  4, float_to_q16(py));
    tape_set_q16(tape, off +  8, float_to_q16(pz));
    tape_set_q16(tape, off + 12, float_to_q16(nx));
    tape_set_q16(tape, off + 16, float_to_q16(ny));
    tape_set_q16(tape, off + 20, float_to_q16(nz));
    return off + 24;
}

/* Write one uint32 index at byte offset, return next offset. */
static inline int emit_idx(uint8_t *tape, int off, uint32_t idx) {
    tape_set_u32(tape, off, idx);
    return off + 4;
}

/* ── Cube ───────────────────────────────────────────────────── */

void bfpp_mesh_cube(uint8_t *tape, int ptr) {
    int addr = tape_q16(tape, ptr);
    /* 6 faces × 2 tris × 3 verts = 36 verts, 36 indices (0..35) */
    uint32_t vc = 36, ic = 36;
    tape_set_u32(tape, addr, vc);
    tape_set_u32(tape, addr + 4, ic);

    static const float faces[6][4][3] = {
        /* +Z */ {{ -.5f, -.5f,  .5f}, { .5f, -.5f,  .5f}, { .5f,  .5f,  .5f}, { -.5f,  .5f,  .5f}},
        /* -Z */ {{ .5f, -.5f, -.5f}, { -.5f, -.5f, -.5f}, { -.5f,  .5f, -.5f}, { .5f,  .5f, -.5f}},
        /* +X */ {{ .5f, -.5f,  .5f}, { .5f, -.5f, -.5f}, { .5f,  .5f, -.5f}, { .5f,  .5f,  .5f}},
        /* -X */ {{ -.5f, -.5f, -.5f}, { -.5f, -.5f,  .5f}, { -.5f,  .5f,  .5f}, { -.5f,  .5f, -.5f}},
        /* +Y */ {{ -.5f,  .5f,  .5f}, { .5f,  .5f,  .5f}, { .5f,  .5f, -.5f}, { -.5f,  .5f, -.5f}},
        /* -Y */ {{ -.5f, -.5f, -.5f}, { .5f, -.5f, -.5f}, { .5f, -.5f,  .5f}, { -.5f, -.5f,  .5f}},
    };
    static const float normals[6][3] = {
        {0,0,1}, {0,0,-1}, {1,0,0}, {-1,0,0}, {0,1,0}, {0,-1,0}
    };

    int voff = addr + 8;
    for (int f = 0; f < 6; f++) {
        const float *n = normals[f];
        /* Triangle 1: 0,1,2 */
        voff = emit_vert(tape, voff, faces[f][0][0], faces[f][0][1], faces[f][0][2], n[0], n[1], n[2]);
        voff = emit_vert(tape, voff, faces[f][1][0], faces[f][1][1], faces[f][1][2], n[0], n[1], n[2]);
        voff = emit_vert(tape, voff, faces[f][2][0], faces[f][2][1], faces[f][2][2], n[0], n[1], n[2]);
        /* Triangle 2: 0,2,3 */
        voff = emit_vert(tape, voff, faces[f][0][0], faces[f][0][1], faces[f][0][2], n[0], n[1], n[2]);
        voff = emit_vert(tape, voff, faces[f][2][0], faces[f][2][1], faces[f][2][2], n[0], n[1], n[2]);
        voff = emit_vert(tape, voff, faces[f][3][0], faces[f][3][1], faces[f][3][2], n[0], n[1], n[2]);
    }

    int ioff = voff;
    for (uint32_t i = 0; i < 36; i++)
        ioff = emit_idx(tape, ioff, i);
}

/* ── Sphere (icosphere) ─────────────────────────────────────── */

/* Max subdivisions=4: 20*4^4 = 5120 faces, 15360 indices, 2562 verts */
#define ICO_MAX_VERTS  2562
#define ICO_MAX_FACES  5120

void bfpp_mesh_sphere(uint8_t *tape, int ptr) {
    int addr = tape_q16(tape, ptr);
    int subdiv = tape_q16(tape, ptr + 4);
    if (subdiv < 1) subdiv = 1;
    if (subdiv > 4) subdiv = 4;

    /* Working buffers on stack — max ~180KB, acceptable for runtime. */
    float verts[ICO_MAX_VERTS][3];
    uint32_t faces[ICO_MAX_FACES][3];
    int nv = 0, nf = 0;

    /* Icosahedron base vertices */
    float t = (1.0f + sqrtf(5.0f)) / 2.0f;
    float base[12][3] = {
        {-1, t, 0}, { 1, t, 0}, {-1,-t, 0}, { 1,-t, 0},
        { 0,-1, t}, { 0, 1, t}, { 0,-1,-t}, { 0, 1,-t},
        { t, 0,-1}, { t, 0, 1}, {-t, 0,-1}, {-t, 0, 1},
    };
    for (int i = 0; i < 12; i++) {
        float len = sqrtf(base[i][0]*base[i][0] + base[i][1]*base[i][1] + base[i][2]*base[i][2]);
        verts[nv][0] = base[i][0] / len;
        verts[nv][1] = base[i][1] / len;
        verts[nv][2] = base[i][2] / len;
        nv++;
    }

    /* Icosahedron 20 faces */
    static const uint32_t ico_faces[20][3] = {
        {0,11,5}, {0,5,1}, {0,1,7}, {0,7,10}, {0,10,11},
        {1,5,9}, {5,11,4}, {11,10,2}, {10,7,6}, {7,1,8},
        {3,9,4}, {3,4,2}, {3,2,6}, {3,6,8}, {3,8,9},
        {4,9,5}, {2,4,11}, {6,2,10}, {8,6,7}, {9,8,1},
    };
    for (int i = 0; i < 20; i++) {
        faces[nf][0] = ico_faces[i][0];
        faces[nf][1] = ico_faces[i][1];
        faces[nf][2] = ico_faces[i][2];
        nf++;
    }

    /* Subdivide */
    for (int s = 0; s < subdiv; s++) {
        int new_nf = 0;
        uint32_t new_faces[ICO_MAX_FACES][3];

        /* Simple midpoint cache: linear scan (good enough for <=5120 faces) */
        /* We use a flat lookup: for each edge, find or create midpoint */
        /* Edge key: min(a,b)*ICO_MAX_VERTS + max(a,b) */
        /* Cache up to 3*nf edges */
        int edge_count = 0;
        uint32_t edge_a[ICO_MAX_FACES * 3], edge_b[ICO_MAX_FACES * 3], edge_mid[ICO_MAX_FACES * 3];

        #define MIDPOINT(a, b) ({ \
            uint32_t _lo = (a) < (b) ? (a) : (b); \
            uint32_t _hi = (a) < (b) ? (b) : (a); \
            int _found = -1; \
            for (int _e = 0; _e < edge_count; _e++) { \
                if (edge_a[_e] == _lo && edge_b[_e] == _hi) { _found = _e; break; } \
            } \
            uint32_t _mid; \
            if (_found >= 0) { \
                _mid = edge_mid[_found]; \
            } else { \
                float mx = (verts[_lo][0] + verts[_hi][0]) * 0.5f; \
                float my = (verts[_lo][1] + verts[_hi][1]) * 0.5f; \
                float mz = (verts[_lo][2] + verts[_hi][2]) * 0.5f; \
                float ml = sqrtf(mx*mx + my*my + mz*mz); \
                verts[nv][0] = mx / ml; \
                verts[nv][1] = my / ml; \
                verts[nv][2] = mz / ml; \
                _mid = (uint32_t)nv; \
                edge_a[edge_count] = _lo; \
                edge_b[edge_count] = _hi; \
                edge_mid[edge_count] = _mid; \
                edge_count++; \
                nv++; \
            } \
            _mid; \
        })

        for (int f = 0; f < nf; f++) {
            uint32_t v0 = faces[f][0], v1 = faces[f][1], v2 = faces[f][2];
            uint32_t m01 = MIDPOINT(v0, v1);
            uint32_t m12 = MIDPOINT(v1, v2);
            uint32_t m02 = MIDPOINT(v0, v2);

            new_faces[new_nf][0] = v0;  new_faces[new_nf][1] = m01; new_faces[new_nf][2] = m02; new_nf++;
            new_faces[new_nf][0] = v1;  new_faces[new_nf][1] = m12; new_faces[new_nf][2] = m01; new_nf++;
            new_faces[new_nf][0] = v2;  new_faces[new_nf][1] = m02; new_faces[new_nf][2] = m12; new_nf++;
            new_faces[new_nf][0] = m01; new_faces[new_nf][1] = m12; new_faces[new_nf][2] = m02; new_nf++;
        }

        #undef MIDPOINT

        memcpy(faces, new_faces, new_nf * sizeof(uint32_t[3]));
        nf = new_nf;
    }

    /* Write output */
    uint32_t vc = (uint32_t)nv;
    uint32_t ic = (uint32_t)(nf * 3);
    tape_set_u32(tape, addr, vc);
    tape_set_u32(tape, addr + 4, ic);

    int off = addr + 8;
    for (int i = 0; i < nv; i++) {
        /* Normal = position (unit sphere) */
        off = emit_vert(tape, off, verts[i][0], verts[i][1], verts[i][2],
                                    verts[i][0], verts[i][1], verts[i][2]);
    }
    for (int f = 0; f < nf; f++) {
        off = emit_idx(tape, off, faces[f][0]);
        off = emit_idx(tape, off, faces[f][1]);
        off = emit_idx(tape, off, faces[f][2]);
    }
}

/* ── Torus ──────────────────────────────────────────────────── */

void bfpp_mesh_torus(uint8_t *tape, int ptr) {
    int addr = tape_q16(tape, ptr);
    int major_seg = tape_q16(tape, ptr + 4);
    int minor_seg = tape_q16(tape, ptr + 8);
    if (major_seg < 3) major_seg = 16;
    if (minor_seg < 3) minor_seg = 8;

    float R = 1.0f, r = 0.3f;
    uint32_t vc = (uint32_t)(major_seg * minor_seg);
    uint32_t ic = (uint32_t)(major_seg * minor_seg * 6);
    tape_set_u32(tape, addr, vc);
    tape_set_u32(tape, addr + 4, ic);

    int voff = addr + 8;
    for (int i = 0; i < major_seg; i++) {
        float theta = 2.0f * (float)M_PI * i / major_seg;
        float ct = cosf(theta), st = sinf(theta);
        for (int j = 0; j < minor_seg; j++) {
            float phi = 2.0f * (float)M_PI * j / minor_seg;
            float cp = cosf(phi), sp = sinf(phi);

            float px = (R + r * cp) * ct;
            float py = r * sp;
            float pz = (R + r * cp) * st;

            /* Normal = direction from ring center to vertex */
            float nx = cp * ct;
            float ny = sp;
            float nz = cp * st;

            voff = emit_vert(tape, voff, px, py, pz, nx, ny, nz);
        }
    }

    int ioff = voff;
    for (int i = 0; i < major_seg; i++) {
        int next_i = (i + 1) % major_seg;
        for (int j = 0; j < minor_seg; j++) {
            int next_j = (j + 1) % minor_seg;
            uint32_t a = i * minor_seg + j;
            uint32_t b = next_i * minor_seg + j;
            uint32_t c = next_i * minor_seg + next_j;
            uint32_t d = i * minor_seg + next_j;
            /* Two triangles per quad */
            ioff = emit_idx(tape, ioff, a);
            ioff = emit_idx(tape, ioff, b);
            ioff = emit_idx(tape, ioff, c);
            ioff = emit_idx(tape, ioff, a);
            ioff = emit_idx(tape, ioff, c);
            ioff = emit_idx(tape, ioff, d);
        }
    }
}

/* ── Plane ──────────────────────────────────────────────────── */

void bfpp_mesh_plane(uint8_t *tape, int ptr) {
    int addr = tape_q16(tape, ptr);
    int subdiv = tape_q16(tape, ptr + 4);
    if (subdiv < 1) subdiv = 1;

    int grid = subdiv + 1; /* verts per side */
    uint32_t vc = (uint32_t)(grid * grid);
    uint32_t ic = (uint32_t)(subdiv * subdiv * 6);
    tape_set_u32(tape, addr, vc);
    tape_set_u32(tape, addr + 4, ic);

    int voff = addr + 8;
    for (int z = 0; z < grid; z++) {
        for (int x = 0; x < grid; x++) {
            float px = -1.0f + 2.0f * x / subdiv;
            float pz = -1.0f + 2.0f * z / subdiv;
            voff = emit_vert(tape, voff, px, 0.0f, pz, 0.0f, 1.0f, 0.0f);
        }
    }

    int ioff = voff;
    for (int z = 0; z < subdiv; z++) {
        for (int x = 0; x < subdiv; x++) {
            uint32_t tl = z * grid + x;
            uint32_t tr = tl + 1;
            uint32_t bl = (z + 1) * grid + x;
            uint32_t br = bl + 1;
            ioff = emit_idx(tape, ioff, tl);
            ioff = emit_idx(tape, ioff, bl);
            ioff = emit_idx(tape, ioff, tr);
            ioff = emit_idx(tape, ioff, tr);
            ioff = emit_idx(tape, ioff, bl);
            ioff = emit_idx(tape, ioff, br);
        }
    }
}

/* ── Cylinder ───────────────────────────────────────────────── */

void bfpp_mesh_cylinder(uint8_t *tape, int ptr) {
    int addr = tape_q16(tape, ptr);
    int seg = tape_q16(tape, ptr + 4);
    if (seg < 3) seg = 16;

    /*
     * Side: seg quads = seg*2 tris = seg*6 indices, seg*2 verts (top+bottom rings)
     * Top cap: seg tris = seg*3 indices, seg+1 verts (ring + center)
     * Bottom cap: same
     * Total verts: seg*2 + (seg+1)*2 = seg*4 + 2
     * Total indices: seg*6 + seg*3 + seg*3 = seg*12
     */
    uint32_t vc = (uint32_t)(seg * 4 + 2);
    uint32_t ic = (uint32_t)(seg * 12);
    tape_set_u32(tape, addr, vc);
    tape_set_u32(tape, addr + 4, ic);

    int voff = addr + 8;

    /* Side verts: bottom ring [0..seg-1], top ring [seg..2*seg-1] */
    for (int i = 0; i < seg; i++) {
        float angle = 2.0f * (float)M_PI * i / seg;
        float cx = cosf(angle), cz = sinf(angle);
        voff = emit_vert(tape, voff, cx, -1.0f, cz, cx, 0.0f, cz);
    }
    for (int i = 0; i < seg; i++) {
        float angle = 2.0f * (float)M_PI * i / seg;
        float cx = cosf(angle), cz = sinf(angle);
        voff = emit_vert(tape, voff, cx, 1.0f, cz, cx, 0.0f, cz);
    }

    /* Top cap verts: center [2*seg], ring [2*seg+1 .. 3*seg] */
    int top_center = seg * 2;
    voff = emit_vert(tape, voff, 0.0f, 1.0f, 0.0f, 0.0f, 1.0f, 0.0f);
    for (int i = 0; i < seg; i++) {
        float angle = 2.0f * (float)M_PI * i / seg;
        voff = emit_vert(tape, voff, cosf(angle), 1.0f, sinf(angle), 0.0f, 1.0f, 0.0f);
    }

    /* Bottom cap verts: center [3*seg+1], ring [3*seg+2 .. 4*seg+1] */
    int bot_center = seg * 3 + 1;
    voff = emit_vert(tape, voff, 0.0f, -1.0f, 0.0f, 0.0f, -1.0f, 0.0f);
    for (int i = 0; i < seg; i++) {
        float angle = 2.0f * (float)M_PI * i / seg;
        voff = emit_vert(tape, voff, cosf(angle), -1.0f, sinf(angle), 0.0f, -1.0f, 0.0f);
    }

    /* Side indices */
    int ioff = voff;
    for (int i = 0; i < seg; i++) {
        uint32_t b0 = i, b1 = (i + 1) % seg;
        uint32_t t0 = seg + i, t1 = seg + (i + 1) % seg;
        ioff = emit_idx(tape, ioff, b0);
        ioff = emit_idx(tape, ioff, b1);
        ioff = emit_idx(tape, ioff, t1);
        ioff = emit_idx(tape, ioff, b0);
        ioff = emit_idx(tape, ioff, t1);
        ioff = emit_idx(tape, ioff, t0);
    }

    /* Top cap indices (CCW from above) */
    for (int i = 0; i < seg; i++) {
        uint32_t r0 = top_center + 1 + i;
        uint32_t r1 = top_center + 1 + (i + 1) % seg;
        ioff = emit_idx(tape, ioff, (uint32_t)top_center);
        ioff = emit_idx(tape, ioff, r0);
        ioff = emit_idx(tape, ioff, r1);
    }

    /* Bottom cap indices (CCW from below = CW from above) */
    for (int i = 0; i < seg; i++) {
        uint32_t r0 = bot_center + 1 + i;
        uint32_t r1 = bot_center + 1 + (i + 1) % seg;
        ioff = emit_idx(tape, ioff, (uint32_t)bot_center);
        ioff = emit_idx(tape, ioff, r1);
        ioff = emit_idx(tape, ioff, r0);
    }
}
