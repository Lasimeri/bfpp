#ifndef BFPP_RT_3D_MULTIGPU_H
#define BFPP_RT_3D_MULTIGPU_H

/*
 * bfpp_rt_3d_multigpu.h — Multi-GPU rendering for the BF++ 3D subsystem.
 *
 * Architecture:
 *   Uses EGL device enumeration to discover GPUs, creates per-GPU offscreen
 *   GL contexts, and dispatches rendering via SFR (split-frame rendering)
 *   or AFR (alternate-frame rendering).
 *
 *   SFR: each GPU renders a horizontal strip; strips composited into tape.
 *   AFR: GPUs render alternating frames; presentation queue delivers in order.
 *   AUTO: starts SFR, switches to AFR (or vice versa) on sustained drops.
 *
 *   Hardware targets:
 *     Desktop — 2 GPUs (3090 Ti + 3090), threads pinned to cores 8-9
 *     Rack    — 8 GPUs (3080 20GB), NUMA-aware staging, 2 GPUs per node
 *
 *   Command buffer: main thread records GL commands, GPU threads replay them.
 *   Readback via PBO double-buffer → NUMA-local staging → memcpy to tape.
 */

#include <stdint.h>

#define BFPP_MAX_GPUS 16

typedef enum {
    BFPP_MULTI_NONE = 0,
    BFPP_MULTI_SFR  = 1,
    BFPP_MULTI_AFR  = 2,
    BFPP_MULTI_AUTO = 3
} bfpp_multi_mode_t;

/* ── Lifecycle ───────────────────────────────────────────────── */

/* Enumerate available GPUs via EGL. Returns device count. */
int bfpp_mgpu_enumerate(void);

/* Initialize multi-GPU: create EGL contexts, FBOs, PBOs per GPU.
 * mode: SFR, AFR, or AUTO.
 * Returns 0 on success, -1 on failure (falls back to single-GPU). */
int bfpp_mgpu_init(bfpp_multi_mode_t mode, int width, int height,
                   uint8_t *tape, int fb_offset);

/* Tear down all GPU contexts and staging buffers. */
void bfpp_mgpu_cleanup(void);

/* Query active GPU count (post-init). */
int bfpp_mgpu_gpu_count(void);

/* ── Frame dispatch ──────────────────────────────────────────── */

/* Present frame via multi-GPU pipeline.
 * Dispatches to SFR or AFR based on current mode. */
void bfpp_mgpu_present(uint8_t *tape, int fb_offset);

/* ── Command buffer ──────────────────────────────────────────── */

/* Record a GL command for multi-GPU replay (SFR/AFR). */
void bfpp_mgpu_cmd_clear(float r, float g, float b);
void bfpp_mgpu_cmd_bind_vao(uint32_t vao_id);
void bfpp_mgpu_cmd_use_program(uint32_t prog_id);
void bfpp_mgpu_cmd_uniform_1f(int32_t loc, float val);
void bfpp_mgpu_cmd_uniform_3f(int32_t loc, float x, float y, float z);
void bfpp_mgpu_cmd_uniform_4f(int32_t loc, float x, float y, float z, float w);
void bfpp_mgpu_cmd_uniform_mat4(int32_t loc, const float mat[16]);
void bfpp_mgpu_cmd_draw_arrays(uint32_t mode, int32_t first, int32_t count);
void bfpp_mgpu_cmd_draw_elements(uint32_t mode, int32_t count, uint32_t type);
void bfpp_mgpu_cmd_viewport(int32_t x, int32_t y, int32_t w, int32_t h);
void bfpp_mgpu_cmd_depth_test(int enable);
void bfpp_mgpu_cmd_reset(void);  /* Clear command buffer for new frame */

/* ── Intrinsic wrappers (called from generated C) ────────────── */

void bfpp_gl_multi_gpu(uint8_t *tape, int ptr);
void bfpp_gl_gpu_count(uint8_t *tape, int ptr);

#endif /* BFPP_RT_3D_MULTIGPU_H */
