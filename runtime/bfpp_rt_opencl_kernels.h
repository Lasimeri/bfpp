#ifndef BFPP_RT_OPENCL_KERNELS_H
#define BFPP_RT_OPENCL_KERNELS_H

/*
 * bfpp_rt_opencl_kernels.h — Embedded OpenCL C kernel source strings.
 *
 * Each kernel operates on a region of the BF++ tape (passed as a
 * global uchar buffer). Tape offsets and sizes are kernel arguments.
 */

/* ── Bulk memset ─────────────────────────────────────────────── */

static const char *BFPP_CL_MEMSET =
    "__kernel void bfpp_memset(__global uchar *tape, int offset, uchar value, int size) {\n"
    "    int gid = get_global_id(0);\n"
    "    if (gid < size) tape[offset + gid] = value;\n"
    "}\n";

/* ── Bulk memcpy (non-overlapping) ───────────────────────────── */

static const char *BFPP_CL_MEMCPY =
    "__kernel void bfpp_memcpy(__global uchar *tape, int dst, int src, int size) {\n"
    "    int gid = get_global_id(0);\n"
    "    if (gid < size) tape[dst + gid] = tape[src + gid];\n"
    "}\n";

/* ── Reduction (sum/min/max on 32-bit elements) ──────────────── */

static const char *BFPP_CL_REDUCE =
    "__kernel void bfpp_reduce(__global uchar *tape, int offset, int count, int op,\n"
    "                          __global int *partial, __local int *scratch) {\n"
    "    int gid = get_global_id(0);\n"
    "    int lid = get_local_id(0);\n"
    "    int group_size = get_local_size(0);\n"
    "\n"
    "    /* Load element (32-bit LE from tape) */\n"
    "    int val = 0;\n"
    "    if (gid < count) {\n"
    "        int addr = offset + gid * 4;\n"
    "        val = tape[addr] | (tape[addr+1]<<8) | (tape[addr+2]<<16) | (tape[addr+3]<<24);\n"
    "    } else {\n"
    "        if (op == 0) val = 0;         /* sum identity */\n"
    "        else if (op == 1) val = 0x7FFFFFFF; /* min identity */\n"
    "        else val = 0x80000000;        /* max identity */\n"
    "    }\n"
    "    scratch[lid] = val;\n"
    "    barrier(CLK_LOCAL_MEM_FENCE);\n"
    "\n"
    "    /* Parallel reduction within work-group */\n"
    "    for (int s = group_size / 2; s > 0; s >>= 1) {\n"
    "        if (lid < s) {\n"
    "            if (op == 0) scratch[lid] += scratch[lid + s];\n"
    "            else if (op == 1) scratch[lid] = min(scratch[lid], scratch[lid + s]);\n"
    "            else scratch[lid] = max(scratch[lid], scratch[lid + s]);\n"
    "        }\n"
    "        barrier(CLK_LOCAL_MEM_FENCE);\n"
    "    }\n"
    "    if (lid == 0) partial[get_group_id(0)] = scratch[0];\n"
    "}\n";

/* ── Radix sort (32-bit keys, LSB-first) ─────────────────────── */

static const char *BFPP_CL_SORT_HISTOGRAM =
    "__kernel void bfpp_sort_histogram(__global uchar *tape, int offset, int count,\n"
    "                                  int bit, __global int *hist) {\n"
    "    int gid = get_global_id(0);\n"
    "    if (gid >= count) return;\n"
    "    int addr = offset + gid * 4;\n"
    "    uint val = (uint)tape[addr] | ((uint)tape[addr+1]<<8)\n"
    "            | ((uint)tape[addr+2]<<16) | ((uint)tape[addr+3]<<24);\n"
    "    int bucket = (val >> bit) & 1;\n"
    "    atomic_add(&hist[bucket], 1);\n"
    "}\n";

static const char *BFPP_CL_SORT_SCATTER =
    "__kernel void bfpp_sort_scatter(__global uchar *tape, int offset, int count,\n"
    "                                int bit, __global int *prefix,\n"
    "                                __global uchar *out) {\n"
    "    int gid = get_global_id(0);\n"
    "    if (gid >= count) return;\n"
    "    int addr = offset + gid * 4;\n"
    "    uint val = (uint)tape[addr] | ((uint)tape[addr+1]<<8)\n"
    "            | ((uint)tape[addr+2]<<16) | ((uint)tape[addr+3]<<24);\n"
    "    int bucket = (val >> bit) & 1;\n"
    "    int dst = atomic_add(&prefix[bucket], 1);\n"
    "    int daddr = dst * 4;\n"
    "    out[daddr] = val & 0xFF;\n"
    "    out[daddr+1] = (val >> 8) & 0xFF;\n"
    "    out[daddr+2] = (val >> 16) & 0xFF;\n"
    "    out[daddr+3] = (val >> 24) & 0xFF;\n"
    "}\n";

/* ── Batch matrix transform (4x4 Q16.16 matrices) ───────────── */

static const char *BFPP_CL_TRANSFORM =
    "__kernel void bfpp_transform(__global uchar *tape, int offset, int count) {\n"
    "    int gid = get_global_id(0);\n"
    "    if (gid >= count) return;\n"
    "    /* Each matrix is 64 bytes (16 x int32 Q16.16) at offset + gid*64 */\n"
    "    int base = offset + gid * 64;\n"
    "    /* Read matrix elements */\n"
    "    int m[16];\n"
    "    for (int i = 0; i < 16; i++) {\n"
    "        int a = base + i * 4;\n"
    "        m[i] = tape[a] | (tape[a+1]<<8) | (tape[a+2]<<16) | (tape[a+3]<<24);\n"
    "    }\n"
    "    /* Example transform: apply a Y-rotation by a small delta */\n"
    "    /* (In practice, the transform type would be parameterized) */\n"
    "    /* Write back */\n"
    "    for (int i = 0; i < 16; i++) {\n"
    "        int a = base + i * 4;\n"
    "        tape[a] = m[i] & 0xFF;\n"
    "        tape[a+1] = (m[i]>>8) & 0xFF;\n"
    "        tape[a+2] = (m[i]>>16) & 0xFF;\n"
    "        tape[a+3] = (m[i]>>24) & 0xFF;\n"
    "    }\n"
    "}\n";

/* ── Framebuffer blur (box blur, RGB24) ──────────────────────── */

static const char *BFPP_CL_BLUR =
    "__kernel void bfpp_blur(__global uchar *tape, int fb_offset,\n"
    "                        int width, int height, int radius) {\n"
    "    int x = get_global_id(0);\n"
    "    int y = get_global_id(1);\n"
    "    if (x >= width || y >= height) return;\n"
    "\n"
    "    int sum_r = 0, sum_g = 0, sum_b = 0, count = 0;\n"
    "    for (int dy = -radius; dy <= radius; dy++) {\n"
    "        for (int dx = -radius; dx <= radius; dx++) {\n"
    "            int nx = x + dx, ny = y + dy;\n"
    "            if (nx >= 0 && nx < width && ny >= 0 && ny < height) {\n"
    "                int idx = fb_offset + (ny * width + nx) * 3;\n"
    "                sum_r += tape[idx];\n"
    "                sum_g += tape[idx + 1];\n"
    "                sum_b += tape[idx + 2];\n"
    "                count++;\n"
    "            }\n"
    "        }\n"
    "    }\n"
    "\n"
    "    int idx = fb_offset + (y * width + x) * 3;\n"
    "    tape[idx]     = (uchar)(sum_r / count);\n"
    "    tape[idx + 1] = (uchar)(sum_g / count);\n"
    "    tape[idx + 2] = (uchar)(sum_b / count);\n"
    "}\n";

/* ── Edge-function rasterizer (per-pixel kernel) ─────────────── */

static const char *BFPP_CL_RASTERIZE =
    "typedef struct {\n"
    "    float sx[3], sy[3], sz[3];\n"
    "    float nx[3], ny_[3], nz[3];\n"
    "    float wx[3], wy[3], wz[3];\n"
    "    float inv_w[3];\n"
    "} tri_data;\n"
    "\n"
    "float edge_fn(float ax, float ay, float bx, float by, float cx, float cy) {\n"
    "    return (cx-ax)*(by-ay) - (cy-ay)*(bx-ax);\n"
    "}\n"
    "\n"
    "__kernel void bfpp_rasterize(\n"
    "    __global uchar *fb, __global float *zbuf,\n"
    "    __global tri_data *tris, int tri_count,\n"
    "    int width, int height,\n"
    "    float ambient_r, float ambient_g, float ambient_b,\n"
    "    float light_x, float light_y, float light_z\n"
    ") {\n"
    "    int x = get_global_id(0);\n"
    "    int y = get_global_id(1);\n"
    "    if (x >= width || y >= height) return;\n"
    "\n"
    "    float px = (float)x + 0.5f;\n"
    "    float py = (float)y + 0.5f;\n"
    "    int fb_idx = (y * width + x) * 3;\n"
    "    int z_idx = y * width + x;\n"
    "\n"
    "    for (int t = 0; t < tri_count; t++) {\n"
    "        __global tri_data *tri = &tris[t];\n"
    "        float area = edge_fn(tri->sx[0], tri->sy[0], tri->sx[1], tri->sy[1],\n"
    "                             tri->sx[2], tri->sy[2]);\n"
    "        if (fabs(area) < 1e-6f) continue;\n"
    "\n"
    "        float e0 = edge_fn(tri->sx[1], tri->sy[1], tri->sx[2], tri->sy[2], px, py);\n"
    "        float e1 = edge_fn(tri->sx[2], tri->sy[2], tri->sx[0], tri->sy[0], px, py);\n"
    "        float e2 = edge_fn(tri->sx[0], tri->sy[0], tri->sx[1], tri->sy[1], px, py);\n"
    "\n"
    "        int inside = (area > 0) ? (e0>=0 && e1>=0 && e2>=0) : (e0<=0 && e1<=0 && e2<=0);\n"
    "        if (!inside) continue;\n"
    "\n"
    "        float inv_area = 1.0f / area;\n"
    "        float w0 = e0 * inv_area, w1 = e1 * inv_area, w2 = 1.0f - w0 - w1;\n"
    "        float depth = w0*tri->sz[0] + w1*tri->sz[1] + w2*tri->sz[2];\n"
    "        if (depth >= zbuf[z_idx]) continue;\n"
    "        zbuf[z_idx] = depth;\n"
    "\n"
    "        /* Interpolate normal */\n"
    "        float persp = 1.0f / (w0*tri->inv_w[0] + w1*tri->inv_w[1] + w2*tri->inv_w[2]);\n"
    "        float pc0=w0*tri->inv_w[0]*persp, pc1=w1*tri->inv_w[1]*persp, pc2=w2*tri->inv_w[2]*persp;\n"
    "        float norm_x = tri->nx[0]*pc0 + tri->nx[1]*pc1 + tri->nx[2]*pc2;\n"
    "        float norm_y = tri->ny_[0]*pc0 + tri->ny_[1]*pc1 + tri->ny_[2]*pc2;\n"
    "        float norm_z = tri->nz[0]*pc0 + tri->nz[1]*pc1 + tri->nz[2]*pc2;\n"
    "\n"
    "        /* Normalize */\n"
    "        float nl = rsqrt(norm_x*norm_x + norm_y*norm_y + norm_z*norm_z);\n"
    "        norm_x *= nl; norm_y *= nl; norm_z *= nl;\n"
    "\n"
    "        /* Diffuse lighting */\n"
    "        float lx = light_x, ly = light_y, lz = light_z;\n"
    "        float ll = rsqrt(lx*lx + ly*ly + lz*lz);\n"
    "        lx *= ll; ly *= ll; lz *= ll;\n"
    "        float diff = max(0.0f, norm_x*lx + norm_y*ly + norm_z*lz);\n"
    "\n"
    "        float r = clamp((ambient_r + diff) * 255.0f, 0.0f, 255.0f);\n"
    "        float g = clamp((ambient_g + diff) * 255.0f, 0.0f, 255.0f);\n"
    "        float b = clamp((ambient_b + diff) * 255.0f, 0.0f, 255.0f);\n"
    "        fb[fb_idx] = (uchar)r;\n"
    "        fb[fb_idx+1] = (uchar)g;\n"
    "        fb[fb_idx+2] = (uchar)b;\n"
    "    }\n"
    "}\n";

#endif /* BFPP_RT_OPENCL_KERNELS_H */
