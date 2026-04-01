#ifndef BFPP_RT_OPENCL_H
#define BFPP_RT_OPENCL_H

/*
 * bfpp_rt_opencl.h — GPU compute offloading for BF++ via OpenCL.
 *
 * Architecture:
 *   - Optional runtime: loaded via dlopen("libOpenCL.so") at init.
 *   - Programs compile without OpenCL installed; GPU intrinsics become no-ops.
 *   - Supports up to 8 GPUs with per-device command queues.
 *   - Async dispatch: CPU continues BF++ execution while GPU works.
 *   - Event-based completion: poll or wait for results.
 *   - The BF++ tape is the shared address space (host-side).
 *     GPU operations read from / write to tape regions via DMA.
 *
 * Bandwidth threshold: operations must exceed >64KB data or >10K ops
 * to justify GPU dispatch overhead (~50-100μs per kernel launch).
 */

#include <stdint.h>

/* ── Lifecycle ───────────────────────────────────────────────── */

/* Initialize OpenCL: enumerate platforms/devices, create contexts/queues.
 * Returns number of compute devices found (0 if OpenCL unavailable). */
int bfpp_opencl_init(void);

/* Cleanup: release all contexts, queues, kernels, buffers. */
void bfpp_opencl_cleanup(void);

/* Query number of available compute devices. */
int bfpp_opencl_device_count(void);

/* Returns 1 if OpenCL is available and initialized. */
int bfpp_opencl_available(void);

/* ── Async Operations ────────────────────────────────────────── */

/* All async operations return a handle (>=0) on success, -1 on failure.
 * Use bfpp_opencl_poll/wait to check/collect results. */

/* GPU-accelerated bulk memset on tape region. */
int bfpp_opencl_memset(uint8_t *tape, int offset, uint8_t value, int size);

/* GPU-accelerated bulk memcpy (non-overlapping). */
int bfpp_opencl_memcpy(uint8_t *tape, int dst, int src, int size);

/* GPU radix sort on tape region (32-bit elements). */
int bfpp_opencl_sort(uint8_t *tape, int offset, int count, int elem_size);

/* GPU reduction (sum/min/max) on tape region (32-bit elements).
 * op: 0=sum, 1=min, 2=max. Result written to tape[offset]. */
int bfpp_opencl_reduce(uint8_t *tape, int offset, int count, int op);

/* GPU batch matrix transform: transform `count` 4x4 matrices in-place. */
int bfpp_opencl_transform(uint8_t *tape, int matrices_offset, int count);

/* GPU software rasterization dispatch. */
int bfpp_opencl_rasterize(uint8_t *tape, int vert_offset, int vert_count,
                          int idx_offset, int idx_count,
                          int fb_offset, int width, int height);

/* GPU framebuffer blur (box blur with given radius). */
int bfpp_opencl_blur(uint8_t *tape, int fb_offset, int width, int height, int radius);

/* ── Completion ──────────────────────────────────────────────── */

/* Poll: returns 1 if operation is complete, 0 if still running. */
int bfpp_opencl_poll(int handle);

/* Wait: blocks until operation completes. */
void bfpp_opencl_wait(int handle);

/* ── Intrinsic Wrappers (called from generated C) ────────────── */

void bfpp_gpu_init(uint8_t *tape, int ptr);
void bfpp_gpu_count(uint8_t *tape, int ptr);
void bfpp_gpu_memset(uint8_t *tape, int ptr);
void bfpp_gpu_memcpy(uint8_t *tape, int ptr);
void bfpp_gpu_sort(uint8_t *tape, int ptr);
void bfpp_gpu_reduce(uint8_t *tape, int ptr);
void bfpp_gpu_transform(uint8_t *tape, int ptr);
void bfpp_gpu_rasterize(uint8_t *tape, int ptr);
void bfpp_gpu_blur(uint8_t *tape, int ptr);
void bfpp_gpu_poll(uint8_t *tape, int ptr);
void bfpp_gpu_wait(uint8_t *tape, int ptr);
void bfpp_gpu_dispatch(uint8_t *tape, int ptr);

#endif /* BFPP_RT_OPENCL_H */
