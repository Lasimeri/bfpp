/*
 * bfpp_rt_3d.c — BF++ 3D rendering subsystem
 *
 * Architecture:
 *   OpenGL 3.3 core profile renders to an offscreen FBO. bfpp_gl_present()
 *   reads pixels back into tape[fb_offset] (RGB24) and calls
 *   bfpp_fb_request_flush() to push the frame through the FB pipeline.
 *
 *   If GL context creation fails, all Tier 1 functions dispatch to the
 *   software rasterizer (bfpp_rt_3d_software.c) transparently.
 *
 *   BF++ tape values are Q16.16 fixed-point in 32-bit cells, little-endian.
 *   Tier 1 functions read params from tape[ptr + N*4].
 *
 * Resource limits:
 *   16 GL buffers, 16 VAOs, 16 shaders, 8 programs, 16 textures, 4 shadow FBOs.
 */

#include "bfpp_rt_3d.h"
#include "bfpp_rt_3d_shaders.h"
#include "bfpp_fb_pipeline.h"

#include <GL/glew.h>
#include <SDL2/SDL.h>
#include <SDL2/SDL_opengl.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include <time.h>

#include <pthread.h>

#ifdef __x86_64__
#include <immintrin.h>
#endif

/* ── Error codes ─────────────────────────────────────────────── */

/* When threading is active, bfpp_err is _Thread_local. Otherwise it's a plain int.
 * Both cases have external linkage so this extern declaration resolves. */
extern int bfpp_err;

#define BFPP_ERR_GENERIC     1
#define BFPP_ERR_INVALID_ARG 6

/* ── Section A: State structure ──────────────────────────────── */

static struct {
    int gpu_mode;           /* 1 = OpenGL, 0 = software                   */
    int width, height;
    uint8_t *tape;
    int fb_offset;
    int fb_size;            /* width * height * 3                          */

    /* SDL/GL context (hidden window for offscreen rendering) */
    SDL_Window   *gl_window;
    SDL_GLContext  gl_ctx;

    /* FBO for offscreen rendering */
    GLuint fbo;
    GLuint fbo_color;       /* color attachment texture                    */
    GLuint fbo_depth;       /* depth renderbuffer                          */

    /* PBO double-buffer for async readback (Phase 0) */
    GLuint pbo[2];
    int    pbo_index;
    int    pbo_initialized;
    int    pbo_first_frame;  /* skip map on first frame — no data yet */
    GLsync pbo_fence[2];    /* fence sync for PBO readback overlap    */

    /* Frame timing */
    uint64_t frame_start_ns;
    uint64_t last_frame_us;  /* microseconds */

    /* Resource tracking — BF++ programs manage IDs explicitly.
     * Static arrays; no dynamic allocation needed for the small
     * resource counts that BF++ programs use. */
    GLuint buffers[16];
    int    buffer_count;
    GLuint vaos[16];
    int    vao_count;
    GLuint shaders[16];
    int    shader_count;
    GLuint programs[8];
    int    program_count;

    /* Texture tracking */
    GLuint textures[16];
    int    texture_count;

    /* Shadow mapping */
    int    shadow_enabled;
    int    shadow_quality;  /* 0=off, 1=hard, 2=soft PCF                  */
    GLuint shadow_fbo[4];
    GLuint shadow_depth[4];
    int    shadow_map_size; /* default 1024                                */
    int    shadow_initialized;

    /* Default shader program (compiled from bfpp_rt_3d_shaders.h) */
    GLuint default_program;

    /* Multi-GPU dispatch mode (0=none, set by bfpp_rt_3d_multigpu.c) */
    int multi_mode;
} g3d;

/* ── Section B: Q16.16 helpers ───────────────────────────────── */

/* Convert Q16.16 fixed-point to float. */
static inline float q16_to_float(int32_t q)
{
    return (float)q / 65536.0f;
}

/* Convert float to Q16.16 fixed-point. */
static inline int32_t float_to_q16(float f)
{
    return (int32_t)(f * 65536.0f);
}

/* Read a Q16.16 (signed 32-bit) value from tape at addr. Little-endian. */
static inline int32_t tape_q16(uint8_t *tape, int addr)
{
    return (int32_t)((uint32_t)tape[addr]
                   | ((uint32_t)tape[addr + 1] << 8)
                   | ((uint32_t)tape[addr + 2] << 16)
                   | ((uint32_t)tape[addr + 3] << 24));
}

/* Write a Q16.16 (signed 32-bit) value to tape at addr. Little-endian. */
static inline void tape_set_q16(uint8_t *tape, int addr, int32_t val)
{
    tape[addr]     =  val        & 0xFF;
    tape[addr + 1] = (val >> 8)  & 0xFF;
    tape[addr + 2] = (val >> 16) & 0xFF;
    tape[addr + 3] = (val >> 24) & 0xFF;
}

/* Read a raw uint32 from tape (buffer IDs, enum values, etc). */
static inline uint32_t tape_u32(uint8_t *tape, int addr)
{
    return (uint32_t)tape[addr]
         | ((uint32_t)tape[addr + 1] << 8)
         | ((uint32_t)tape[addr + 2] << 16)
         | ((uint32_t)tape[addr + 3] << 24);
}

/* Write a raw uint32 to tape. */
static inline void tape_set_u32(uint8_t *tape, int addr, uint32_t val)
{
    tape[addr]     =  val        & 0xFF;
    tape[addr + 1] = (val >> 8)  & 0xFF;
    tape[addr + 2] = (val >> 16) & 0xFF;
    tape[addr + 3] = (val >> 24) & 0xFF;
}

/* ── Section C: Init / Cleanup ───────────────────────────────── */

/* Forward declarations for internal helpers */
static int  create_offscreen_fbo(int w, int h);
static void compile_default_shaders(void);
static void setup_shadow_fbo(int index);
static void init_pbo(int w, int h);
static void cleanup_pbo(void);

static void init_pbo(int w, int h) {
    int size = w * h * 3;  /* RGB24 */
    glGenBuffers(2, g3d.pbo);
    for (int i = 0; i < 2; i++) {
        glBindBuffer(GL_PIXEL_PACK_BUFFER, g3d.pbo[i]);
        glBufferData(GL_PIXEL_PACK_BUFFER, size, NULL, GL_STREAM_READ);
    }
    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    g3d.pbo_index = 0;
    g3d.pbo_initialized = 1;
    g3d.pbo_first_frame = 1;
}

static void cleanup_pbo(void) {
    if (g3d.pbo_initialized) {
        for (int i = 0; i < 2; i++) {
            if (g3d.pbo_fence[i]) {
                glDeleteSync(g3d.pbo_fence[i]);
                g3d.pbo_fence[i] = NULL;
            }
        }
        glDeleteBuffers(2, g3d.pbo);
        g3d.pbo[0] = g3d.pbo[1] = 0;
        g3d.pbo_initialized = 0;
    }
}

/*
 * Initialize the 3D subsystem.
 *
 * 1. Store dimensions, tape reference, fb_offset.
 * 2. Create a hidden SDL window + OpenGL 3.3 core context.
 * 3. If GL succeeds: glewInit, create offscreen FBO, compile defaults.
 * 4. If GL fails: fall back to software rasterizer.
 */
void bfpp_3d_init(int width, int height, uint8_t *tape, int fb_offset)
{
    memset(&g3d, 0, sizeof(g3d));

    g3d.width     = width;
    g3d.height    = height;
    g3d.tape      = tape;
    g3d.fb_offset = fb_offset;
    g3d.fb_size   = width * height * 3;
    g3d.shadow_map_size = 1024;

    /* Ensure SDL video is initialized (may already be from FB pipeline) */
    if (!SDL_WasInit(SDL_INIT_VIDEO)) {
        if (SDL_Init(SDL_INIT_VIDEO) != 0) {
            fprintf(stderr, "bfpp_3d: SDL_Init failed: %s\n", SDL_GetError());
            goto software_fallback;
        }
    }

    /* Request OpenGL 3.3 core profile */
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK,
                        SDL_GL_CONTEXT_PROFILE_CORE);
    SDL_GL_SetAttribute(SDL_GL_DOUBLEBUFFER, 1);

    /* Create a hidden window for the GL context — the FB pipeline's
     * presenter thread owns the visible window. We don't share contexts
     * across threads; instead we render offscreen and readback to tape. */
    g3d.gl_window = SDL_CreateWindow(
        "BF++ GL",
        SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
        1, 1,
        SDL_WINDOW_OPENGL | SDL_WINDOW_HIDDEN
    );
    if (!g3d.gl_window) {
        fprintf(stderr, "bfpp_3d: GL window creation failed: %s\n",
                SDL_GetError());
        goto software_fallback;
    }

    g3d.gl_ctx = SDL_GL_CreateContext(g3d.gl_window);
    if (!g3d.gl_ctx) {
        fprintf(stderr, "bfpp_3d: GL context creation failed: %s\n",
                SDL_GetError());
        SDL_DestroyWindow(g3d.gl_window);
        g3d.gl_window = NULL;
        goto software_fallback;
    }

    /* Initialize GLEW */
    glewExperimental = GL_TRUE;
    GLenum glew_err = glewInit();
    if (glew_err != GLEW_OK) {
        fprintf(stderr, "bfpp_3d: glewInit failed: %s\n",
                glewGetErrorString(glew_err));
        SDL_GL_DeleteContext(g3d.gl_ctx);
        SDL_DestroyWindow(g3d.gl_window);
        g3d.gl_ctx    = NULL;
        g3d.gl_window = NULL;
        goto software_fallback;
    }

    /* Clear any spurious GL errors from glewInit (known GLEW bug) */
    while (glGetError() != GL_NO_ERROR) {}

    /* Create offscreen FBO */
    if (!create_offscreen_fbo(width, height)) {
        fprintf(stderr, "bfpp_3d: FBO creation failed\n");
        SDL_GL_DeleteContext(g3d.gl_ctx);
        SDL_DestroyWindow(g3d.gl_window);
        g3d.gl_ctx    = NULL;
        g3d.gl_window = NULL;
        goto software_fallback;
    }

    /* Compile default shader program */
    compile_default_shaders();

    /* Initialize PBO double-buffer for async readback */
    init_pbo(width, height);

    /* Set initial GL state */
    glViewport(0, 0, width, height);
    glEnable(GL_DEPTH_TEST);
    glDepthFunc(GL_LESS);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);

    g3d.gpu_mode = 1;
    fprintf(stderr, "bfpp_3d: GPU mode (OpenGL 3.3) initialized %dx%d\n",
            width, height);
    return;

software_fallback:
    g3d.gpu_mode = 0;
    bfpp_sw_init(width, height, tape, fb_offset);
    fprintf(stderr, "bfpp_3d: software fallback initialized %dx%d\n",
            width, height);
}

/*
 * Create the offscreen FBO with color texture + depth renderbuffer.
 * Returns 1 on success, 0 on failure.
 */
static int create_offscreen_fbo(int w, int h)
{
    /* Color attachment: RGBA8 texture */
    glGenTextures(1, &g3d.fbo_color);
    glBindTexture(GL_TEXTURE_2D, g3d.fbo_color);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, w, h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    /* Depth attachment: renderbuffer */
    glGenRenderbuffers(1, &g3d.fbo_depth);
    glBindRenderbuffer(GL_RENDERBUFFER, g3d.fbo_depth);
    glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH_COMPONENT24, w, h);

    /* FBO assembly */
    glGenFramebuffers(1, &g3d.fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, g3d.fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                           GL_TEXTURE_2D, g3d.fbo_color, 0);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT,
                              GL_RENDERBUFFER, g3d.fbo_depth);

    GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (status != GL_FRAMEBUFFER_COMPLETE) {
        fprintf(stderr, "bfpp_3d: FBO incomplete (status 0x%X)\n", status);
        glDeleteFramebuffers(1, &g3d.fbo);
        glDeleteTextures(1, &g3d.fbo_color);
        glDeleteRenderbuffers(1, &g3d.fbo_depth);
        g3d.fbo = g3d.fbo_color = g3d.fbo_depth = 0;
        return 0;
    }

    /* Leave the FBO bound as the default render target */
    return 1;
}

/*
 * Tear down all 3D state.
 * Deletes FBO, shadow FBOs, all tracked GL resources, destroys context.
 * For software mode, delegates to bfpp_sw_cleanup().
 */
void bfpp_3d_cleanup(void)
{
    if (g3d.gpu_mode) {
        /* Make our context current for cleanup */
        SDL_GL_MakeCurrent(g3d.gl_window, g3d.gl_ctx);

        /* Delete tracked buffers */
        for (int i = 0; i < g3d.buffer_count; i++) {
            if (g3d.buffers[i])
                glDeleteBuffers(1, &g3d.buffers[i]);
        }

        /* Delete tracked VAOs */
        for (int i = 0; i < g3d.vao_count; i++) {
            if (g3d.vaos[i])
                glDeleteVertexArrays(1, &g3d.vaos[i]);
        }

        /* Delete tracked shaders */
        for (int i = 0; i < g3d.shader_count; i++) {
            if (g3d.shaders[i])
                glDeleteShader(g3d.shaders[i]);
        }

        /* Delete tracked programs */
        for (int i = 0; i < g3d.program_count; i++) {
            if (g3d.programs[i])
                glDeleteProgram(g3d.programs[i]);
        }

        /* Delete tracked textures */
        for (int i = 0; i < g3d.texture_count; i++) {
            if (g3d.textures[i])
                glDeleteTextures(1, &g3d.textures[i]);
        }

        /* Delete default program */
        if (g3d.default_program) {
            glDeleteProgram(g3d.default_program);
            g3d.default_program = 0;
        }

        /* Delete shadow FBOs */
        for (int i = 0; i < 4; i++) {
            if (g3d.shadow_fbo[i])
                glDeleteFramebuffers(1, &g3d.shadow_fbo[i]);
            if (g3d.shadow_depth[i])
                glDeleteTextures(1, &g3d.shadow_depth[i]);
        }

        /* Delete PBOs */
        cleanup_pbo();

        /* Delete main FBO */
        if (g3d.fbo)       glDeleteFramebuffers(1, &g3d.fbo);
        if (g3d.fbo_color) glDeleteTextures(1, &g3d.fbo_color);
        if (g3d.fbo_depth) glDeleteRenderbuffers(1, &g3d.fbo_depth);

        /* Destroy GL context + hidden window */
        SDL_GL_DeleteContext(g3d.gl_ctx);
        SDL_DestroyWindow(g3d.gl_window);
    } else {
        bfpp_sw_cleanup();
    }

    memset(&g3d, 0, sizeof(g3d));
}

/* Returns 1 if GPU backend active, 0 for software. */
int bfpp_3d_is_gpu(void)
{
    return g3d.gpu_mode;
}

/* ── Section D: Buffer management ────────────────────────────── */

/*
 * Create a GL buffer. Writes the new buffer ID to tape[ptr] as uint32.
 * Layout: tape[ptr+0] ← buffer_id (output)
 */
void bfpp_gl_create_buffer(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    if (g3d.buffer_count >= 16) {
        fprintf(stderr, "bfpp_3d: buffer limit (16) reached\n");
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    GLuint buf;
    glGenBuffers(1, &buf);
    g3d.buffers[g3d.buffer_count++] = buf;
    tape_set_u32(tape, ptr, (uint32_t)buf);
}

/*
 * Upload data to a GL buffer.
 * Layout:
 *   tape[ptr+0]  = buffer_id (uint32)
 *   tape[ptr+4]  = data_addr (uint32) — tape address of source data
 *   tape[ptr+8]  = byte_count (uint32)
 *   tape[ptr+12] = usage (uint32): 0=STATIC_DRAW, 1=DYNAMIC_DRAW, 2=STREAM_DRAW
 *
 * Source data at data_addr is Q16.16 vertex data: each 4 bytes is one
 * fixed-point value, converted to float before upload.
 */
void bfpp_gl_buffer_data(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t buf_id     = tape_u32(tape, ptr);
    uint32_t data_addr  = tape_u32(tape, ptr + 4);
    uint32_t byte_count = tape_u32(tape, ptr + 8);
    uint32_t usage_val  = tape_u32(tape, ptr + 12);

    GLenum usage;
    switch (usage_val) {
    case 1:  usage = GL_DYNAMIC_DRAW; break;
    case 2:  usage = GL_STREAM_DRAW;  break;
    default: usage = GL_STATIC_DRAW;  break;
    }

    /* Convert Q16.16 tape data to float array.
     * byte_count is the number of bytes of Q16.16 data (4 bytes per value). */
    int float_count = (int)(byte_count / 4);
    float *fdata = (float *)malloc((size_t)float_count * sizeof(float));
    if (!fdata) {
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    for (int i = 0; i < float_count; i++) {
        int32_t q = tape_q16(tape, (int)(data_addr + (uint32_t)(i * 4)));
        fdata[i] = q16_to_float(q);
    }

    glBindBuffer(GL_ARRAY_BUFFER, (GLuint)buf_id);
    glBufferData(GL_ARRAY_BUFFER,
                 (GLsizeiptr)((size_t)float_count * sizeof(float)),
                 fdata, usage);

    free(fdata);
}

/*
 * Delete a GL buffer.
 * Layout: tape[ptr+0] = buffer_id (uint32)
 */
void bfpp_gl_delete_buffer(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint buf_id = (GLuint)tape_u32(tape, ptr);
    glDeleteBuffers(1, &buf_id);

    /* Remove from tracking array */
    for (int i = 0; i < g3d.buffer_count; i++) {
        if (g3d.buffers[i] == buf_id) {
            g3d.buffers[i] = g3d.buffers[--g3d.buffer_count];
            break;
        }
    }
}

/* ── Section E: VAO management ───────────────────────────────── */

/*
 * Create a vertex array object. Writes ID to tape[ptr].
 * Layout: tape[ptr+0] ← vao_id (output)
 */
void bfpp_gl_create_vao(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    if (g3d.vao_count >= 16) {
        fprintf(stderr, "bfpp_3d: VAO limit (16) reached\n");
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    GLuint vao;
    glGenVertexArrays(1, &vao);
    g3d.vaos[g3d.vao_count++] = vao;
    tape_set_u32(tape, ptr, (uint32_t)vao);
}

/*
 * Bind a VAO.
 * Layout: tape[ptr+0] = vao_id (uint32)
 */
void bfpp_gl_bind_vao(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    GLuint vao_id = (GLuint)tape_u32(tape, ptr);
    glBindVertexArray(vao_id);
}

/*
 * Configure a vertex attribute pointer.
 * Layout:
 *   tape[ptr+0]  = attrib_index (uint32)
 *   tape[ptr+4]  = component_size (uint32) — 1, 2, 3, or 4
 *   tape[ptr+8]  = stride_bytes (uint32)
 *   tape[ptr+12] = offset_bytes (uint32)
 *
 * Always uses GL_FLOAT, not normalized. Calls glEnableVertexAttribArray.
 */
void bfpp_gl_vertex_attrib(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t index  = tape_u32(tape, ptr);
    uint32_t size   = tape_u32(tape, ptr + 4);
    uint32_t stride = tape_u32(tape, ptr + 8);
    uint32_t offset = tape_u32(tape, ptr + 12);

    if (size < 1 || size > 4) {
        bfpp_err = BFPP_ERR_INVALID_ARG;
        return;
    }

    glVertexAttribPointer((GLuint)index, (GLint)size, GL_FLOAT, GL_FALSE,
                          (GLsizei)stride,
                          (const void *)(uintptr_t)offset);
    glEnableVertexAttribArray((GLuint)index);
}

/*
 * Delete a VAO.
 * Layout: tape[ptr+0] = vao_id (uint32)
 */
void bfpp_gl_delete_vao(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint vao_id = (GLuint)tape_u32(tape, ptr);
    glDeleteVertexArrays(1, &vao_id);

    for (int i = 0; i < g3d.vao_count; i++) {
        if (g3d.vaos[i] == vao_id) {
            g3d.vaos[i] = g3d.vaos[--g3d.vao_count];
            break;
        }
    }
}

/* ── Section F: Shader management ────────────────────────────── */

/*
 * Create a shader object.
 * Layout:
 *   tape[ptr+0] = type (uint32): 0=vertex, 1=fragment
 *   tape[ptr+0] ← shader_id (output, overwrites type)
 */
void bfpp_gl_create_shader(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    if (g3d.shader_count >= 16) {
        fprintf(stderr, "bfpp_3d: shader limit (16) reached\n");
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    uint32_t type_val = tape_u32(tape, ptr);
    GLenum gl_type;
    switch (type_val) {
    case 0:  gl_type = GL_VERTEX_SHADER;   break;
    case 1:  gl_type = GL_FRAGMENT_SHADER; break;
    default:
        bfpp_err = BFPP_ERR_INVALID_ARG;
        return;
    }

    GLuint shader = glCreateShader(gl_type);
    if (!shader) {
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    g3d.shaders[g3d.shader_count++] = shader;
    tape_set_u32(tape, ptr, (uint32_t)shader);
}

/*
 * Set shader source from a null-terminated string in tape.
 * Layout:
 *   tape[ptr+0] = shader_id (uint32)
 *   tape[ptr+4] = source_addr (uint32) — tape address of GLSL string
 */
void bfpp_gl_shader_source(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint shader_id   = (GLuint)tape_u32(tape, ptr);
    uint32_t src_addr  = tape_u32(tape, ptr + 4);

    /* Source is a null-terminated string at tape[src_addr] */
    const char *src = (const char *)(tape + src_addr);
    glShaderSource(shader_id, 1, &src, NULL);
}

/*
 * Compile a shader. Sets bfpp_err on failure and prints info log.
 * Layout: tape[ptr+0] = shader_id (uint32)
 */
void bfpp_gl_compile_shader(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint shader_id = (GLuint)tape_u32(tape, ptr);
    glCompileShader(shader_id);

    GLint success;
    glGetShaderiv(shader_id, GL_COMPILE_STATUS, &success);
    if (!success) {
        char log[512];
        glGetShaderInfoLog(shader_id, sizeof(log), NULL, log);
        fprintf(stderr, "bfpp_3d: shader compile error: %s\n", log);
        bfpp_err = BFPP_ERR_GENERIC;
    }
}

/*
 * Create a shader program. Writes ID to tape[ptr].
 * Layout: tape[ptr+0] ← program_id (output)
 */
void bfpp_gl_create_program(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    if (g3d.program_count >= 8) {
        fprintf(stderr, "bfpp_3d: program limit (8) reached\n");
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    GLuint prog = glCreateProgram();
    if (!prog) {
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    g3d.programs[g3d.program_count++] = prog;
    tape_set_u32(tape, ptr, (uint32_t)prog);
}

/*
 * Attach a shader to a program.
 * Layout:
 *   tape[ptr+0] = program_id (uint32)
 *   tape[ptr+4] = shader_id (uint32)
 */
void bfpp_gl_attach_shader(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint prog_id   = (GLuint)tape_u32(tape, ptr);
    GLuint shader_id = (GLuint)tape_u32(tape, ptr + 4);
    glAttachShader(prog_id, shader_id);
}

/*
 * Link a shader program. Sets bfpp_err on failure.
 * Layout: tape[ptr+0] = program_id (uint32)
 */
void bfpp_gl_link_program(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint prog_id = (GLuint)tape_u32(tape, ptr);
    glLinkProgram(prog_id);

    GLint success;
    glGetProgramiv(prog_id, GL_LINK_STATUS, &success);
    if (!success) {
        char log[512];
        glGetProgramInfoLog(prog_id, sizeof(log), NULL, log);
        fprintf(stderr, "bfpp_3d: program link error: %s\n", log);
        bfpp_err = BFPP_ERR_GENERIC;
    }
}

/*
 * Use (bind) a shader program. Pass 0 to unbind.
 * Layout: tape[ptr+0] = program_id (uint32)
 */
void bfpp_gl_use_program(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    GLuint prog_id = (GLuint)tape_u32(tape, ptr);
    glUseProgram(prog_id);
}

/*
 * Compile the default shaders from bfpp_rt_3d_shaders.h.
 * Stores the linked program in g3d.default_program.
 * Called during init — does not consume user shader/program slots.
 */
static void compile_default_shaders(void)
{
    GLuint vs = glCreateShader(GL_VERTEX_SHADER);
    GLuint fs = glCreateShader(GL_FRAGMENT_SHADER);

    const char *vs_src = BFPP_VERT_DEFAULT;
    const char *fs_src = BFPP_FRAG_DEFAULT;

    glShaderSource(vs, 1, &vs_src, NULL);
    glCompileShader(vs);

    GLint success;
    glGetShaderiv(vs, GL_COMPILE_STATUS, &success);
    if (!success) {
        char log[512];
        glGetShaderInfoLog(vs, sizeof(log), NULL, log);
        fprintf(stderr, "bfpp_3d: default VS compile error: %s\n", log);
    }

    glShaderSource(fs, 1, &fs_src, NULL);
    glCompileShader(fs);

    glGetShaderiv(fs, GL_COMPILE_STATUS, &success);
    if (!success) {
        char log[512];
        glGetShaderInfoLog(fs, sizeof(log), NULL, log);
        fprintf(stderr, "bfpp_3d: default FS compile error: %s\n", log);
    }

    g3d.default_program = glCreateProgram();
    glAttachShader(g3d.default_program, vs);
    glAttachShader(g3d.default_program, fs);
    glLinkProgram(g3d.default_program);

    glGetProgramiv(g3d.default_program, GL_LINK_STATUS, &success);
    if (!success) {
        char log[512];
        glGetProgramInfoLog(g3d.default_program, sizeof(log), NULL, log);
        fprintf(stderr, "bfpp_3d: default program link error: %s\n", log);
    }

    /* Shaders can be detached after linking */
    glDeleteShader(vs);
    glDeleteShader(fs);

    /* Activate default program */
    glUseProgram(g3d.default_program);
}

/* ── Section G: Uniforms ─────────────────────────────────────── */

/*
 * Get uniform location. Writes location to tape[ptr+8].
 * Layout:
 *   tape[ptr+0] = program_id (uint32)
 *   tape[ptr+4] = name_addr (uint32) — tape address of null-terminated name
 *   tape[ptr+8] ← location (int32, output)
 */
void bfpp_gl_uniform_loc(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    GLuint prog_id     = (GLuint)tape_u32(tape, ptr);
    uint32_t name_addr = tape_u32(tape, ptr + 4);
    const char *name   = (const char *)(tape + name_addr);

    GLint loc = glGetUniformLocation(prog_id, name);
    tape_set_q16(tape, ptr + 8, (int32_t)loc);
}

/*
 * Set a float uniform.
 * Layout:
 *   tape[ptr+0] = location (int32)
 *   tape[ptr+4] = value (Q16.16)
 */
void bfpp_gl_uniform_1f(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    int32_t loc = tape_q16(tape, ptr);
    float   val = q16_to_float(tape_q16(tape, ptr + 4));
    glUniform1f((GLint)loc, val);
}

/*
 * Set a vec3 uniform.
 * Layout:
 *   tape[ptr+0]  = location (int32)
 *   tape[ptr+4]  = x (Q16.16)
 *   tape[ptr+8]  = y (Q16.16)
 *   tape[ptr+12] = z (Q16.16)
 */
void bfpp_gl_uniform_3f(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    int32_t loc = tape_q16(tape, ptr);
    float x = q16_to_float(tape_q16(tape, ptr + 4));
    float y = q16_to_float(tape_q16(tape, ptr + 8));
    float z = q16_to_float(tape_q16(tape, ptr + 12));
    glUniform3f((GLint)loc, x, y, z);
}

/*
 * Set a vec4 uniform.
 * Layout:
 *   tape[ptr+0]  = location (int32)
 *   tape[ptr+4]  = x (Q16.16)
 *   tape[ptr+8]  = y (Q16.16)
 *   tape[ptr+12] = z (Q16.16)
 *   tape[ptr+16] = w (Q16.16)
 */
void bfpp_gl_uniform_4f(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    int32_t loc = tape_q16(tape, ptr);
    float x = q16_to_float(tape_q16(tape, ptr + 4));
    float y = q16_to_float(tape_q16(tape, ptr + 8));
    float z = q16_to_float(tape_q16(tape, ptr + 12));
    float w = q16_to_float(tape_q16(tape, ptr + 16));
    glUniform4f((GLint)loc, x, y, z, w);
}

/*
 * Set a mat4 uniform from 16 Q16.16 values in tape.
 * Layout:
 *   tape[ptr+0]  = location (int32)
 *   tape[ptr+4]  = matrix_addr (uint32) — tape address of 16 Q16.16 values
 */
void bfpp_gl_uniform_mat4(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    int32_t  loc      = tape_q16(tape, ptr);
    uint32_t mat_addr = tape_u32(tape, ptr + 4);

    float mat[16];
    for (int i = 0; i < 16; i++) {
        mat[i] = q16_to_float(tape_q16(tape, (int)(mat_addr + (uint32_t)(i * 4))));
    }

    glUniformMatrix4fv((GLint)loc, 1, GL_FALSE, mat);
}

/* ── Section H: Drawing ──────────────────────────────────────── */

/* Map BF++ draw mode enum to GL enum. */
static GLenum mode_to_gl(uint32_t mode)
{
    switch (mode) {
    case 0:  return GL_TRIANGLES;
    case 1:  return GL_LINES;
    case 2:  return GL_POINTS;
    case 3:  return GL_TRIANGLE_STRIP;
    case 4:  return GL_TRIANGLE_FAN;
    case 5:  return GL_LINE_STRIP;
    default: return GL_TRIANGLES;
    }
}

/*
 * Clear the framebuffer with a color.
 * Layout:
 *   tape[ptr+0] = red   (uint32, 0-255)
 *   tape[ptr+4] = green (uint32, 0-255)
 *   tape[ptr+8] = blue  (uint32, 0-255)
 */
void bfpp_gl_clear(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) {
        bfpp_sw_clear(tape, ptr);
        return;
    }

    uint32_t r = tape_u32(tape, ptr);
    uint32_t g = tape_u32(tape, ptr + 4);
    uint32_t b = tape_u32(tape, ptr + 8);

    /* Bind our FBO before clearing */
    glBindFramebuffer(GL_FRAMEBUFFER, g3d.fbo);
    glClearColor((float)r / 255.0f, (float)g / 255.0f,
                 (float)b / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
}

/*
 * Draw arrays (non-indexed).
 * Layout:
 *   tape[ptr+0] = mode (uint32): 0=triangles, 1=lines, 2=points, ...
 *   tape[ptr+4] = first (uint32)
 *   tape[ptr+8] = count (uint32)
 */
void bfpp_gl_draw_arrays(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) {
        bfpp_sw_draw_triangles(tape, ptr);
        return;
    }

    uint32_t mode  = tape_u32(tape, ptr);
    uint32_t first = tape_u32(tape, ptr + 4);
    uint32_t count = tape_u32(tape, ptr + 8);

    glBindFramebuffer(GL_FRAMEBUFFER, g3d.fbo);
    glDrawArrays(mode_to_gl(mode), (GLint)first, (GLsizei)count);
}

/*
 * Draw elements (indexed).
 * Layout:
 *   tape[ptr+0]  = mode (uint32)
 *   tape[ptr+4]  = index_count (uint32)
 *   tape[ptr+8]  = index_buffer_id (uint32)
 *   tape[ptr+12] = index_data_addr (uint32) — tape addr of index data, or 0 if already in buffer
 */
void bfpp_gl_draw_elements(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) {
        bfpp_sw_draw_triangles(tape, ptr);
        return;
    }

    uint32_t mode        = tape_u32(tape, ptr);
    uint32_t count       = tape_u32(tape, ptr + 4);
    uint32_t idx_buf_id  = tape_u32(tape, ptr + 8);

    glBindFramebuffer(GL_FRAMEBUFFER, g3d.fbo);
    glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, (GLuint)idx_buf_id);
    glDrawElements(mode_to_gl(mode), (GLsizei)count,
                   GL_UNSIGNED_INT, NULL);
}

/*
 * Set the viewport.
 * Layout:
 *   tape[ptr+0]  = x (uint32)
 *   tape[ptr+4]  = y (uint32)
 *   tape[ptr+8]  = width (uint32)
 *   tape[ptr+12] = height (uint32)
 */
void bfpp_gl_viewport(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t x = tape_u32(tape, ptr);
    uint32_t y = tape_u32(tape, ptr + 4);
    uint32_t w = tape_u32(tape, ptr + 8);
    uint32_t h = tape_u32(tape, ptr + 12);

    glViewport((GLint)x, (GLint)y, (GLsizei)w, (GLsizei)h);
}

/*
 * Enable or disable depth testing.
 * Layout: tape[ptr+0] = enable (uint32): 0=disable, 1=enable
 */
void bfpp_gl_depth_test(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t enable = tape_u32(tape, ptr);
    if (enable)
        glEnable(GL_DEPTH_TEST);
    else
        glDisable(GL_DEPTH_TEST);
}

/* ── Section I: Present ──────────────────────────────────────── */

/* Parallel row-flip worker for bfpp_gl_present (Change 1: Round 3) */
typedef struct {
    uint8_t *fb;
    int stride, y_start, y_end, h;
} flip_arg_t;

static void *flip_worker(void *arg) {
    flip_arg_t *a = (flip_arg_t *)arg;
    for (int y = a->y_start; y < a->y_end; y++) {
        uint8_t *top = a->fb + y * a->stride;
        uint8_t *bot = a->fb + (a->h - 1 - y) * a->stride;
#ifdef __AVX2__
        int x;
        for (x = 0; x + 32 <= a->stride; x += 32) {
            __m256i t = _mm256_loadu_si256((__m256i*)(top + x));
            __m256i b = _mm256_loadu_si256((__m256i*)(bot + x));
            _mm256_storeu_si256((__m256i*)(top + x), b);
            _mm256_storeu_si256((__m256i*)(bot + x), t);
        }
        for (; x < a->stride; x++) {
            uint8_t tmp = top[x];
            top[x] = bot[x];
            bot[x] = tmp;
        }
#else
        uint8_t tmp;
        for (int x = 0; x < a->stride; x++) {
            tmp = top[x];
            top[x] = bot[x];
            bot[x] = tmp;
        }
#endif
    }
    return NULL;
}

#define FLIP_THREAD_COUNT 4
#define FLIP_MIN_HEIGHT   240

/*
 * Read the rendered frame from the FBO into tape[fb_offset] and
 * trigger a flush through the FB pipeline.
 *
 * Uses PBO double-buffered async readback when available:
 *   Frame N: initiate async read into pbo[current]
 *   Frame N: map pbo[previous] (already complete), copy to tape
 *   Net effect: 1 frame of readback latency, but no GPU sync stall.
 *
 * Falls back to synchronous glReadPixels if PBOs aren't initialized.
 * For software mode, delegates to bfpp_sw_present().
 */
void bfpp_gl_present(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) {
        bfpp_sw_present(tape, ptr);
        return;
    }

    /* Multi-GPU dispatch (Phase 1+) */
    /* if (g3d.multi_mode != 0) { bfpp_mgpu_present(tape, ptr); return; } */

    /* Frame timing */
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    uint64_t now_ns = (uint64_t)ts.tv_sec * 1000000000ULL + ts.tv_nsec;
    if (g3d.frame_start_ns > 0) {
        g3d.last_frame_us = (now_ns - g3d.frame_start_ns) / 1000;
    }
    g3d.frame_start_ns = now_ns;

    uint8_t *fb = tape + g3d.fb_offset;
    int w = g3d.width;
    int h = g3d.height;
    int stride = w * 3;
    int fb_size = w * h * 3;

    if (!g3d.pbo_initialized) {
        /* Fallback: synchronous readback */
        glBindFramebuffer(GL_READ_FRAMEBUFFER, g3d.fbo);
        glReadPixels(0, 0, w, h, GL_RGB, GL_UNSIGNED_BYTE, fb);
    } else {
        /* Async PBO double-buffered readback:
         * Frame N: initiate async read into pbo[current]
         * Frame N: map pbo[previous] (already complete), copy to tape
         * Net effect: 1 frame of readback latency, but no GPU sync stall */

        /* Step 1: Initiate async readback of current frame into pbo[current] */
        glBindBuffer(GL_PIXEL_PACK_BUFFER, g3d.pbo[g3d.pbo_index]);
        glBindFramebuffer(GL_READ_FRAMEBUFFER, g3d.fbo);
        glReadPixels(0, 0, w, h, GL_RGB, GL_UNSIGNED_BYTE, 0);

        /* Fence the current PBO readback so we know when it's safe to map */
        g3d.pbo_fence[g3d.pbo_index] = glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);

        /* Step 2: Map previous frame's PBO (skip on first frame) */
        if (!g3d.pbo_first_frame) {
            /* Wait for the previous frame's readback to complete */
            if (g3d.pbo_fence[g3d.pbo_index ^ 1]) {
                glClientWaitSync(g3d.pbo_fence[g3d.pbo_index ^ 1],
                                 GL_SYNC_FLUSH_COMMANDS_BIT, 5000000);
                glDeleteSync(g3d.pbo_fence[g3d.pbo_index ^ 1]);
                g3d.pbo_fence[g3d.pbo_index ^ 1] = NULL;
            }
            glBindBuffer(GL_PIXEL_PACK_BUFFER, g3d.pbo[g3d.pbo_index ^ 1]);
            void *data = glMapBufferRange(GL_PIXEL_PACK_BUFFER, 0, fb_size, GL_MAP_READ_BIT);
            if (data) {
                memcpy(fb, data, (size_t)fb_size);
                glUnmapBuffer(GL_PIXEL_PACK_BUFFER);
            }
        } else {
            g3d.pbo_first_frame = 0;
        }

        glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
        g3d.pbo_index ^= 1;
    }

    /* Flip vertically — GL is bottom-up, FB pipeline expects top-down.
     * For h >= FLIP_MIN_HEIGHT, split across FLIP_THREAD_COUNT threads.
     * Below that, threading overhead exceeds the work. */
    {
        int half = h / 2;
        if (half >= FLIP_MIN_HEIGHT) {
            /* Threaded flip */
            flip_arg_t args[FLIP_THREAD_COUNT];
            pthread_t  thr[FLIP_THREAD_COUNT];
            int chunk = half / FLIP_THREAD_COUNT;
            for (int i = 0; i < FLIP_THREAD_COUNT; i++) {
                args[i].fb     = fb;
                args[i].stride = stride;
                args[i].h      = h;
                args[i].y_start = i * chunk;
                args[i].y_end   = (i == FLIP_THREAD_COUNT - 1) ? half : (i + 1) * chunk;
                pthread_create(&thr[i], NULL, flip_worker, &args[i]);
            }
            for (int i = 0; i < FLIP_THREAD_COUNT; i++)
                pthread_join(thr[i], NULL);
        } else {
            /* Single-threaded flip */
            flip_arg_t a = { fb, stride, 0, half, h };
            flip_worker(&a);
        }
    }

    bfpp_fb_request_flush();
}

/*
 * Query last frame time in microseconds.
 * Output: tape[ptr] = frame_time_us (uint32)
 */
void bfpp_gl_frame_time(uint8_t *tape, int ptr)
{
    tape_set_u32(tape, ptr, (uint32_t)g3d.last_frame_us);
}

/* ── Section I2: Input event intrinsics ──────────────────────── */

/*
 * __input_poll: poll next input event from the FB pipeline queue.
 * Output: tape[ptr]=type, tape[ptr+4]=key/button, tape[ptr+8]=x, tape[ptr+12]=y
 * Returns 0 in type if no event available.
 */
void bfpp_gl_input_poll(uint8_t *tape, int ptr)
{
    bfpp_input_event_t evt;
    if (bfpp_input_poll(&evt)) {
        tape_set_u32(tape, ptr,      evt.type);
        tape_set_q16(tape, ptr + 4,  evt.key);
        tape_set_q16(tape, ptr + 8,  evt.x);
        tape_set_q16(tape, ptr + 12, evt.y);
    } else {
        tape_set_u32(tape, ptr, 0);
    }
}

/*
 * __input_mouse_pos: get current mouse position.
 * Output: tape[ptr]=x, tape[ptr+4]=y
 */
void bfpp_gl_input_mouse_pos(uint8_t *tape, int ptr)
{
    int x, y;
    bfpp_input_mouse_pos(&x, &y);
    tape_set_q16(tape, ptr,     x);
    tape_set_q16(tape, ptr + 4, y);
}

/*
 * __input_key_held: check if a key is held.
 * Input: tape[ptr]=scancode. Output: tape[ptr]=1 if held, 0 if not.
 */
void bfpp_gl_input_key_held(uint8_t *tape, int ptr)
{
    int scancode = tape_q16(tape, ptr);
    tape_set_u32(tape, ptr, bfpp_input_key_held(scancode) ? 1 : 0);
}

/* ── Section J: Shadow mapping ───────────────────────────────── */

/*
 * Create a shadow map FBO with a depth-only texture attachment.
 * index: shadow light index (0-3).
 */
static void setup_shadow_fbo(int index)
{
    if (index < 0 || index >= 4) return;

    int size = g3d.shadow_map_size;

    /* Depth texture */
    glGenTextures(1, &g3d.shadow_depth[index]);
    glBindTexture(GL_TEXTURE_2D, g3d.shadow_depth[index]);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_DEPTH_COMPONENT24,
                 size, size, 0, GL_DEPTH_COMPONENT, GL_FLOAT, NULL);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_BORDER);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_BORDER);

    /* Border color = 1.0 (far plane) so areas outside shadow map aren't shadowed */
    float border[] = { 1.0f, 1.0f, 1.0f, 1.0f };
    glTexParameterfv(GL_TEXTURE_2D, GL_TEXTURE_BORDER_COLOR, border);

    /* Shadow FBO */
    glGenFramebuffers(1, &g3d.shadow_fbo[index]);
    glBindFramebuffer(GL_FRAMEBUFFER, g3d.shadow_fbo[index]);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT,
                           GL_TEXTURE_2D, g3d.shadow_depth[index], 0);

    /* No color attachment — depth only */
    glDrawBuffer(GL_NONE);
    glReadBuffer(GL_NONE);

    GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (status != GL_FRAMEBUFFER_COMPLETE) {
        fprintf(stderr, "bfpp_3d: shadow FBO %d incomplete (0x%X)\n",
                index, status);
    }

    /* Restore main FBO */
    glBindFramebuffer(GL_FRAMEBUFFER, g3d.fbo);
}

/*
 * Enable shadow mapping. Creates shadow FBOs if not yet initialized.
 * Layout: (no params — reads nothing from tape)
 */
void bfpp_gl_shadow_enable(uint8_t *tape, int ptr)
{
    (void)tape; (void)ptr;

    if (!g3d.gpu_mode) { return; }

    if (!g3d.shadow_initialized) {
        for (int i = 0; i < 4; i++) {
            setup_shadow_fbo(i);
        }
        g3d.shadow_initialized = 1;
    }

    g3d.shadow_enabled = 1;
}

/*
 * Disable shadow mapping.
 * Layout: (no params)
 */
void bfpp_gl_shadow_disable(uint8_t *tape, int ptr)
{
    (void)tape; (void)ptr;

    if (!g3d.gpu_mode) { return; }
    g3d.shadow_enabled = 0;
}

/*
 * Set shadow quality level.
 * Layout: tape[ptr+0] = quality (uint32): 0=off, 1=hard, 2=soft PCF
 *
 * Optionally tape[ptr+4] = map_size (uint32): shadow map resolution.
 * If map_size is 0 or omitted, keeps current size (default 1024).
 * Changing map_size re-creates all shadow FBOs.
 */
void bfpp_gl_shadow_quality(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t quality  = tape_u32(tape, ptr);
    uint32_t map_size = tape_u32(tape, ptr + 4);

    if (quality > 2) {
        bfpp_err = BFPP_ERR_INVALID_ARG;
        return;
    }

    g3d.shadow_quality = (int)quality;

    /* Resize shadow maps if requested */
    if (map_size > 0 && (int)map_size != g3d.shadow_map_size) {
        g3d.shadow_map_size = (int)map_size;

        /* Re-create existing shadow FBOs at new size */
        if (g3d.shadow_initialized) {
            for (int i = 0; i < 4; i++) {
                if (g3d.shadow_fbo[i]) {
                    glDeleteFramebuffers(1, &g3d.shadow_fbo[i]);
                    g3d.shadow_fbo[i] = 0;
                }
                if (g3d.shadow_depth[i]) {
                    glDeleteTextures(1, &g3d.shadow_depth[i]);
                    g3d.shadow_depth[i] = 0;
                }
                setup_shadow_fbo(i);
            }
        }
    }
}

/* ── Section J.5: Textures + image loading ──────────────────── */

/* __gl_create_texture: create a texture, write ID to tape[ptr].
 * Output: tape[ptr] = texture_id */
void bfpp_gl_create_texture(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }
    if (g3d.texture_count >= 16) { bfpp_err = BFPP_ERR_GENERIC; return; }
    GLuint tex;
    glGenTextures(1, &tex);
    g3d.textures[g3d.texture_count++] = tex;
    tape_set_u32(tape, ptr, (uint32_t)tex);
}

/* __gl_texture_data: upload pixel data from tape to texture.
 * Input: tape[ptr]=tex_id, tape[ptr+4]=width, tape[ptr+8]=height,
 *        tape[ptr+12]=format (0=RGB, 1=RGBA), tape[ptr+16]=data_addr (tape offset) */
void bfpp_gl_texture_data(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t tex_id    = tape_u32(tape, ptr);
    int w              = (int)tape_u32(tape, ptr + 4);
    int h              = (int)tape_u32(tape, ptr + 8);
    int fmt            = (int)tape_u32(tape, ptr + 12);
    int data_addr      = (int)tape_u32(tape, ptr + 16);

    glBindTexture(GL_TEXTURE_2D, tex_id);
    GLenum gl_fmt = (fmt == 1) ? GL_RGBA : GL_RGB;
    glTexImage2D(GL_TEXTURE_2D, 0, gl_fmt, w, h, 0, gl_fmt,
                 GL_UNSIGNED_BYTE, &tape[data_addr]);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_REPEAT);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_REPEAT);
}

/* __gl_bind_texture: bind texture to a texture unit.
 * Input: tape[ptr]=unit (0-15), tape[ptr+4]=tex_id */
void bfpp_gl_bind_texture(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    int unit       = (int)tape_u32(tape, ptr);
    uint32_t tex_id = tape_u32(tape, ptr + 4);
    glActiveTexture(GL_TEXTURE0 + unit);
    glBindTexture(GL_TEXTURE_2D, tex_id);
}

/* __gl_delete_texture: delete a texture.
 * Input: tape[ptr]=tex_id */
void bfpp_gl_delete_texture(uint8_t *tape, int ptr)
{
    if (!g3d.gpu_mode) { return; }

    uint32_t tex_id = tape_u32(tape, ptr);
    glDeleteTextures(1, &tex_id);
    /* Swap-remove from tracking array */
    for (int i = 0; i < g3d.texture_count; i++) {
        if (g3d.textures[i] == tex_id) {
            g3d.textures[i] = g3d.textures[--g3d.texture_count];
            break;
        }
    }
}

/* __img_load: load a BMP image file from tape path into tape pixel data.
 * Input: tape[ptr]=tape_addr of null-terminated file path,
 *        tape[ptr+4]=dest_addr (where to write pixel data)
 * Output: tape[ptr+8]=width, tape[ptr+12]=height, tape[ptr+16]=channels (3=RGB)
 * Uses SDL_LoadBMP (no extra dependencies). */
void bfpp_gl_img_load(uint8_t *tape, int ptr)
{
    int path_addr = (int)tape_u32(tape, ptr);
    int dest_addr = (int)tape_u32(tape, ptr + 4);
    const char *path = (const char *)&tape[path_addr];

    SDL_Surface *surf = SDL_LoadBMP(path);
    if (!surf) {
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    /* Convert to RGB24 */
    SDL_Surface *rgb = SDL_ConvertSurfaceFormat(surf, SDL_PIXELFORMAT_RGB24, 0);
    SDL_FreeSurface(surf);
    if (!rgb) {
        bfpp_err = BFPP_ERR_GENERIC;
        return;
    }

    int w    = rgb->w;
    int h    = rgb->h;
    int size = w * h * 3;

    memcpy(&tape[dest_addr], rgb->pixels, size);
    tape_set_u32(tape, ptr + 8,  (uint32_t)w);
    tape_set_u32(tape, ptr + 12, (uint32_t)h);
    tape_set_u32(tape, ptr + 16, 3); /* RGB */

    SDL_FreeSurface(rgb);
}

/* ── Section K: Software dispatch layer ──────────────────────── */

/*
 * The Tier 1 functions above already contain inline software dispatch:
 * each function checks g3d.gpu_mode and falls through to the software
 * rasterizer equivalent when GPU mode is off.
 *
 * For functions without a direct software counterpart (VAO/shader
 * management), the software path is a no-op — the software rasterizer
 * uses its own internal state configured via bfpp_sw_set_*.
 *
 * Below are the wrapper functions that explicitly route to software
 * for the functions that don't have inline dispatch above (buffer_data,
 * etc). These are no-ops in software mode since the software rasterizer
 * reads vertex data directly from tape on draw calls.
 */

/* Software dispatch notes:
 *
 * GPU-only functions (no-op in software mode):
 *   bfpp_gl_create_buffer, bfpp_gl_buffer_data, bfpp_gl_delete_buffer
 *   bfpp_gl_create_vao, bfpp_gl_bind_vao, bfpp_gl_vertex_attrib, bfpp_gl_delete_vao
 *   bfpp_gl_create_shader, bfpp_gl_shader_source, bfpp_gl_compile_shader
 *   bfpp_gl_create_program, bfpp_gl_attach_shader, bfpp_gl_link_program
 *   bfpp_gl_use_program
 *   bfpp_gl_uniform_loc
 *   bfpp_gl_viewport, bfpp_gl_depth_test
 *   bfpp_gl_shadow_enable, bfpp_gl_shadow_disable, bfpp_gl_shadow_quality
 *
 * Functions with software dispatch (handled inline above):
 *   bfpp_gl_clear        → bfpp_sw_clear
 *   bfpp_gl_draw_arrays  → bfpp_sw_draw_triangles
 *   bfpp_gl_draw_elements→ bfpp_sw_draw_triangles
 *   bfpp_gl_present      → bfpp_sw_present
 *
 * Software uniform equivalents — BF++ programs should call these directly
 * for software mode, or the compiler can emit dispatch wrappers:
 *   bfpp_gl_uniform_1f   → bfpp_sw_set_color (for material properties)
 *   bfpp_gl_uniform_3f   → bfpp_sw_set_light (for light direction)
 *   bfpp_gl_uniform_mat4 → bfpp_sw_set_mvp (for transform matrix)
 */
