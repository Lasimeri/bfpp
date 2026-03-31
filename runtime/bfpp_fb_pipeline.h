#ifndef BFPP_FB_PIPELINE_H
#define BFPP_FB_PIPELINE_H

/*
 * bfpp_fb_pipeline.h — Public API for the BF++ 4K@60fps tiled render pipeline.
 *
 * Architecture:
 *   - Presenter thread owns SDL; spawned by init(), never touched by main thread.
 *   - N render threads (default 8) process horizontal strips in parallel.
 *   - Triple buffering: write buffer (tape[fb_offset]), staging buffer, present buffer.
 *   - The main thread (BF++ program logic) never calls SDL directly.
 *
 * Lifecycle: init → (write_pixel_nt | request_flush | sync)* → cleanup
 *
 * Buffer allocation uses huge pages (MAP_HUGETLB) with transparent fallback.
 */

#include <stdint.h>
#include <stdatomic.h>

/* ── Configuration ───────────────────────────────────────────── */

#ifndef BFPP_FB_RENDER_THREADS
#define BFPP_FB_RENDER_THREADS 8
#endif

/* ── Quit flag ───────────────────────────────────────────────── */

// Set to 1 by the presenter thread when SDL_QUIT is received.
// The BF++ program should poll this to decide when to exit.
extern atomic_int bfpp_fb_quit;

/* ── Lifecycle ───────────────────────────────────────────────── */

// Spawn the presenter thread (owns SDL window/renderer) and render threads.
// `tape` is the BF++ tape; `fb_offset` is the byte offset where the
// framebuffer region begins. Allocates staging + present buffers internally.
void bfpp_fb_pipeline_init(int width, int height, uint8_t *tape, int fb_offset);

// Join all threads, free triple-buffer allocations, destroy SDL context.
void bfpp_fb_pipeline_cleanup(void);

/* ── Frame operations ────────────────────────────────────────── */

// Non-blocking flush request. Sets an atomic flag; the presenter thread
// picks it up on its next iteration. Implements the `F` operator.
void bfpp_fb_request_flush(void);

// Block until the next frame has been presented to the SDL window.
// Implements the `__fb_sync` intrinsic.
void bfpp_fb_sync(void);

/* ── Pixel write ─────────────────────────────────────────────── */

// Non-temporal (cache-bypassing) pixel write into the write buffer
// at tape[fb_offset]. Implements the `__fb_pixel_nt` intrinsic.
// Pixel layout: 3 bytes per pixel (R, G, B), row-major, stride = width * 3.
void bfpp_fb_write_pixel_nt(uint8_t *tape, int fb_offset,
                            int x, int y, int width,
                            uint8_t r, uint8_t g, uint8_t b);

/* ── Query ───────────────────────────────────────────────────── */

// Returns 1 if SDL_QUIT was received, 0 otherwise.
// Convenience wrapper around `atomic_load(&bfpp_fb_quit)`.
int bfpp_fb_should_quit(void);

#endif /* BFPP_FB_PIPELINE_H */
