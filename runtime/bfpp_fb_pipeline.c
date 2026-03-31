#define _GNU_SOURCE  /* CPU_ZERO, CPU_SET, pthread_setaffinity_np */

/*
 * bfpp_fb_pipeline.c — BF++ 4K@60fps tiled render pipeline
 *
 * Architecture:
 *   1 presenter thread owns SDL (window, renderer, texture). It runs the
 *   render loop: poll events, snapshot the write buffer, dispatch strips to
 *   render threads, present the final frame via SDL_RenderPresent.
 *
 *   N render threads (BFPP_FB_RENDER_THREADS, default 8) each own a
 *   horizontal strip of the framebuffer. On wakeup they perform dirty
 *   detection at 64-byte granularity (memcmp staging vs prev_frame) and
 *   copy only changed chunks from staging → present buffer.
 *
 *   The main thread runs the BF++ program and writes pixels into
 *   tape[fb_offset]. It never touches SDL.
 *
 * Triple buffering:
 *   - Write buffer:   tape[fb_offset]  — BF++ program writes here
 *   - Staging buffer:  separate mmap    — snapshot of write buffer
 *   - Present buffer:  separate mmap    — what SDL reads via UpdateTexture
 *
 * Buffer allocation: MAP_HUGETLB → MAP_ANONYMOUS + MADV_HUGEPAGE → aligned_alloc
 * Core pinning: best-effort via pthread_setaffinity_np
 *
 * Threading: presenter + render threads are internal. Public API is
 * thread-safe (atomic flags, mutex+condvar synchronization).
 */

#include "bfpp_fb_pipeline.h"
#include <SDL2/SDL.h>
#include <pthread.h>
#include <stdatomic.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>

#ifdef __x86_64__
#include <immintrin.h>
#endif

#ifdef __linux__
#include <sys/mman.h>
#include <sched.h>
#endif

/* ── Strip descriptor ────────────────────────────────────────── */

typedef struct {
    int start_row;
    int end_row;
} bfpp_fb_strip_t;

/* ── Pipeline state ──────────────────────────────────────────── */

static struct {
    /* SDL objects (presenter thread only) */
    SDL_Window   *window;
    SDL_Renderer *renderer;
    SDL_Texture  *texture;

    /* Buffers (separate mmap allocations, NOT in tape) */
    uint8_t *staging;       /* snapshot of tape write buffer          */
    uint8_t *present;       /* what SDL reads via UpdateTexture       */
    uint8_t *prev_frame;    /* previous frame for dirty detection     */

    /* Tape reference */
    uint8_t *tape;
    int      fb_offset;
    int      width;
    int      height;
    int      fb_size;       /* width * height * 3                     */
    int      stride;        /* width * 3                              */

    /* Threads */
    pthread_t      presenter_thread;
    pthread_t      render_threads[BFPP_FB_RENDER_THREADS];
    bfpp_fb_strip_t strips[BFPP_FB_RENDER_THREADS];

    /* Synchronization */
    pthread_mutex_t mutex;
    pthread_cond_t  frame_cv;       /* wake render threads             */
    pthread_cond_t  done_cv;        /* render threads → presenter      */
    pthread_cond_t  sync_cv;        /* presenter → fb_sync waiters     */
    atomic_int      flush_requested;
    atomic_int      strips_remaining;
    atomic_int      running;
    uint64_t        frame_seq;
} fb;

/* Global quit flag — set by presenter thread on SDL_QUIT */
atomic_int bfpp_fb_quit = 0;

/* ── Huge-page buffer allocation ─────────────────────────────── */

/*
 * Allocation strategy (in priority order):
 *   1. mmap with MAP_HUGETLB | MAP_HUGE_2MB — explicit 2MB huge pages
 *   2. mmap MAP_ANONYMOUS + madvise(MADV_HUGEPAGE) — transparent huge pages
 *   3. aligned_alloc(64, size) — portable fallback
 *
 * All successful allocations get madvise(MADV_SEQUENTIAL) for prefetch hints.
 *
 * Returns a non-NULL pointer on success. Aborts on total allocation failure.
 */
static void *alloc_fb_buffer(size_t size)
{
    void *ptr = NULL;

#ifdef __linux__
    /* Attempt 1: explicit huge pages */
    ptr = mmap(NULL, size,
               PROT_READ | PROT_WRITE,
               MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | (21 << MAP_HUGE_SHIFT),
               -1, 0);
    if (ptr != MAP_FAILED) {
        madvise(ptr, size, MADV_SEQUENTIAL);
        return ptr;
    }

    /* Attempt 2: anonymous mmap + transparent huge page hint */
    ptr = mmap(NULL, size,
               PROT_READ | PROT_WRITE,
               MAP_PRIVATE | MAP_ANONYMOUS,
               -1, 0);
    if (ptr != MAP_FAILED) {
        madvise(ptr, size, MADV_HUGEPAGE);
        madvise(ptr, size, MADV_SEQUENTIAL);
        return ptr;
    }
#endif

    /* Attempt 3: portable aligned allocation (64-byte alignment for cache lines) */
    ptr = aligned_alloc(64, (size + 63) & ~(size_t)63);
    if (ptr) {
        memset(ptr, 0, size);
        return ptr;
    }

    fprintf(stderr, "bfpp_fb_pipeline: failed to allocate %zu bytes\n", size);
    abort();
}

/*
 * Free a buffer allocated by alloc_fb_buffer.
 * Uses munmap if the pointer looks mmap'd (page-aligned and Linux),
 * otherwise falls back to free().
 */
static void free_fb_buffer(void *ptr, size_t size)
{
    if (!ptr) return;

#ifdef __linux__
    /* If the pointer is page-aligned, it was likely mmap'd.
       munmap on a non-mmap'd pointer is undefined, so we track this
       via alignment heuristic. aligned_alloc(64, ...) won't typically
       be page-aligned unless the allocator decides to, but mmap always is. */
    size_t page_size = 4096;
    if (((uintptr_t)ptr & (page_size - 1)) == 0) {
        if (munmap(ptr, size) == 0) return;
    }
#else
    (void)size;
#endif

    free(ptr);
}

/* ── Render thread ───────────────────────────────────────────── */

/*
 * Each render thread owns a horizontal strip of the framebuffer.
 * On each frame signal it:
 *   1. Performs dirty detection (memcmp staging vs prev_frame at 64-byte granularity)
 *   2. Copies dirty chunks from staging → present buffer
 *   3. Prefetches 8 rows ahead (dual-stream) via _mm_prefetch
 *   4. Atomically decrements strips_remaining; the last thread signals done_cv
 */
static void *render_thread_func(void *arg)
{
    int idx = (int)(intptr_t)arg;

    /* Best-effort core pinning */
#ifdef __linux__
    {
        cpu_set_t cpuset;
        CPU_ZERO(&cpuset);
        CPU_SET(idx % CPU_SETSIZE, &cpuset);
        pthread_setaffinity_np(pthread_self(), sizeof(cpuset), &cpuset);
        /* Failure is silent — some systems restrict affinity */
    }
#endif

    while (atomic_load(&fb.running)) {
        /* Wait for frame signal */
        pthread_mutex_lock(&fb.mutex);
        while (atomic_load(&fb.strips_remaining) == 0 && atomic_load(&fb.running)) {
            pthread_cond_wait(&fb.frame_cv, &fb.mutex);
        }
        pthread_mutex_unlock(&fb.mutex);

        if (!atomic_load(&fb.running)) break;

        /* Process assigned strip */
        int start     = fb.strips[idx].start_row;
        int end       = fb.strips[idx].end_row;
        int row_bytes = fb.stride;

        for (int y = start; y < end; y++) {
            int offset   = y * row_bytes;
            uint8_t *src  = fb.staging    + offset;
            uint8_t *prev = fb.prev_frame + offset;
            uint8_t *dst  = fb.present    + offset;

            /* Prefetch staging + prev_frame 8 rows ahead (dual-stream) */
#ifdef __x86_64__
            if (y + 8 < end) {
                int pf_offset = (y + 8) * row_bytes;
                for (int p = 0; p < row_bytes; p += 64) {
                    _mm_prefetch((const char *)(fb.staging    + pf_offset + p), _MM_HINT_T0);
                    _mm_prefetch((const char *)(fb.prev_frame + pf_offset + p), _MM_HINT_T0);
                }
            }
#endif

            /* Dirty detection + copy at 64-byte granularity */
            for (int x = 0; x < row_bytes; x += 64) {
                int chunk = (row_bytes - x < 64) ? (row_bytes - x) : 64;
                if (memcmp(src + x, prev + x, (size_t)chunk) != 0) {
                    memcpy(dst + x, src + x, (size_t)chunk);
                }
            }
        }

        /* Signal completion — last thread to finish wakes the presenter */
        if (atomic_fetch_sub(&fb.strips_remaining, 1) == 1) {
            pthread_mutex_lock(&fb.mutex);
            pthread_cond_signal(&fb.done_cv);
            pthread_mutex_unlock(&fb.mutex);
        }
    }

    return NULL;
}

/* ── Presenter thread ────────────────────────────────────────── */

/*
 * The presenter thread is the sole owner of all SDL state.
 *
 * Loop:
 *   1. Poll SDL events (handle SDL_QUIT)
 *   2. Check flush_requested flag
 *   3. Snapshot write buffer → staging (memcpy + sfence)
 *   4. Dispatch strips to render threads (broadcast frame_cv)
 *   5. Wait for all strips to complete (done_cv)
 *   6. SDL_UpdateTexture + RenderClear + RenderCopy + RenderPresent
 *   7. Swap prev_frame ↔ staging pointers
 *   8. Increment frame_seq, broadcast sync_cv
 */
static void *presenter_thread_func(void *arg)
{
    (void)arg;

    /* ── Initialize SDL (video subsystem only) ──────────────── */
    if (SDL_Init(SDL_INIT_VIDEO) != 0) {
        fprintf(stderr, "bfpp_fb_pipeline: SDL_Init failed: %s\n", SDL_GetError());
        atomic_store(&fb.running, 0);
        atomic_store(&bfpp_fb_quit, 1);
        return NULL;
    }

    /* Create window */
    fb.window = SDL_CreateWindow(
        "BF++",
        SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
        fb.width, fb.height,
        SDL_WINDOW_SHOWN
    );
    if (!fb.window) {
        fprintf(stderr, "bfpp_fb_pipeline: SDL_CreateWindow failed: %s\n", SDL_GetError());
        SDL_Quit();
        atomic_store(&fb.running, 0);
        atomic_store(&bfpp_fb_quit, 1);
        return NULL;
    }

    /* Create renderer: try accelerated+vsync, fallback to software */
    fb.renderer = SDL_CreateRenderer(
        fb.window, -1,
        SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC
    );
    if (!fb.renderer) {
        fb.renderer = SDL_CreateRenderer(fb.window, -1, SDL_RENDERER_SOFTWARE);
    }
    if (!fb.renderer) {
        fprintf(stderr, "bfpp_fb_pipeline: SDL_CreateRenderer failed: %s\n", SDL_GetError());
        SDL_DestroyWindow(fb.window);
        SDL_Quit();
        atomic_store(&fb.running, 0);
        atomic_store(&bfpp_fb_quit, 1);
        return NULL;
    }

    /* Create streaming texture (RGB24, no alpha) */
    fb.texture = SDL_CreateTexture(
        fb.renderer,
        SDL_PIXELFORMAT_RGB24,
        SDL_TEXTUREACCESS_STREAMING,
        fb.width, fb.height
    );
    if (!fb.texture) {
        fprintf(stderr, "bfpp_fb_pipeline: SDL_CreateTexture failed: %s\n", SDL_GetError());
        SDL_DestroyRenderer(fb.renderer);
        SDL_DestroyWindow(fb.window);
        SDL_Quit();
        atomic_store(&fb.running, 0);
        atomic_store(&bfpp_fb_quit, 1);
        return NULL;
    }

    /* ── Render loop ────────────────────────────────────────── */
    while (atomic_load(&fb.running)) {
        /* Poll SDL events */
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (e.type == SDL_QUIT) {
                atomic_store(&bfpp_fb_quit, 1);
                atomic_store(&fb.running, 0);
                goto cleanup_sdl;
            }
        }

        /* Check for flush request from BF++ program */
        if (!atomic_load(&fb.flush_requested)) {
            SDL_Delay(1);
            continue;
        }
        atomic_store(&fb.flush_requested, 0);

        /* Snapshot write buffer → staging */
        memcpy(fb.staging, fb.tape + fb.fb_offset, (size_t)fb.fb_size);
#ifdef __x86_64__
        _mm_sfence();
#endif

        /* Wake render threads for strip processing */
        pthread_mutex_lock(&fb.mutex);
        atomic_store(&fb.strips_remaining, BFPP_FB_RENDER_THREADS);
        pthread_cond_broadcast(&fb.frame_cv);
        pthread_mutex_unlock(&fb.mutex);

        /* Wait for all strips to finish */
        pthread_mutex_lock(&fb.mutex);
        while (atomic_load(&fb.strips_remaining) > 0) {
            pthread_cond_wait(&fb.done_cv, &fb.mutex);
        }
        pthread_mutex_unlock(&fb.mutex);

        /* Present the frame */
        SDL_UpdateTexture(fb.texture, NULL, fb.present, fb.stride);
        SDL_RenderClear(fb.renderer);
        SDL_RenderCopy(fb.renderer, fb.texture, NULL, NULL);
        SDL_RenderPresent(fb.renderer);

        /* Swap prev_frame ↔ staging (pointer swap, no copy) */
        uint8_t *tmp  = fb.prev_frame;
        fb.prev_frame = fb.staging;
        fb.staging    = tmp;

        /* Signal sync waiters (frame_seq advanced) */
        pthread_mutex_lock(&fb.mutex);
        fb.frame_seq++;
        pthread_cond_broadcast(&fb.sync_cv);
        pthread_mutex_unlock(&fb.mutex);
    }

cleanup_sdl:
    /* Tear down SDL — only this thread created these objects */
    if (fb.texture)  SDL_DestroyTexture(fb.texture);
    if (fb.renderer) SDL_DestroyRenderer(fb.renderer);
    if (fb.window)   SDL_DestroyWindow(fb.window);
    SDL_Quit();

    return NULL;
}

/* ── Public API ──────────────────────────────────────────────── */

/*
 * Initialize the pipeline:
 *   1. Store dimensions and tape reference
 *   2. Allocate staging, present, prev_frame buffers (huge page fallback)
 *   3. Compute strip boundaries for render threads
 *   4. Initialize synchronization primitives
 *   5. Spawn render threads (pinned to cores 0..N-1)
 *   6. Spawn presenter thread (owns SDL)
 */
void bfpp_fb_pipeline_init(int w, int h, uint8_t *tape, int fb_offset)
{
    memset(&fb, 0, sizeof(fb));

    fb.width     = w;
    fb.height    = h;
    fb.stride    = w * 3;
    fb.fb_size   = w * h * 3;
    fb.tape      = tape;
    fb.fb_offset = fb_offset;

    /* Allocate triple-buffer surfaces (staging, present, prev_frame) */
    size_t buf_size = (size_t)fb.fb_size;
    fb.staging    = (uint8_t *)alloc_fb_buffer(buf_size);
    fb.present    = (uint8_t *)alloc_fb_buffer(buf_size);
    fb.prev_frame = (uint8_t *)alloc_fb_buffer(buf_size);

    /* Zero all buffers so first frame detects everything as dirty */
    memset(fb.staging,    0, buf_size);
    memset(fb.present,    0, buf_size);
    memset(fb.prev_frame, 0, buf_size);

    /* Compute strip boundaries: divide rows evenly among render threads */
    int rows_per_thread = h / BFPP_FB_RENDER_THREADS;
    int remainder       = h % BFPP_FB_RENDER_THREADS;
    int row = 0;
    for (int i = 0; i < BFPP_FB_RENDER_THREADS; i++) {
        fb.strips[i].start_row = row;
        /* Distribute remainder rows to the first `remainder` threads */
        int this_rows = rows_per_thread + (i < remainder ? 1 : 0);
        row += this_rows;
        fb.strips[i].end_row = row;
    }

    /* Initialize synchronization */
    pthread_mutex_init(&fb.mutex, NULL);
    pthread_cond_init(&fb.frame_cv, NULL);
    pthread_cond_init(&fb.done_cv, NULL);
    pthread_cond_init(&fb.sync_cv, NULL);

    atomic_store(&fb.flush_requested, 0);
    atomic_store(&fb.strips_remaining, 0);
    atomic_store(&fb.running, 1);
    atomic_store(&bfpp_fb_quit, 0);
    fb.frame_seq = 0;

    /* Spawn render threads */
    for (int i = 0; i < BFPP_FB_RENDER_THREADS; i++) {
        if (pthread_create(&fb.render_threads[i], NULL,
                           render_thread_func, (void *)(intptr_t)i) != 0) {
            fprintf(stderr, "bfpp_fb_pipeline: failed to create render thread %d\n", i);
        }
    }

    /* Spawn presenter thread (owns SDL lifecycle) */
    if (pthread_create(&fb.presenter_thread, NULL,
                       presenter_thread_func, NULL) != 0) {
        fprintf(stderr, "bfpp_fb_pipeline: failed to create presenter thread\n");
        atomic_store(&fb.running, 0);
    }
}

/*
 * Non-blocking flush request. Sets an atomic flag that the presenter
 * thread checks on each iteration. Multiple calls before the presenter
 * processes the first one are coalesced (flag is already 1).
 */
void bfpp_fb_request_flush(void)
{
    atomic_store(&fb.flush_requested, 1);
}

/*
 * Block until the next frame has been presented.
 * Records current frame_seq, then waits on sync_cv until it advances.
 * This ensures the caller's writes are visible on screen before continuing.
 */
void bfpp_fb_sync(void)
{
    pthread_mutex_lock(&fb.mutex);
    uint64_t target = fb.frame_seq + 1;
    while (fb.frame_seq < target && atomic_load(&fb.running)) {
        pthread_cond_wait(&fb.sync_cv, &fb.mutex);
    }
    pthread_mutex_unlock(&fb.mutex);
}

/*
 * Tear down the pipeline:
 *   1. Signal all threads to stop (running = 0)
 *   2. Broadcast all condvars to unblock sleeping threads
 *   3. Join all threads
 *   4. Destroy synchronization primitives
 *   5. Free buffer allocations
 */
void bfpp_fb_pipeline_cleanup(void)
{
    /* Signal shutdown */
    atomic_store(&fb.running, 0);

    /* Wake all threads that may be blocked on condvars */
    pthread_mutex_lock(&fb.mutex);
    atomic_store(&fb.strips_remaining, BFPP_FB_RENDER_THREADS);
    pthread_cond_broadcast(&fb.frame_cv);
    pthread_cond_broadcast(&fb.done_cv);
    pthread_cond_broadcast(&fb.sync_cv);
    pthread_mutex_unlock(&fb.mutex);

    /* Join render threads */
    for (int i = 0; i < BFPP_FB_RENDER_THREADS; i++) {
        pthread_join(fb.render_threads[i], NULL);
    }

    /* Join presenter thread */
    pthread_join(fb.presenter_thread, NULL);

    /* Destroy synchronization primitives */
    pthread_mutex_destroy(&fb.mutex);
    pthread_cond_destroy(&fb.frame_cv);
    pthread_cond_destroy(&fb.done_cv);
    pthread_cond_destroy(&fb.sync_cv);

    /* Free buffer allocations */
    size_t buf_size = (size_t)fb.fb_size;
    free_fb_buffer(fb.staging,    buf_size);
    free_fb_buffer(fb.present,    buf_size);
    free_fb_buffer(fb.prev_frame, buf_size);

    fb.staging    = NULL;
    fb.present    = NULL;
    fb.prev_frame = NULL;
    fb.window     = NULL;
    fb.renderer   = NULL;
    fb.texture    = NULL;
}

/*
 * Write a single pixel into the write buffer at tape[fb_offset].
 *
 * Pixel layout: 3 bytes per pixel (R, G, B), row-major order.
 * Stride = width * 3.
 *
 * Uses regular stores for individual pixel writes. Non-temporal stores
 * (_mm_stream_si32) are only beneficial for bulk sequential writes;
 * scattered single-pixel writes would thrash the write-combine buffers.
 */
void bfpp_fb_write_pixel_nt(uint8_t *tape, int fb_offset,
                             int x, int y, int width,
                             uint8_t r, uint8_t g, uint8_t b)
{
    int idx = (y * width + x) * 3;
    uint8_t *dst = tape + fb_offset + idx;
    dst[0] = r;
    dst[1] = g;
    dst[2] = b;
}

/*
 * Returns 1 if the SDL window close event has been received, 0 otherwise.
 * Convenience wrapper — avoids exposing atomic_load semantics to callers.
 */
int bfpp_fb_should_quit(void)
{
    return atomic_load(&bfpp_fb_quit);
}
