#ifndef _GNU_SOURCE
#define _GNU_SOURCE  /* CPU_ZERO, CPU_SET, pthread_setaffinity_np */
#endif

/*
 * bfpp_rt_3d_multigpu.c — BF++ multi-GPU rendering pipeline
 *
 * Architecture:
 *   EGL device enumeration → per-GPU GL 3.3 core contexts → FBO + PBO
 *   double-buffer readback. Main thread records GL commands; GPU threads
 *   replay with SFR scissor clipping or AFR round-robin assignment.
 *
 *   SFR: weighted horizontal strips, rebalanced every 30 frames.
 *   AFR: round-robin frames, in-order presentation queue.
 *   AUTO: starts SFR, switches on 10+ consecutive drops.
 *
 *   Desktop (5800X): 2 GPUs, threads on cores 8-9 (after FB pipeline).
 *   Rack (EPYC 7742): 8 GPUs, NPS4, NUMA-aware staging via mbind().
 */

#include "bfpp_rt_3d_multigpu.h"
#include "bfpp_rt_3d.h"
#include "bfpp_rt_3d_shaders.h"
#include "bfpp_fb_pipeline.h"
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/glew.h>
#include <GL/gl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <time.h>
#include <sched.h>
#ifdef __linux__
#include <sys/mman.h>
#include <unistd.h>
#endif

#if defined(__has_include)
#if __has_include(<numaif.h>)
#include <numaif.h>
#define BFPP_HAS_NUMA 1
#endif
#endif
#ifndef BFPP_HAS_NUMA
#define BFPP_HAS_NUMA 0
#endif

/* ── Forward declarations ────────────────────────────────────── */
static void init_prev_strips(int gpu_count, int fb_size);
static void cleanup_prev_strips(void);

/* ── Per-GPU context ─────────────────────────────────────────── */

typedef struct {
    EGLDisplay   egl_display;
    EGLContext   egl_context;
    GLuint       fbo, fbo_color, fbo_depth;
    GLuint       pbo[2];
    int          pbo_index, pbo_first_frame;
    GLuint       default_program;
    int          strip_y0, strip_y1;
    uint64_t     assigned_frame;
    uint8_t     *readback_buf;
    int          readback_size, readback_ready;
    uint64_t     last_frame_us;
    pthread_t    thread;
    int          gpu_index;
    int          numa_node;  /* -1 if unknown */
} bfpp_gpu_ctx_t;

/* ── Global state ────────────────────────────────────────────── */

static struct {
    bfpp_multi_mode_t mode;
    int               gpu_count, active, auto_is_sfr;
    bfpp_gpu_ctx_t    gpus[BFPP_MAX_GPUS];
    EGLDeviceEXT      egl_devices[BFPP_MAX_GPUS];
    int               egl_device_count;
    int               width, height;
    uint8_t          *tape;
    int               fb_offset, fb_size, stride;
    pthread_mutex_t   frame_mutex;
    pthread_cond_t    frame_start_cv, frame_done_cv;
    atomic_int        gpus_rendering, running;
    uint64_t          frame_seq;
    uint64_t          target_frame_us;
    uint64_t          last_present_ns;
    int               consecutive_drops;
    float             gpu_weight[BFPP_MAX_GPUS];
    int               rebalance_counter;
} mgpu;

/* ── EGL function pointers ───────────────────────────────────── */

typedef EGLBoolean (EGLAPIENTRYP PFN_eglQueryDevicesEXT)(
    EGLint, EGLDeviceEXT *, EGLint *);
typedef EGLDisplay (EGLAPIENTRYP PFN_eglGetPlatformDisplayEXT)(
    EGLenum, void *, const EGLint *);
typedef const char *(EGLAPIENTRYP PFN_eglQueryDeviceStringEXT)(
    EGLDeviceEXT, EGLint);

static PFN_eglQueryDevicesEXT       pfn_QueryDevices;
static PFN_eglGetPlatformDisplayEXT pfn_GetPlatformDisplay;
static PFN_eglQueryDeviceStringEXT  pfn_QueryDeviceString;

/* ── Command buffer ──────────────────────────────────────────── */

typedef enum {
    MGPU_CMD_CLEAR, MGPU_CMD_BIND_VAO, MGPU_CMD_USE_PROGRAM,
    MGPU_CMD_UNIFORM_1F, MGPU_CMD_UNIFORM_3F, MGPU_CMD_UNIFORM_4F,
    MGPU_CMD_UNIFORM_MAT4, MGPU_CMD_DRAW_ARRAYS, MGPU_CMD_DRAW_ELEMENTS,
    MGPU_CMD_VIEWPORT, MGPU_CMD_DEPTH_TEST
} mgpu_cmd_type_t;

typedef struct {
    mgpu_cmd_type_t type;
    union {
        struct { float r, g, b; }                    clear;
        struct { uint32_t id; }                      bind_vao;
        struct { uint32_t id; }                      use_program;
        struct { int32_t loc; float val; }           uniform_1f;
        struct { int32_t loc; float x, y, z; }       uniform_3f;
        struct { int32_t loc; float x, y, z, w; }    uniform_4f;
        struct { int32_t loc; float mat[16]; }        uniform_mat4;
        struct { uint32_t mode; int32_t first; int32_t count; } draw_arrays;
        struct { uint32_t mode; int32_t count; uint32_t type; } draw_elements;
        struct { int32_t x, y, w, h; }               viewport;
        struct { int enable; }                        depth_test;
    };
} mgpu_cmd_t;

#define MGPU_MAX_CMDS 4096

static mgpu_cmd_t cmd_buf[MGPU_MAX_CMDS];
static int         cmd_count = 0;
static mgpu_cmd_t cmd_snapshot[MGPU_MAX_CMDS];
static int         cmd_snapshot_count = 0;

/* ── AFR presentation queue ──────────────────────────────────── */

typedef struct {
    uint8_t *pixels;
    uint64_t frame_seq;
    int      gpu_index;
    atomic_int ready;
} mgpu_afr_frame_t;

static mgpu_afr_frame_t afr_queue[BFPP_MAX_GPUS];
static uint64_t afr_next_present = 0;
static int      afr_robin = 0;

/* ── Helpers ─────────────────────────────────────────────────── */

static uint64_t now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static uint8_t *alloc_readback_buf(int size, int numa_node)
{
#if BFPP_HAS_NUMA
    if (numa_node >= 0) {
        void *buf = mmap(NULL, (size_t)size, PROT_READ|PROT_WRITE,
                         MAP_PRIVATE|MAP_ANONYMOUS, -1, 0);
        if (buf != MAP_FAILED) {
            unsigned long nodemask = 1UL << numa_node;
            mbind(buf, (size_t)size, MPOL_BIND, &nodemask,
                  sizeof(nodemask) * 8, 0);
            return (uint8_t *)buf;
        }
    }
#else
    (void)numa_node;
#endif
    return (uint8_t *)aligned_alloc(64, ((size_t)size + 63) & ~63UL);
}

static void free_readback_buf(uint8_t *buf, int size, int numa_node)
{
    if (!buf) return;
#if BFPP_HAS_NUMA
    if (numa_node >= 0) { munmap(buf, (size_t)size); return; }
#else
    (void)numa_node; (void)size;
#endif
    free(buf);
}

/* Desktop (<=16 cores): GPU threads after FB pipeline on cores 8+.
 * Rack (>16 cores, NPS4): distribute across NUMA nodes, 2 GPUs/node. */
static void set_gpu_thread_affinity(int gpu_index)
{
#ifdef __linux__
    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    int nprocs = (int)sysconf(_SC_NPROCESSORS_ONLN);
    if (nprocs <= 16) {
        int core = 8 + gpu_index;
        if (core < nprocs) CPU_SET(core, &cpuset);
    } else {
        int numa = gpu_index / 2;
        int cpn  = nprocs / 4;
        int core = numa * cpn + 8 + (gpu_index % 2) * 2;
        if (core < nprocs) CPU_SET(core, &cpuset);
    }
    pthread_setaffinity_np(pthread_self(), sizeof(cpuset), &cpuset);
#else
    (void)gpu_index;
#endif
}

static int detect_numa_node(int gpu_index)
{
#ifdef __linux__
    if ((int)sysconf(_SC_NPROCESSORS_ONLN) > 16)
        return gpu_index / 2;
#else
    (void)gpu_index;
#endif
    return -1;
}

static inline uint32_t tape_read_u32_le(const uint8_t *t, int off)
{
    return (uint32_t)t[off] | ((uint32_t)t[off+1]<<8)
         | ((uint32_t)t[off+2]<<16) | ((uint32_t)t[off+3]<<24);
}

/* ── EGL enumeration ─────────────────────────────────────────── */

int bfpp_mgpu_enumerate(void)
{
    pfn_QueryDevices = (PFN_eglQueryDevicesEXT)
        eglGetProcAddress("eglQueryDevicesEXT");
    pfn_GetPlatformDisplay = (PFN_eglGetPlatformDisplayEXT)
        eglGetProcAddress("eglGetPlatformDisplayEXT");
    pfn_QueryDeviceString = (PFN_eglQueryDeviceStringEXT)
        eglGetProcAddress("eglQueryDeviceStringEXT");

    if (!pfn_QueryDevices || !pfn_GetPlatformDisplay) {
        fprintf(stderr, "bfpp_mgpu: EGL device extensions unavailable\n");
        return (mgpu.egl_device_count = 0);
    }

    EGLint count = 0;
    if (!pfn_QueryDevices(BFPP_MAX_GPUS, mgpu.egl_devices, &count)) {
        fprintf(stderr, "bfpp_mgpu: eglQueryDevicesEXT failed\n");
        return (mgpu.egl_device_count = 0);
    }
    mgpu.egl_device_count = (int)count;

    for (int i = 0; i < (int)count; i++) {
        const char *drm = pfn_QueryDeviceString
            ? pfn_QueryDeviceString(mgpu.egl_devices[i], EGL_DRM_DEVICE_FILE_EXT)
            : NULL;
        fprintf(stderr, "bfpp_mgpu: GPU %d: %s\n", i, drm ? drm : "(unknown)");
    }
    return (int)count;
}

/* ── Shader compilation ──────────────────────────────────────── */

static GLuint compile_default_program(void)
{
    GLint ok;
    char log[512];

    GLuint vs = glCreateShader(GL_VERTEX_SHADER);
    glShaderSource(vs, 1, &BFPP_VERT_DEFAULT, NULL);
    glCompileShader(vs);
    glGetShaderiv(vs, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        glGetShaderInfoLog(vs, 512, NULL, log);
        fprintf(stderr, "bfpp_mgpu: vert error: %s\n", log);
        glDeleteShader(vs);
        return 0;
    }

    GLuint fs = glCreateShader(GL_FRAGMENT_SHADER);
    glShaderSource(fs, 1, &BFPP_FRAG_DEFAULT, NULL);
    glCompileShader(fs);
    glGetShaderiv(fs, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        glGetShaderInfoLog(fs, 512, NULL, log);
        fprintf(stderr, "bfpp_mgpu: frag error: %s\n", log);
        glDeleteShader(vs); glDeleteShader(fs);
        return 0;
    }

    GLuint prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);
    glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    if (!ok) {
        glGetProgramInfoLog(prog, 512, NULL, log);
        fprintf(stderr, "bfpp_mgpu: link error: %s\n", log);
        glDeleteProgram(prog); prog = 0;
    }
    glDeleteShader(vs);
    glDeleteShader(fs);
    return prog;
}

/* ── Per-GPU FBO + PBO setup ─────────────────────────────────── */

static int create_gpu_fbo(bfpp_gpu_ctx_t *gpu, int w, int h)
{
    glGenTextures(1, &gpu->fbo_color);
    glBindTexture(GL_TEXTURE_2D, gpu->fbo_color);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB8, w, h, 0,
                 GL_RGB, GL_UNSIGNED_BYTE, NULL);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    glGenRenderbuffers(1, &gpu->fbo_depth);
    glBindRenderbuffer(GL_RENDERBUFFER, gpu->fbo_depth);
    glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH_COMPONENT24, w, h);

    glGenFramebuffers(1, &gpu->fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, gpu->fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                           GL_TEXTURE_2D, gpu->fbo_color, 0);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT,
                              GL_RENDERBUFFER, gpu->fbo_depth);

    GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (st != GL_FRAMEBUFFER_COMPLETE) {
        fprintf(stderr, "bfpp_mgpu: GPU %d FBO incomplete: 0x%x\n",
                gpu->gpu_index, st);
        return -1;
    }
    return 0;
}

static void create_gpu_pbos(bfpp_gpu_ctx_t *gpu, int fb_size)
{
    glGenBuffers(2, gpu->pbo);
    for (int i = 0; i < 2; i++) {
        glBindBuffer(GL_PIXEL_PACK_BUFFER, gpu->pbo[i]);
        glBufferData(GL_PIXEL_PACK_BUFFER, (GLsizeiptr)fb_size,
                     NULL, GL_STREAM_READ);
    }
    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    gpu->pbo_index = 0;
    gpu->pbo_first_frame = 1;
}

/* ── Command replay ──────────────────────────────────────────── */

static void mgpu_replay_commands(bfpp_gpu_ctx_t *gpu, int is_sfr,
                                  const mgpu_cmd_t *cmds, int count)
{
    for (int i = 0; i < count; i++) {
        const mgpu_cmd_t *c = &cmds[i];
        switch (c->type) {
        case MGPU_CMD_CLEAR:
            glClearColor(c->clear.r, c->clear.g, c->clear.b, 1.0f);
            glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
            break;
        case MGPU_CMD_BIND_VAO:
            glBindVertexArray(c->bind_vao.id);
            break;
        case MGPU_CMD_USE_PROGRAM:
            glUseProgram(c->use_program.id ? c->use_program.id
                                           : gpu->default_program);
            break;
        case MGPU_CMD_UNIFORM_1F:
            glUniform1f(c->uniform_1f.loc, c->uniform_1f.val);
            break;
        case MGPU_CMD_UNIFORM_3F:
            glUniform3f(c->uniform_3f.loc,
                        c->uniform_3f.x, c->uniform_3f.y, c->uniform_3f.z);
            break;
        case MGPU_CMD_UNIFORM_4F:
            glUniform4f(c->uniform_4f.loc, c->uniform_4f.x, c->uniform_4f.y,
                        c->uniform_4f.z, c->uniform_4f.w);
            break;
        case MGPU_CMD_UNIFORM_MAT4:
            glUniformMatrix4fv(c->uniform_mat4.loc, 1, GL_FALSE,
                               c->uniform_mat4.mat);
            break;
        case MGPU_CMD_DRAW_ARRAYS:
            if (is_sfr) {
                glEnable(GL_SCISSOR_TEST);
                glScissor(0, gpu->strip_y0, mgpu.width,
                          gpu->strip_y1 - gpu->strip_y0);
            }
            glDrawArrays(c->draw_arrays.mode,
                         c->draw_arrays.first, c->draw_arrays.count);
            if (is_sfr) glDisable(GL_SCISSOR_TEST);
            break;
        case MGPU_CMD_DRAW_ELEMENTS:
            if (is_sfr) {
                glEnable(GL_SCISSOR_TEST);
                glScissor(0, gpu->strip_y0, mgpu.width,
                          gpu->strip_y1 - gpu->strip_y0);
            }
            glDrawElements(c->draw_elements.mode, c->draw_elements.count,
                           c->draw_elements.type, NULL);
            if (is_sfr) glDisable(GL_SCISSOR_TEST);
            break;
        case MGPU_CMD_VIEWPORT:
            glViewport(c->viewport.x, c->viewport.y,
                       c->viewport.w, c->viewport.h);
            break;
        case MGPU_CMD_DEPTH_TEST:
            if (c->depth_test.enable) glEnable(GL_DEPTH_TEST);
            else                       glDisable(GL_DEPTH_TEST);
            break;
        }
    }
}

/* ── PBO readback ────────────────────────────────────────────── */

static void mgpu_pbo_readback(bfpp_gpu_ctx_t *gpu, int y0, int h, int stride)
{
    int cur = gpu->pbo_index;
    int prv = 1 - cur;
    int strip_size = stride * h;

    glBindFramebuffer(GL_READ_FRAMEBUFFER, gpu->fbo);

    /* Async read into pbo[cur] */
    glBindBuffer(GL_PIXEL_PACK_BUFFER, gpu->pbo[cur]);
    glReadPixels(0, y0, mgpu.width, h, GL_RGB, GL_UNSIGNED_BYTE, NULL);

    if (!gpu->pbo_first_frame) {
        /* Map pbo[prv] for previous frame's data */
        glBindBuffer(GL_PIXEL_PACK_BUFFER, gpu->pbo[prv]);
        void *mapped = glMapBuffer(GL_PIXEL_PACK_BUFFER, GL_READ_ONLY);
        if (mapped) {
            memcpy(gpu->readback_buf, mapped, (size_t)strip_size);
            glUnmapBuffer(GL_PIXEL_PACK_BUFFER);
            gpu->readback_ready = 1;
        }
    } else {
        /* First frame: synchronous fallback */
        glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
        glReadPixels(0, y0, mgpu.width, h, GL_RGB, GL_UNSIGNED_BYTE,
                     gpu->readback_buf);
        gpu->readback_ready = 1;
        gpu->pbo_first_frame = 0;
    }

    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    gpu->pbo_index = prv;
}

/* ── GPU thread ──────────────────────────────────────────────── */

static void *gpu_thread_func(void *arg)
{
    bfpp_gpu_ctx_t *gpu = (bfpp_gpu_ctx_t *)arg;
    set_gpu_thread_affinity(gpu->gpu_index);

    while (atomic_load(&mgpu.running)) {
        pthread_mutex_lock(&mgpu.frame_mutex);
        while (atomic_load(&mgpu.gpus_rendering) == 0 &&
               atomic_load(&mgpu.running))
            pthread_cond_wait(&mgpu.frame_start_cv, &mgpu.frame_mutex);
        pthread_mutex_unlock(&mgpu.frame_mutex);

        if (!atomic_load(&mgpu.running)) break;

        uint64_t t0 = now_ns();

        eglMakeCurrent(gpu->egl_display, EGL_NO_SURFACE,
                       EGL_NO_SURFACE, gpu->egl_context);
        glBindFramebuffer(GL_FRAMEBUFFER, gpu->fbo);

        int is_sfr = (mgpu.mode == BFPP_MULTI_SFR ||
                     (mgpu.mode == BFPP_MULTI_AUTO && mgpu.auto_is_sfr));

        if (is_sfr)
            glViewport(0, 0, mgpu.width, mgpu.height);

        mgpu_replay_commands(gpu, is_sfr, cmd_snapshot, cmd_snapshot_count);
        glFinish();

        if (is_sfr) {
            int sh = gpu->strip_y1 - gpu->strip_y0;
            mgpu_pbo_readback(gpu, gpu->strip_y0, sh, mgpu.stride);
        } else {
            mgpu_pbo_readback(gpu, 0, mgpu.height, mgpu.stride);
        }

        eglMakeCurrent(gpu->egl_display, EGL_NO_SURFACE,
                       EGL_NO_SURFACE, EGL_NO_CONTEXT);

        gpu->last_frame_us = (now_ns() - t0) / 1000;

        if (atomic_fetch_sub(&mgpu.gpus_rendering, 1) == 1) {
            pthread_mutex_lock(&mgpu.frame_mutex);
            pthread_cond_signal(&mgpu.frame_done_cv);
            pthread_mutex_unlock(&mgpu.frame_mutex);
        }
    }
    return NULL;
}

/* ── Init ────────────────────────────────────────────────────── */

int bfpp_mgpu_init(bfpp_multi_mode_t mode, int width, int height,
                   uint8_t *tape, int fb_offset)
{
    if (mgpu.egl_device_count < 2) {
        fprintf(stderr, "bfpp_mgpu: need >= 2 GPUs, have %d\n",
                mgpu.egl_device_count);
        return -1;
    }

    memset(&mgpu.gpus, 0, sizeof(mgpu.gpus));
    mgpu.mode       = mode;
    mgpu.gpu_count   = mgpu.egl_device_count;
    mgpu.width       = width;
    mgpu.height      = height;
    mgpu.tape        = tape;
    mgpu.fb_offset   = fb_offset;
    mgpu.fb_size     = width * height * 3;
    mgpu.stride      = width * 3;
    mgpu.frame_seq   = 0;
    mgpu.target_frame_us   = 0;
    mgpu.last_present_ns   = 0;
    mgpu.consecutive_drops = 0;
    mgpu.rebalance_counter = 0;
    mgpu.auto_is_sfr       = 1;

    EGLint cfg_attr[] = {
        EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8,
        EGL_DEPTH_SIZE, 24, EGL_NONE
    };
    EGLint ctx_attr[] = {
        EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3,
        EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
        EGL_NONE
    };

    int init_count = 0;
    for (int i = 0; i < mgpu.gpu_count; i++) {
        bfpp_gpu_ctx_t *g = &mgpu.gpus[i];
        g->gpu_index = i;
        g->numa_node = detect_numa_node(i);

        g->egl_display = pfn_GetPlatformDisplay(
            EGL_PLATFORM_DEVICE_EXT, mgpu.egl_devices[i], NULL);
        if (g->egl_display == EGL_NO_DISPLAY) {
            fprintf(stderr, "bfpp_mgpu: GPU %d: display failed\n", i);
            continue;
        }

        EGLint maj, min;
        if (!eglInitialize(g->egl_display, &maj, &min)) {
            fprintf(stderr, "bfpp_mgpu: GPU %d: eglInit failed\n", i);
            continue;
        }
        if (!eglBindAPI(EGL_OPENGL_API)) {
            eglTerminate(g->egl_display); continue;
        }

        EGLConfig cfg;
        EGLint ncfg;
        if (!eglChooseConfig(g->egl_display, cfg_attr, &cfg, 1, &ncfg)
            || ncfg < 1) {
            eglTerminate(g->egl_display); continue;
        }

        g->egl_context = eglCreateContext(
            g->egl_display, cfg, EGL_NO_CONTEXT, ctx_attr);
        if (g->egl_context == EGL_NO_CONTEXT) {
            eglTerminate(g->egl_display); continue;
        }

        eglMakeCurrent(g->egl_display, EGL_NO_SURFACE,
                       EGL_NO_SURFACE, g->egl_context);

        glewExperimental = GL_TRUE;
        if (glewInit() != GLEW_OK) {
            eglDestroyContext(g->egl_display, g->egl_context);
            eglTerminate(g->egl_display); continue;
        }
        while (glGetError() != GL_NO_ERROR) {}  /* drain GLEW artifacts */

        if (create_gpu_fbo(g, width, height) != 0) {
            eglDestroyContext(g->egl_display, g->egl_context);
            eglTerminate(g->egl_display); continue;
        }
        create_gpu_pbos(g, mgpu.fb_size);

        g->default_program = compile_default_program();

        eglMakeCurrent(g->egl_display, EGL_NO_SURFACE,
                       EGL_NO_SURFACE, EGL_NO_CONTEXT);

        g->readback_size = mgpu.fb_size;
        g->readback_buf = alloc_readback_buf(g->readback_size, g->numa_node);
        if (!g->readback_buf) continue;
        memset(g->readback_buf, 0, (size_t)g->readback_size);
        init_count++;
    }

    if (init_count < 2) {
        fprintf(stderr, "bfpp_mgpu: only %d GPU(s) ok, need >= 2\n",
                init_count);
        bfpp_mgpu_cleanup();
        return -1;
    }
    mgpu.gpu_count = init_count;

    /* Equal SFR weights */
    for (int i = 0; i < mgpu.gpu_count; i++)
        mgpu.gpu_weight[i] = 1.0f / (float)mgpu.gpu_count;

    /* Assign initial strips */
    {
        float acc = 0.0f;
        for (int i = 0; i < mgpu.gpu_count; i++) {
            mgpu.gpus[i].strip_y0 = (int)(acc * height);
            acc += mgpu.gpu_weight[i];
            mgpu.gpus[i].strip_y1 = (i == mgpu.gpu_count - 1)
                ? height : (int)(acc * height);
        }
    }

    /* AFR queue */
    for (int i = 0; i < BFPP_MAX_GPUS; i++) {
        afr_queue[i].pixels = NULL;
        atomic_store(&afr_queue[i].ready, 0);
    }
    if (mode == BFPP_MULTI_AFR || mode == BFPP_MULTI_AUTO) {
        for (int i = 0; i < mgpu.gpu_count; i++) {
            afr_queue[i].pixels = (uint8_t *)aligned_alloc(
                64, ((size_t)mgpu.fb_size + 63) & ~63UL);
            if (afr_queue[i].pixels)
                memset(afr_queue[i].pixels, 0, (size_t)mgpu.fb_size);
        }
    }
    afr_next_present = 0;
    afr_robin = 0;

    /* Sync primitives */
    pthread_mutex_init(&mgpu.frame_mutex, NULL);
    pthread_cond_init(&mgpu.frame_start_cv, NULL);
    pthread_cond_init(&mgpu.frame_done_cv, NULL);
    atomic_store(&mgpu.gpus_rendering, 0);
    atomic_store(&mgpu.running, 1);

    /* Spawn GPU threads */
    for (int i = 0; i < mgpu.gpu_count; i++) {
        if (pthread_create(&mgpu.gpus[i].thread, NULL,
                           gpu_thread_func, &mgpu.gpus[i]) != 0)
            fprintf(stderr, "bfpp_mgpu: thread %d create failed\n", i);
    }

    /* Allocate previous-strip buffers for strip change detection */
    init_prev_strips(mgpu.gpu_count, mgpu.fb_size);

    mgpu.active = 1;
    fprintf(stderr, "bfpp_mgpu: %d GPUs, mode=%d\n",
            mgpu.gpu_count, (int)mode);
    return 0;
}

/* ── Cleanup ─────────────────────────────────────────────────── */

void bfpp_mgpu_cleanup(void)
{
    if (!mgpu.active) return;

    atomic_store(&mgpu.running, 0);
    pthread_mutex_lock(&mgpu.frame_mutex);
    atomic_store(&mgpu.gpus_rendering, mgpu.gpu_count);
    pthread_cond_broadcast(&mgpu.frame_start_cv);
    pthread_cond_broadcast(&mgpu.frame_done_cv);
    pthread_mutex_unlock(&mgpu.frame_mutex);

    for (int i = 0; i < mgpu.gpu_count; i++)
        pthread_join(mgpu.gpus[i].thread, NULL);

    for (int i = 0; i < mgpu.gpu_count; i++) {
        bfpp_gpu_ctx_t *g = &mgpu.gpus[i];
        if (g->egl_context != EGL_NO_CONTEXT) {
            eglMakeCurrent(g->egl_display, EGL_NO_SURFACE,
                           EGL_NO_SURFACE, g->egl_context);
            if (g->fbo)       glDeleteFramebuffers(1, &g->fbo);
            if (g->fbo_color) glDeleteTextures(1, &g->fbo_color);
            if (g->fbo_depth) glDeleteRenderbuffers(1, &g->fbo_depth);
            if (g->pbo[0])    glDeleteBuffers(2, g->pbo);
            if (g->default_program) glDeleteProgram(g->default_program);
            eglMakeCurrent(g->egl_display, EGL_NO_SURFACE,
                           EGL_NO_SURFACE, EGL_NO_CONTEXT);
            eglDestroyContext(g->egl_display, g->egl_context);
        }
        if (g->egl_display != EGL_NO_DISPLAY)
            eglTerminate(g->egl_display);
        free_readback_buf(g->readback_buf, g->readback_size, g->numa_node);
    }

    for (int i = 0; i < BFPP_MAX_GPUS; i++) {
        free(afr_queue[i].pixels);
        afr_queue[i].pixels = NULL;
    }

    cleanup_prev_strips();

    pthread_mutex_destroy(&mgpu.frame_mutex);
    pthread_cond_destroy(&mgpu.frame_start_cv);
    pthread_cond_destroy(&mgpu.frame_done_cv);
    mgpu.active = 0;
    mgpu.gpu_count = 0;
}

int bfpp_mgpu_gpu_count(void) { return mgpu.active ? mgpu.gpu_count : 0; }

/* ── Command buffer recording ────────────────────────────────── */

static inline void cmd_push(mgpu_cmd_t c)
{
    if (cmd_count < MGPU_MAX_CMDS) cmd_buf[cmd_count++] = c;
}

void bfpp_mgpu_cmd_reset(void) { cmd_count = 0; }

void bfpp_mgpu_cmd_clear(float r, float g, float b)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_CLEAR, .clear = {r,g,b} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_bind_vao(uint32_t id)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_BIND_VAO, .bind_vao = {id} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_use_program(uint32_t id)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_USE_PROGRAM, .use_program = {id} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_uniform_1f(int32_t loc, float val)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_UNIFORM_1F,
                     .uniform_1f = {loc, val} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_uniform_3f(int32_t loc, float x, float y, float z)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_UNIFORM_3F,
                     .uniform_3f = {loc, x, y, z} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_uniform_4f(int32_t loc, float x, float y, float z, float w)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_UNIFORM_4F,
                     .uniform_4f = {loc, x, y, z, w} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_uniform_mat4(int32_t loc, const float mat[16])
{
    mgpu_cmd_t c;
    c.type = MGPU_CMD_UNIFORM_MAT4;
    c.uniform_mat4.loc = loc;
    memcpy(c.uniform_mat4.mat, mat, 16 * sizeof(float));
    cmd_push(c);
}

void bfpp_mgpu_cmd_draw_arrays(uint32_t mode, int32_t first, int32_t count)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_DRAW_ARRAYS,
                     .draw_arrays = {mode, first, count} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_draw_elements(uint32_t mode, int32_t count, uint32_t type)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_DRAW_ELEMENTS,
                     .draw_elements = {mode, count, type} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_viewport(int32_t x, int32_t y, int32_t w, int32_t h)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_VIEWPORT,
                     .viewport = {x, y, w, h} };
    cmd_push(c);
}

void bfpp_mgpu_cmd_depth_test(int enable)
{
    mgpu_cmd_t c = { .type = MGPU_CMD_DEPTH_TEST,
                     .depth_test = {enable} };
    cmd_push(c);
}

/* ── Strip change detection (skip unchanged SFR strips) ──────── */

/* Previous frame's strip data per GPU — used to skip compositor memcpy
 * when a strip is identical to the previous frame. */
static uint8_t *prev_strips[BFPP_MAX_GPUS];
static int       prev_strip_sizes[BFPP_MAX_GPUS];

static void init_prev_strips(int gpu_count, int fb_size)
{
    for (int i = 0; i < gpu_count; i++) {
        if (prev_strips[i]) { free(prev_strips[i]); prev_strips[i] = NULL; }
        prev_strips[i] = (uint8_t *)calloc(1, (size_t)fb_size);
        prev_strip_sizes[i] = fb_size;
    }
}

static void cleanup_prev_strips(void)
{
    for (int i = 0; i < BFPP_MAX_GPUS; i++) {
        free(prev_strips[i]);
        prev_strips[i] = NULL;
        prev_strip_sizes[i] = 0;
    }
}

/* ── SFR rebalancing ─────────────────────────────────────────── */

/* Adjust weights inversely proportional to frame time. Smoothed 75/25. */
static void sfr_rebalance(void)
{
    float inv[BFPP_MAX_GPUS], total = 0.0f;
    for (int i = 0; i < mgpu.gpu_count; i++) {
        uint64_t us = mgpu.gpus[i].last_frame_us;
        inv[i] = 1.0f / (float)(us > 0 ? us : 1);
        total += inv[i];
    }
    if (total < 1e-8f) return;

    float sum = 0.0f;
    for (int i = 0; i < mgpu.gpu_count; i++) {
        mgpu.gpu_weight[i] = 0.75f * mgpu.gpu_weight[i]
                            + 0.25f * (inv[i] / total);
        sum += mgpu.gpu_weight[i];
    }
    for (int i = 0; i < mgpu.gpu_count; i++)
        mgpu.gpu_weight[i] /= sum;

    float acc = 0.0f;
    for (int i = 0; i < mgpu.gpu_count; i++) {
        mgpu.gpus[i].strip_y0 = (int)(acc * mgpu.height);
        acc += mgpu.gpu_weight[i];
        mgpu.gpus[i].strip_y1 = (i == mgpu.gpu_count - 1)
            ? mgpu.height : (int)(acc * mgpu.height);
    }
}

/* ── Vertical-flip composite helper ──────────────────────────── */

/* GL origin = bottom-left, tape = top-left. Flip during composite. */
static void composite_flip(uint8_t *dst, const uint8_t *src,
                            int y0, int h, int stride, int total_h)
{
    for (int j = 0; j < h; j++) {
        int gl_row   = y0 + j;
        int tape_row = total_h - 1 - gl_row;
        memcpy(dst + tape_row * stride, src + j * stride, (size_t)stride);
    }
}

/* ── SFR present ─────────────────────────────────────────────── */

static void bfpp_mgpu_sfr_present(void)
{
    memcpy(cmd_snapshot, cmd_buf, (size_t)cmd_count * sizeof(mgpu_cmd_t));
    cmd_snapshot_count = cmd_count;

    pthread_mutex_lock(&mgpu.frame_mutex);
    atomic_store(&mgpu.gpus_rendering, mgpu.gpu_count);
    pthread_cond_broadcast(&mgpu.frame_start_cv);
    pthread_mutex_unlock(&mgpu.frame_mutex);

    pthread_mutex_lock(&mgpu.frame_mutex);
    while (atomic_load(&mgpu.gpus_rendering) > 0)
        pthread_cond_wait(&mgpu.frame_done_cv, &mgpu.frame_mutex);
    pthread_mutex_unlock(&mgpu.frame_mutex);

    uint8_t *fb = mgpu.tape + mgpu.fb_offset;
    int any_changed = 0;
    for (int g = 0; g < mgpu.gpu_count; g++) {
        bfpp_gpu_ctx_t *gpu = &mgpu.gpus[g];
        if (!gpu->readback_ready) continue;

        int strip_h = gpu->strip_y1 - gpu->strip_y0;
        int strip_size = mgpu.stride * strip_h;

        /* Skip compositor memcpy if strip is identical to previous frame */
        if (prev_strips[g] && strip_size <= prev_strip_sizes[g] &&
            memcmp(gpu->readback_buf, prev_strips[g], (size_t)strip_size) == 0) {
            gpu->readback_ready = 0;
            continue;
        }

        /* Strip changed — composite and update previous */
        composite_flip(fb, gpu->readback_buf, gpu->strip_y0,
                       strip_h, mgpu.stride, mgpu.height);
        if (prev_strips[g] && strip_size <= prev_strip_sizes[g])
            memcpy(prev_strips[g], gpu->readback_buf, (size_t)strip_size);
        gpu->readback_ready = 0;
        any_changed = 1;
    }

    /* Only flush if at least one strip changed */
    if (any_changed)
        bfpp_fb_request_flush();

    if (++mgpu.rebalance_counter >= 30) {
        sfr_rebalance();
        mgpu.rebalance_counter = 0;
    }
    mgpu.frame_seq++;
}

/* ── AFR present ─────────────────────────────────────────────── */

static void bfpp_mgpu_afr_present(void)
{
    int idx = afr_robin;
    bfpp_gpu_ctx_t *gpu = &mgpu.gpus[idx];
    mgpu_afr_frame_t *slot = &afr_queue[idx];

    /* Wait if GPU hasn't finished previous frame */
    while (atomic_load(&slot->ready) && atomic_load(&mgpu.running)) {
        struct timespec ts = { .tv_sec = 0, .tv_nsec = 100000 };
        nanosleep(&ts, NULL);
    }

    slot->frame_seq = mgpu.frame_seq;
    slot->gpu_index = idx;
    atomic_store(&slot->ready, 0);
    gpu->assigned_frame = mgpu.frame_seq;

    memcpy(cmd_snapshot, cmd_buf, (size_t)cmd_count * sizeof(mgpu_cmd_t));
    cmd_snapshot_count = cmd_count;

    pthread_mutex_lock(&mgpu.frame_mutex);
    atomic_store(&mgpu.gpus_rendering, 1);
    pthread_cond_broadcast(&mgpu.frame_start_cv);
    pthread_mutex_unlock(&mgpu.frame_mutex);

    afr_robin = (afr_robin + 1) % mgpu.gpu_count;

    /* Present next in-order frame if ready */
    int pidx = (int)(afr_next_present % (uint64_t)mgpu.gpu_count);
    mgpu_afr_frame_t *ps = &afr_queue[pidx];
    bfpp_gpu_ctx_t *pg = &mgpu.gpus[pidx];

    if (pg->readback_ready && ps->frame_seq == afr_next_present) {
        composite_flip(mgpu.tape + mgpu.fb_offset, pg->readback_buf,
                       0, mgpu.height, mgpu.stride, mgpu.height);
        pg->readback_ready = 0;
        atomic_store(&ps->ready, 0);
        afr_next_present++;
        bfpp_fb_request_flush();
    }

    mgpu.frame_seq++;
}

/* ── Frame pacing ────────────────────────────────────────────── */

static void frame_pace(void)
{
    if (mgpu.target_frame_us == 0) return;

    uint64_t now = now_ns();
    uint64_t target = mgpu.last_present_ns + mgpu.target_frame_us * 1000;

    if (now < target) {
        struct timespec ts;
        uint64_t rem = target - now;
        ts.tv_sec  = (time_t)(rem / 1000000000ULL);
        ts.tv_nsec = (long)(rem % 1000000000ULL);
        clock_nanosleep(CLOCK_MONOTONIC, 0, &ts, NULL);
        mgpu.consecutive_drops = 0;
    } else {
        mgpu.consecutive_drops++;
    }

    mgpu.last_present_ns = now_ns();

    /* AUTO: switch SFR↔AFR on 10+ consecutive drops */
    if (mgpu.mode == BFPP_MULTI_AUTO && mgpu.consecutive_drops >= 10) {
        mgpu.auto_is_sfr = !mgpu.auto_is_sfr;
        mgpu.consecutive_drops = 0;
        fprintf(stderr, "bfpp_mgpu: AUTO → %s\n",
                mgpu.auto_is_sfr ? "SFR" : "AFR");

        if (!mgpu.auto_is_sfr) {
            for (int i = 0; i < mgpu.gpu_count; i++) {
                if (!afr_queue[i].pixels) {
                    afr_queue[i].pixels = (uint8_t *)aligned_alloc(
                        64, ((size_t)mgpu.fb_size + 63) & ~63UL);
                    if (afr_queue[i].pixels)
                        memset(afr_queue[i].pixels, 0, (size_t)mgpu.fb_size);
                }
                atomic_store(&afr_queue[i].ready, 0);
            }
            afr_next_present = mgpu.frame_seq;
            afr_robin = 0;
        }
    }
}

/* ── Present dispatch ────────────────────────────────────────── */

void bfpp_mgpu_present(uint8_t *tape, int fb_offset)
{
    if (!mgpu.active) return;
    mgpu.tape = tape;
    mgpu.fb_offset = fb_offset;

    /* Reprojection on sustained drops */
    if (mgpu.consecutive_drops >= 3) {
        bfpp_fb_request_flush();
        frame_pace();
        return;
    }

    int use_sfr = (mgpu.mode == BFPP_MULTI_SFR) ||
                  (mgpu.mode == BFPP_MULTI_AUTO && mgpu.auto_is_sfr);

    if (use_sfr)
        bfpp_mgpu_sfr_present();
    else
        bfpp_mgpu_afr_present();

    frame_pace();
}

/* ── Intrinsic wrappers ──────────────────────────────────────── */

void bfpp_gl_multi_gpu(uint8_t *tape, int ptr)
{
    uint32_t mode = tape_read_u32_le(tape, ptr);

    if (mode == 0) { bfpp_mgpu_cleanup(); return; }

    if (mgpu.active) {
        if (mode <= 3) mgpu.mode = (bfpp_multi_mode_t)mode;
        return;
    }

    int count = bfpp_mgpu_enumerate();
    if (count <= 1) {
        fprintf(stderr, "bfpp_mgpu: %d GPU(s), staying single-GPU\n", count);
        return;
    }

    int w = mgpu.width, h = mgpu.height;
    if (w == 0 || h == 0) {
        w = (int32_t)tape_read_u32_le(tape, ptr + 4) >> 16;
        h = (int32_t)tape_read_u32_le(tape, ptr + 8) >> 16;
    }
    if (w <= 0 || h <= 0) {
        fprintf(stderr, "bfpp_mgpu: bad dimensions %dx%d\n", w, h);
        return;
    }

    int fbo = mgpu.fb_offset;
    if (fbo == 0) fbo = (int)tape_read_u32_le(tape, ptr + 12);

    bfpp_mgpu_init((bfpp_multi_mode_t)mode, w, h, tape, fbo);
}

void bfpp_gl_gpu_count(uint8_t *tape, int ptr)
{
    int c = mgpu.active ? mgpu.gpu_count : bfpp_mgpu_enumerate();
    tape[ptr]   = (uint8_t)(c & 0xFF);
    tape[ptr+1] = (uint8_t)((c >> 8) & 0xFF);
    tape[ptr+2] = (uint8_t)((c >> 16) & 0xFF);
    tape[ptr+3] = (uint8_t)((c >> 24) & 0xFF);
}
