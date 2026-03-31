#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

/*
 * bfpp_rt_3d_oracle.c — Scene Oracle: CPU-decoupled temporal rendering.
 *
 * Lock-free SPSC triple buffer decouples CPU simulation rate from GPU
 * render rate. Producer (CPU sim) writes scene state and publishes;
 * consumer (GPU render thread) acquires the latest snapshot and
 * extrapolates forward by the elapsed time since publication.
 *
 * Triple buffer protocol:
 *   3 snapshot slots. Producer writes to write_idx, swaps it into
 *   latest_idx on publish. Consumer swaps latest_idx into read_idx
 *   on acquire. Third slot absorbs timing mismatches.
 *
 * Extrapolation:
 *   Linear position via velocity * dt.
 *   Angular rotation via Rodrigues formula around angular_velocity axis.
 *   Confidence degrades linearly: 1.0 - (dt / extrap_max_ms).
 *   Clamped to extrap_max_ms to prevent runaway prediction.
 *
 * Hardware targets:
 *   Desktop (5800X): 3 x ~30KB snapshots = ~90KB, fits L3 (32MB).
 *     Single snapshot fits L2 (512KB/core).
 *   Rack (EPYC 7742): 90KB fits any CCD's L3 partition (32MB).
 *     Producer should be pinned to same NUMA node as triple buffer.
 *     All slots _Alignas(64) to prevent false sharing.
 */

#include "bfpp_rt_3d_oracle.h"
#include "bfpp_rt_3d.h"
#include "bfpp_fb_pipeline.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <math.h>
#include <time.h>
#include <pthread.h>

#ifdef __linux__
#include <sched.h>
#endif

/* ── Section A: Lock-Free SPSC Triple Buffer ─────────────────── */

/*
 * Three snapshot slots. Producer writes to one, consumer reads another,
 * third holds the most recently completed write ("latest").
 *
 * Memory ordering:
 *   - latest_idx uses release/acquire to ensure snapshot data is visible
 *     to the consumer before the index swap.
 *   - read_lock uses release so the producer sees it before choosing
 *     which slot to reclaim.
 *   - write_idx is producer-private; relaxed ordering suffices.
 */

typedef struct {
    _Alignas(64)
    bfpp_scene_snapshot_t slots[3];

    _Alignas(64)
    _Atomic uint8_t write_idx;   /* producer writes here                */
    _Atomic uint8_t latest_idx;  /* most recent completed write         */
    _Atomic uint8_t read_idx;    /* consumer reads from here            */
    _Atomic uint8_t read_lock;   /* 1 if consumer is reading            */
} triple_buf_t;

/*
 * tb_init — reset triple buffer indices.
 * Slot 0 = write, slot 1 = latest, slot 2 = read.
 */
static void tb_init(triple_buf_t *tb)
{
    memset(tb->slots, 0, sizeof(tb->slots));
    atomic_store_explicit(&tb->write_idx,  0, memory_order_relaxed);
    atomic_store_explicit(&tb->latest_idx, 1, memory_order_relaxed);
    atomic_store_explicit(&tb->read_idx,   2, memory_order_relaxed);
    atomic_store_explicit(&tb->read_lock,  0, memory_order_relaxed);
}

/*
 * tb_write_slot — get mutable pointer to current write slot.
 * Only the producer calls this.
 */
static bfpp_scene_snapshot_t *tb_write_slot(triple_buf_t *tb)
{
    uint8_t wi = atomic_load_explicit(&tb->write_idx, memory_order_relaxed);
    return &tb->slots[wi];
}

/*
 * tb_publish — swap write slot into latest.
 *
 * Protocol:
 *   1. Exchange write_idx with latest_idx (release: data visible first).
 *   2. Reclaim old latest as next write buffer, unless the consumer
 *      holds it — in that case, use the third slot.
 */
static void tb_publish(triple_buf_t *tb)
{
    uint8_t wi = atomic_load_explicit(&tb->write_idx, memory_order_relaxed);

    /* Swap write_idx into latest, get old latest back */
    uint8_t old_latest = atomic_exchange_explicit(
        &tb->latest_idx, wi, memory_order_release);

    /* Choose next write slot: prefer old_latest, avoid read slot if locked */
    uint8_t ri = atomic_load_explicit(&tb->read_idx, memory_order_acquire);
    if (old_latest == ri &&
        atomic_load_explicit(&tb->read_lock, memory_order_acquire)) {
        /* Consumer holds old_latest — use the third slot */
        uint8_t third = 3 - wi - ri;
        atomic_store_explicit(&tb->write_idx, third, memory_order_relaxed);
    } else {
        atomic_store_explicit(&tb->write_idx, old_latest, memory_order_relaxed);
    }
}

/*
 * tb_acquire — consumer swaps latest into read slot.
 * Returns pointer to the acquired snapshot. Sets read_lock.
 *
 * Non-blocking: always returns the most recent published data.
 * Returns NULL if no data has been published yet (frame_seq == 0).
 */
static const bfpp_scene_snapshot_t *tb_acquire(triple_buf_t *tb)
{
    /* Swap latest into read */
    uint8_t old_read = atomic_load_explicit(&tb->read_idx, memory_order_relaxed);
    uint8_t new_read = atomic_exchange_explicit(
        &tb->latest_idx, old_read, memory_order_acq_rel);

    atomic_store_explicit(&tb->read_idx, new_read, memory_order_relaxed);
    atomic_store_explicit(&tb->read_lock, 1, memory_order_release);

    const bfpp_scene_snapshot_t *snap = &tb->slots[new_read];

    /* No data published yet */
    if (snap->frame_seq == 0) {
        atomic_store_explicit(&tb->read_lock, 0, memory_order_release);
        return NULL;
    }

    return snap;
}

/*
 * tb_release — consumer releases the read slot.
 */
static void tb_release(triple_buf_t *tb)
{
    atomic_store_explicit(&tb->read_lock, 0, memory_order_release);
}

/* ── Section B: Oracle State ─────────────────────────────────── */

static struct {
    triple_buf_t buffer;

    /* Mutable CPU-side working copy. Producer builds scene here,
     * then memcpy's into the write slot on publish. */
    bfpp_scene_snapshot_t staging;

    float    extrap_max_ms;   /* max extrapolation lookahead (default 20ms) */
    int      extrap_enabled;  /* 0 = disabled, 1 = enabled                  */
    int      stale_frames;    /* frames since last publish                  */

    uint64_t last_publish_ns;
    uint64_t publish_seq;

    atomic_int initialized;
} oracle;

/* ── Section C: Timestamp Helper ─────────────────────────────── */

static uint64_t now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

/* ── Section D: Init / Cleanup ───────────────────────────────── */

/*
 * bfpp_oracle_init — zero all state, set defaults.
 * Called once during 3D subsystem startup.
 */
void bfpp_oracle_init(void)
{
    memset(&oracle, 0, sizeof(oracle));
    tb_init(&oracle.buffer);

    oracle.extrap_max_ms  = 20.0f;
    oracle.extrap_enabled = 0;
    oracle.stale_frames   = 0;
    oracle.last_publish_ns = 0;
    oracle.publish_seq    = 0;

    atomic_store(&oracle.initialized, 1);
}

/*
 * bfpp_oracle_cleanup — tear down oracle state.
 * Safe to call multiple times.
 */
void bfpp_oracle_cleanup(void)
{
    atomic_store(&oracle.initialized, 0);

    /* Zero everything to prevent stale reads */
    memset(&oracle.buffer, 0, sizeof(oracle.buffer));
    memset(&oracle.staging, 0, sizeof(oracle.staging));

    oracle.extrap_max_ms  = 0;
    oracle.extrap_enabled = 0;
    oracle.stale_frames   = 0;
    oracle.last_publish_ns = 0;
    oracle.publish_seq    = 0;
}

/* ── Section E: Scene State Setters ──────────────────────────── */

/*
 * bfpp_oracle_set_object — write object state into staging snapshot.
 *
 * Caller provides the full model matrix, velocity vectors, color,
 * and GL draw parameters. Object is marked active.
 * Updates object_count to track the high-water mark.
 */
void bfpp_oracle_set_object(int obj_id, const float model[16],
                            const float velocity[3],
                            const float angular_vel[3],
                            const float color[3],
                            uint32_t vao_id, int vertex_count,
                            uint32_t draw_mode)
{
    if (obj_id < 0 || obj_id >= ORACLE_MAX_OBJECTS) return;

    bfpp_scene_object_t *obj = &oracle.staging.objects[obj_id];

    memcpy(obj->model_mat, model, sizeof(float) * 16);
    memcpy(obj->velocity, velocity, sizeof(float) * 3);
    memcpy(obj->angular_velocity, angular_vel, sizeof(float) * 3);
    memcpy(obj->color, color, sizeof(float) * 3);

    obj->vao_id       = vao_id;
    obj->vertex_count  = vertex_count;
    obj->draw_mode     = draw_mode;
    obj->active        = 1;
    obj->confidence    = 1.0f;

    /* Track high-water mark for object_count */
    if (obj_id + 1 > oracle.staging.object_count)
        oracle.staging.object_count = obj_id + 1;
}

/*
 * bfpp_oracle_set_camera — set view/projection matrices and eye position.
 */
void bfpp_oracle_set_camera(const float view[16], const float proj[16],
                            const float pos[3])
{
    memcpy(oracle.staging.view_mat, view, sizeof(float) * 16);
    memcpy(oracle.staging.proj_mat, proj, sizeof(float) * 16);
    memcpy(oracle.staging.view_pos, pos,  sizeof(float) * 3);
}

/*
 * bfpp_oracle_set_light — set a light source in the staging snapshot.
 * idx: 0-3. Out of range silently ignored.
 */
void bfpp_oracle_set_light(int idx, const float pos[3],
                           const float color[3], float intensity)
{
    if (idx < 0 || idx >= 4) return;

    memcpy(oracle.staging.light_pos[idx],   pos,   sizeof(float) * 3);
    memcpy(oracle.staging.light_color[idx], color, sizeof(float) * 3);
    oracle.staging.light_intensity[idx] = intensity;

    /* Track active light count */
    if (idx + 1 > oracle.staging.num_lights)
        oracle.staging.num_lights = idx + 1;
}

/*
 * bfpp_oracle_publish — copy staging into triple buffer write slot,
 * stamp with timestamp and sequence number, then publish.
 *
 * This is the atomic commit point: everything set via set_object/
 * set_camera/set_light becomes visible to GPU threads after this call.
 */
void bfpp_oracle_publish(void)
{
    if (!atomic_load(&oracle.initialized)) return;

    bfpp_scene_snapshot_t *slot = tb_write_slot(&oracle.buffer);

    /* Copy staging into write slot */
    memcpy(slot, &oracle.staging, sizeof(bfpp_scene_snapshot_t));

    /* Stamp */
    slot->timestamp_ns = now_ns();
    slot->frame_seq    = ++oracle.publish_seq;

    oracle.last_publish_ns = slot->timestamp_ns;

    /* Publish (release fence ensures data is visible before index swap) */
    tb_publish(&oracle.buffer);

    oracle.stale_frames = 0;
}

/* ── Section F: Acquire / Release ────────────────────────────── */

/*
 * bfpp_oracle_acquire — GPU-side: get latest published snapshot.
 * Non-blocking. Returns NULL if nothing has been published yet.
 * Increments stale_frames counter on each call.
 */
const bfpp_scene_snapshot_t *bfpp_oracle_acquire(void)
{
    if (!atomic_load(&oracle.initialized)) return NULL;

    const bfpp_scene_snapshot_t *snap = tb_acquire(&oracle.buffer);
    if (snap) oracle.stale_frames++;

    return snap;
}

/*
 * bfpp_oracle_release — GPU-side: release the acquired snapshot.
 * Must be called after bfpp_oracle_acquire before the next acquire.
 */
void bfpp_oracle_release(void)
{
    tb_release(&oracle.buffer);
}

/* ── Section G: Temporal Extrapolation ───────────────────────── */

/*
 * mat4_rotate_axis — build rotation matrix via Rodrigues formula.
 *
 *   R = I*cos(theta) + (1-cos(theta))*(k (x) k) + sin(theta)*K
 *
 * where k = normalized axis, K = skew-symmetric matrix of k.
 * Output is column-major 4x4 with identity w-row/col.
 */
static void mat4_rotate_axis(float out[16], const float axis[3], float angle)
{
    float c  = cosf(angle);
    float s  = sinf(angle);
    float t  = 1.0f - c;

    float x = axis[0], y = axis[1], z = axis[2];

    /* Row 0 */
    out[0]  = t * x * x + c;
    out[1]  = t * x * y + s * z;
    out[2]  = t * x * z - s * y;
    out[3]  = 0.0f;

    /* Row 1 */
    out[4]  = t * x * y - s * z;
    out[5]  = t * y * y + c;
    out[6]  = t * y * z + s * x;
    out[7]  = 0.0f;

    /* Row 2 */
    out[8]  = t * x * z + s * y;
    out[9]  = t * y * z - s * x;
    out[10] = t * z * z + c;
    out[11] = 0.0f;

    /* Row 3 */
    out[12] = 0.0f;
    out[13] = 0.0f;
    out[14] = 0.0f;
    out[15] = 1.0f;
}

/*
 * mat4_mul_inplace — pre-multiply: m = r * m.
 *
 * Column-major layout. r is applied "before" m in transform order.
 * Used to apply incremental rotation to an existing model matrix.
 */
static void mat4_mul_inplace(float m[16], const float r[16])
{
    float tmp[16];

    for (int col = 0; col < 4; col++) {
        for (int row = 0; row < 4; row++) {
            tmp[col * 4 + row] =
                r[0 * 4 + row] * m[col * 4 + 0] +
                r[1 * 4 + row] * m[col * 4 + 1] +
                r[2 * 4 + row] * m[col * 4 + 2] +
                r[3 * 4 + row] * m[col * 4 + 3];
        }
    }

    memcpy(m, tmp, sizeof(float) * 16);
}

/*
 * bfpp_oracle_extrapolate — predict scene state dt_ms into the future.
 *
 * For each active object:
 *   1. Clamp dt_ms to extrap_max_ms.
 *   2. Linear position: pos += velocity * dt.
 *   3. Angular rotation: Rodrigues around angular_velocity axis by
 *      |angular_velocity| * dt radians. Pre-multiplied onto model_mat.
 *   4. Confidence: 1.0 - (clamped_dt / extrap_max_ms).
 *
 * Modifies the snapshot in place.
 */
void bfpp_oracle_extrapolate(bfpp_scene_snapshot_t *snap, float dt_ms)
{
    if (!snap || dt_ms <= 0.0f) return;

    /* Clamp to max extrapolation distance */
    float max_ms = oracle.extrap_max_ms;
    if (max_ms <= 0.0f) max_ms = 20.0f;

    float clamped = (dt_ms > max_ms) ? max_ms : dt_ms;
    float dt_sec  = clamped / 1000.0f;

    for (int i = 0; i < snap->object_count; i++) {
        bfpp_scene_object_t *obj = &snap->objects[i];
        if (!obj->active) continue;

        /* 1. Linear position extrapolation.
         *    Column-major: position is in column 3 (indices 12, 13, 14). */
        obj->model_mat[12] += obj->velocity[0] * dt_sec;
        obj->model_mat[13] += obj->velocity[1] * dt_sec;
        obj->model_mat[14] += obj->velocity[2] * dt_sec;

        /* 2. Angular rotation extrapolation.
         *    angular_velocity is axis-angle: direction = axis, magnitude = rad/s. */
        float ax = obj->angular_velocity[0];
        float ay = obj->angular_velocity[1];
        float az = obj->angular_velocity[2];
        float omega = sqrtf(ax * ax + ay * ay + az * az);

        if (omega > 0.0001f) {
            float angle = omega * dt_sec;

            /* Normalize axis */
            float inv_omega = 1.0f / omega;
            float norm_axis[3] = {
                ax * inv_omega,
                ay * inv_omega,
                az * inv_omega
            };

            /* Build rotation matrix and pre-multiply onto model_mat */
            float rot[16];
            mat4_rotate_axis(rot, norm_axis, angle);
            mat4_mul_inplace(obj->model_mat, rot);
        }

        /* 3. Confidence degrades linearly with extrapolation distance */
        float conf = 1.0f - (clamped / max_ms);
        if (conf < 0.0f) conf = 0.0f;
        obj->confidence = conf;
    }
}

/*
 * bfpp_oracle_set_extrap_max — set the maximum extrapolation lookahead.
 * Values <= 0 are clamped to 1ms minimum.
 */
void bfpp_oracle_set_extrap_max(float max_ms)
{
    if (max_ms < 1.0f) max_ms = 1.0f;
    oracle.extrap_max_ms = max_ms;
}

float bfpp_oracle_get_extrap_max(void)
{
    return oracle.extrap_max_ms;
}

int bfpp_oracle_get_stale_frames(void)
{
    return oracle.stale_frames;
}

/* ── Section H: Oracle GPU Render Loop ───────────────────────── */

/*
 * Uniform locations for the oracle's compiled shader program.
 * Resolved once after shader compilation in oracle_compile_program().
 */
typedef struct {
    int loc_model;
    int loc_view;
    int loc_projection;
    int loc_object_color;
    int loc_view_pos;
    int loc_ambient;
    int loc_num_lights;
    /* Per-light uniforms: uLights[i].position, .color, .intensity */
    int loc_light_pos[4];
    int loc_light_color[4];
    int loc_light_intensity[4];
} oracle_uniforms_t;

/*
 * Oracle vertex shader — simplified Blinn-Phong without shadow maps.
 * CPU-side extrapolation already modified model_mat, so the shader
 * receives the predicted transform directly.
 */
static const char *ORACLE_VERT_SHADER =
    "#version 330 core\n"
    "layout(location = 0) in vec3 aPos;\n"
    "layout(location = 1) in vec3 aNormal;\n"
    "uniform mat4 uModel;\n"
    "uniform mat4 uView;\n"
    "uniform mat4 uProjection;\n"
    "out vec3 vFragPos;\n"
    "out vec3 vNormal;\n"
    "void main() {\n"
    "    vec4 worldPos = uModel * vec4(aPos, 1.0);\n"
    "    vFragPos = worldPos.xyz;\n"
    "    vNormal = mat3(transpose(inverse(uModel))) * aNormal;\n"
    "    gl_Position = uProjection * uView * worldPos;\n"
    "}\n";

/*
 * Oracle fragment shader — Blinn-Phong, up to 4 lights, no shadows.
 * Lighter than the full default shader; oracle mode trades shadow
 * quality for lower latency.
 */
static const char *ORACLE_FRAG_SHADER =
    "#version 330 core\n"
    "in vec3 vFragPos;\n"
    "in vec3 vNormal;\n"
    "\n"
    "uniform vec3 uObjectColor;\n"
    "uniform vec3 uViewPos;\n"
    "uniform vec3 uAmbient;\n"
    "\n"
    "struct Light {\n"
    "    vec3 position;\n"
    "    vec3 color;\n"
    "    float intensity;\n"
    "};\n"
    "uniform Light uLights[4];\n"
    "uniform int uNumLights;\n"
    "\n"
    "out vec4 FragColor;\n"
    "\n"
    "void main() {\n"
    "    vec3 norm = normalize(vNormal);\n"
    "    vec3 result = uAmbient * uObjectColor;\n"
    "    for (int i = 0; i < uNumLights; i++) {\n"
    "        vec3 lightDir = normalize(uLights[i].position - vFragPos);\n"
    "        float diff = max(dot(norm, lightDir), 0.0);\n"
    "        vec3 diffuse = diff * uLights[i].color * uLights[i].intensity;\n"
    "        vec3 viewDir = normalize(uViewPos - vFragPos);\n"
    "        vec3 halfDir = normalize(lightDir + viewDir);\n"
    "        float spec = pow(max(dot(norm, halfDir), 0.0), 32.0);\n"
    "        vec3 specular = spec * uLights[i].color * uLights[i].intensity * 0.5;\n"
    "        result += (diffuse + specular) * uObjectColor;\n"
    "    }\n"
    "    FragColor = vec4(result, 1.0);\n"
    "}\n";

/*
 * oracle_compile_shader — compile a single GLSL shader.
 * Returns GL shader ID, or 0 on failure (logged to stderr).
 */
static unsigned int oracle_compile_shader(unsigned int type, const char *src)
{
    /* Forward-declared to avoid pulling GL headers into this file
     * at the top level. The actual GL calls are resolved at link time
     * via GLEW. We include GL/glew.h conditionally below. */
#ifndef GL_VERTEX_SHADER
    /* If GL headers aren't available, this file compiles but the
     * GPU render loop is a no-op stub. */
    (void)type; (void)src;
    return 0;
#else
    unsigned int shader = glCreateShader(type);
    glShaderSource(shader, 1, &src, NULL);
    glCompileShader(shader);

    int success;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &success);
    if (!success) {
        char info[512];
        glGetShaderInfoLog(shader, sizeof(info), NULL, info);
        fprintf(stderr, "bfpp_oracle: shader compile error: %s\n", info);
        glDeleteShader(shader);
        return 0;
    }
    return shader;
#endif
}

/*
 * oracle_compile_program — compile + link the oracle shader program.
 * Resolves all uniform locations into `unis`.
 * Returns GL program ID, or 0 on failure.
 */
static unsigned int oracle_compile_program(oracle_uniforms_t *unis)
{
#ifndef GL_VERTEX_SHADER
    (void)unis;
    return 0;
#else
    unsigned int vs = oracle_compile_shader(GL_VERTEX_SHADER, ORACLE_VERT_SHADER);
    unsigned int fs = oracle_compile_shader(GL_FRAGMENT_SHADER, ORACLE_FRAG_SHADER);
    if (!vs || !fs) {
        if (vs) glDeleteShader(vs);
        if (fs) glDeleteShader(fs);
        return 0;
    }

    unsigned int prog = glCreateProgram();
    glAttachShader(prog, vs);
    glAttachShader(prog, fs);
    glLinkProgram(prog);

    int success;
    glGetProgramiv(prog, GL_LINK_STATUS, &success);
    if (!success) {
        char info[512];
        glGetProgramInfoLog(prog, sizeof(info), NULL, info);
        fprintf(stderr, "bfpp_oracle: program link error: %s\n", info);
        glDeleteProgram(prog);
        glDeleteShader(vs);
        glDeleteShader(fs);
        return 0;
    }

    /* Shaders can be detached after linking */
    glDeleteShader(vs);
    glDeleteShader(fs);

    /* Resolve uniform locations */
    unis->loc_model        = glGetUniformLocation(prog, "uModel");
    unis->loc_view         = glGetUniformLocation(prog, "uView");
    unis->loc_projection   = glGetUniformLocation(prog, "uProjection");
    unis->loc_object_color = glGetUniformLocation(prog, "uObjectColor");
    unis->loc_view_pos     = glGetUniformLocation(prog, "uViewPos");
    unis->loc_ambient      = glGetUniformLocation(prog, "uAmbient");
    unis->loc_num_lights   = glGetUniformLocation(prog, "uNumLights");

    /* Per-light uniforms */
    for (int i = 0; i < 4; i++) {
        char buf[64];

        snprintf(buf, sizeof(buf), "uLights[%d].position", i);
        unis->loc_light_pos[i] = glGetUniformLocation(prog, buf);

        snprintf(buf, sizeof(buf), "uLights[%d].color", i);
        unis->loc_light_color[i] = glGetUniformLocation(prog, buf);

        snprintf(buf, sizeof(buf), "uLights[%d].intensity", i);
        unis->loc_light_intensity[i] = glGetUniformLocation(prog, buf);
    }

    return prog;
#endif
}

/*
 * GL draw mode mapping: oracle draw_mode enum → GL enum.
 *   0 = GL_TRIANGLES, 1 = GL_LINES, 2 = GL_POINTS.
 */
static unsigned int oracle_gl_mode(uint32_t draw_mode)
{
#ifndef GL_TRIANGLES
    (void)draw_mode;
    return 0;
#else
    switch (draw_mode) {
    case 1:  return GL_LINES;
    case 2:  return GL_POINTS;
    default: return GL_TRIANGLES;
    }
#endif
}

/*
 * oracle_render_snapshot — render a single extrapolated snapshot.
 *
 * Assumes:
 *   - A valid GL context is current on this thread.
 *   - The FBO is bound and sized correctly.
 *   - The oracle program is compiled and uniform locations resolved.
 *
 * Steps:
 *   1. Clear color + depth.
 *   2. Set view/projection/camera uniforms.
 *   3. Set light uniforms.
 *   4. For each active object: set model/color uniforms, bind VAO, draw.
 */
static void oracle_render_snapshot(const bfpp_scene_snapshot_t *snap,
                                   unsigned int program,
                                   const oracle_uniforms_t *unis)
{
#ifndef GL_COLOR_BUFFER_BIT
    (void)snap; (void)program; (void)unis;
    return;
#else
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);

    glUseProgram(program);

    /* Camera */
    glUniformMatrix4fv(unis->loc_view, 1, GL_FALSE, snap->view_mat);
    glUniformMatrix4fv(unis->loc_projection, 1, GL_FALSE, snap->proj_mat);
    glUniform3fv(unis->loc_view_pos, 1, snap->view_pos);

    /* Ambient — hardcoded 0.1 gray. Could be made configurable. */
    float ambient[3] = {0.1f, 0.1f, 0.1f};
    glUniform3fv(unis->loc_ambient, 1, ambient);

    /* Lights */
    glUniform1i(unis->loc_num_lights, snap->num_lights);
    for (int i = 0; i < snap->num_lights && i < 4; i++) {
        glUniform3fv(unis->loc_light_pos[i], 1, snap->light_pos[i]);
        glUniform3fv(unis->loc_light_color[i], 1, snap->light_color[i]);
        glUniform1f(unis->loc_light_intensity[i], snap->light_intensity[i]);
    }

    /* Objects */
    for (int i = 0; i < snap->object_count; i++) {
        const bfpp_scene_object_t *obj = &snap->objects[i];
        if (!obj->active) continue;
        if (obj->vao_id == 0 || obj->vertex_count <= 0) continue;

        glUniformMatrix4fv(unis->loc_model, 1, GL_FALSE, obj->model_mat);
        glUniform3fv(unis->loc_object_color, 1, obj->color);

        glBindVertexArray(obj->vao_id);
        glDrawArrays(oracle_gl_mode(obj->draw_mode), 0, obj->vertex_count);
    }

    glBindVertexArray(0);
#endif
}

/*
 * Per-GPU oracle thread context. Holds the compiled program and
 * uniform locations for this GPU's GL context.
 */
typedef struct {
    unsigned int       program;
    oracle_uniforms_t  unis;
    int                gpu_index;
    int                width;
    int                height;
    unsigned int       fbo;
    unsigned int       fbo_color;
    unsigned int       fbo_depth;
    unsigned int       pbo[2];
    int                pbo_index;
    uint8_t           *tape;
    int                fb_offset;
} oracle_gpu_ctx_t;

/* Thread contexts. One per GPU in oracle mode. */
#define ORACLE_MAX_GPUS 16
static oracle_gpu_ctx_t oracle_gpu_ctx[ORACLE_MAX_GPUS];
static pthread_t        oracle_gpu_threads[ORACLE_MAX_GPUS];
static int              oracle_gpu_count = 0;

/*
 * oracle_gpu_setup_fbo — create offscreen FBO for oracle rendering.
 * Called once per GPU thread after GL context is current.
 * Returns 0 on success, -1 on failure.
 */
static int oracle_gpu_setup_fbo(oracle_gpu_ctx_t *ctx)
{
#ifndef GL_FRAMEBUFFER
    (void)ctx;
    return -1;
#else
    /* Color attachment */
    glGenTextures(1, &ctx->fbo_color);
    glBindTexture(GL_TEXTURE_2D, ctx->fbo_color);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB8,
                 ctx->width, ctx->height, 0,
                 GL_RGB, GL_UNSIGNED_BYTE, NULL);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    /* Depth attachment */
    glGenRenderbuffers(1, &ctx->fbo_depth);
    glBindRenderbuffer(GL_RENDERBUFFER, ctx->fbo_depth);
    glRenderbufferStorage(GL_RENDERBUFFER, GL_DEPTH_COMPONENT24,
                          ctx->width, ctx->height);

    /* FBO */
    glGenFramebuffers(1, &ctx->fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, ctx->fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                           GL_TEXTURE_2D, ctx->fbo_color, 0);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT,
                              GL_RENDERBUFFER, ctx->fbo_depth);

    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        fprintf(stderr, "bfpp_oracle: FBO incomplete on GPU %d\n",
                ctx->gpu_index);
        glDeleteFramebuffers(1, &ctx->fbo);
        glDeleteTextures(1, &ctx->fbo_color);
        glDeleteRenderbuffers(1, &ctx->fbo_depth);
        ctx->fbo = 0;
        return -1;
    }

    /* PBO double-buffer for async readback */
    int fb_size = ctx->width * ctx->height * 3;
    glGenBuffers(2, ctx->pbo);
    for (int i = 0; i < 2; i++) {
        glBindBuffer(GL_PIXEL_PACK_BUFFER, ctx->pbo[i]);
        glBufferData(GL_PIXEL_PACK_BUFFER, fb_size, NULL, GL_STREAM_READ);
    }
    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    ctx->pbo_index = 0;

    /* Enable depth test */
    glEnable(GL_DEPTH_TEST);
    glViewport(0, 0, ctx->width, ctx->height);
    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);

    return 0;
#endif
}

/*
 * oracle_gpu_cleanup_fbo — destroy FBO, PBOs, textures for a GPU context.
 */
static void oracle_gpu_cleanup_fbo(oracle_gpu_ctx_t *ctx)
{
#ifndef GL_FRAMEBUFFER
    (void)ctx;
    return;
#else
    if (ctx->fbo)       glDeleteFramebuffers(1, &ctx->fbo);
    if (ctx->fbo_color) glDeleteTextures(1, &ctx->fbo_color);
    if (ctx->fbo_depth) glDeleteRenderbuffers(1, &ctx->fbo_depth);
    if (ctx->pbo[0])    glDeleteBuffers(2, ctx->pbo);
    if (ctx->program)   glDeleteProgram(ctx->program);

    ctx->fbo       = 0;
    ctx->fbo_color = 0;
    ctx->fbo_depth = 0;
    ctx->pbo[0]    = 0;
    ctx->pbo[1]    = 0;
    ctx->program   = 0;
#endif
}

/*
 * oracle_gpu_readback — async PBO readback of the rendered frame.
 *
 * Double-buffered PBO pipeline:
 *   Frame N:   glReadPixels into PBO[curr]  (DMA, non-blocking)
 *   Frame N+1: map PBO[prev], memcpy to tape, unmap
 *
 * The one-frame latency is acceptable because the extrapolation
 * already compensates for timing offsets.
 */
static void oracle_gpu_readback(oracle_gpu_ctx_t *ctx)
{
#ifndef GL_PIXEL_PACK_BUFFER
    (void)ctx;
    return;
#else
    int fb_size = ctx->width * ctx->height * 3;
    int curr = ctx->pbo_index;
    int prev = 1 - curr;

    /* Initiate async read of current frame into curr PBO */
    glBindBuffer(GL_PIXEL_PACK_BUFFER, ctx->pbo[curr]);
    glReadPixels(0, 0, ctx->width, ctx->height,
                 GL_RGB, GL_UNSIGNED_BYTE, NULL);

    /* Map previous PBO and copy to tape (if not first frame) */
    glBindBuffer(GL_PIXEL_PACK_BUFFER, ctx->pbo[prev]);
    void *data = glMapBuffer(GL_PIXEL_PACK_BUFFER, GL_READ_ONLY);
    if (data) {
        uint8_t *dst = ctx->tape + ctx->fb_offset;
        const uint8_t *src = (const uint8_t *)data;

        /* GL reads bottom-up; tape is top-down. Flip rows. */
        int stride = ctx->width * 3;
        for (int y = 0; y < ctx->height; y++) {
            memcpy(dst + y * stride,
                   src + (ctx->height - 1 - y) * stride,
                   (size_t)stride);
        }
        glUnmapBuffer(GL_PIXEL_PACK_BUFFER);
    }
    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);

    ctx->pbo_index = prev;
#endif
}

/*
 * oracle_gpu_thread — autonomous render loop for one GPU.
 *
 * Loop:
 *   1. Acquire latest snapshot from triple buffer.
 *   2. Calculate extrapolation dt from snapshot timestamp.
 *   3. Deep copy + extrapolate.
 *   4. Release snapshot.
 *   5. Render extrapolated scene.
 *   6. Async PBO readback to tape.
 *   7. Flush FB pipeline.
 *
 * Stale check: if no new data for 3+ frames, skip extrapolation
 * to prevent runaway prediction artifacts.
 */
static void *oracle_gpu_thread(void *arg)
{
    oracle_gpu_ctx_t *ctx = (oracle_gpu_ctx_t *)arg;

    /* NOTE: Caller must have already made this GPU's GL context current.
     * For multi-GPU (EGL), bfpp_mgpu_make_current() handles this.
     * For single-GPU (SDL), the context is set before thread launch.
     * This thread assumes GL calls are valid from this point. */

    /* Compile oracle shader program on this context */
    ctx->program = oracle_compile_program(&ctx->unis);
    if (!ctx->program) {
        fprintf(stderr, "bfpp_oracle: failed to compile program on GPU %d\n",
                ctx->gpu_index);
        return NULL;
    }

    /* Set up offscreen FBO */
    if (oracle_gpu_setup_fbo(ctx) != 0) {
        fprintf(stderr, "bfpp_oracle: failed to create FBO on GPU %d\n",
                ctx->gpu_index);
        return NULL;
    }

#ifdef GL_FRAMEBUFFER
    glBindFramebuffer(GL_FRAMEBUFFER, ctx->fbo);
#endif

    int first_frame = 1;

    while (atomic_load(&oracle.initialized)) {
        const bfpp_scene_snapshot_t *snap = bfpp_oracle_acquire();
        if (!snap) {
            /* No data yet — yield and retry */
            sched_yield();
            continue;
        }

        /* Calculate extrapolation dt */
        uint64_t t_now = now_ns();
        float dt_ms = (float)(t_now - snap->timestamp_ns) / 1e6f;

        /* Deep copy so we can release the triple buffer slot */
        bfpp_scene_snapshot_t local;
        memcpy(&local, snap, sizeof(local));
        bfpp_oracle_release();

        /* Extrapolate unless stale or disabled */
        if (oracle.extrap_enabled && dt_ms > 0.0f &&
            oracle.stale_frames <= 3) {
            float clamped = (dt_ms > oracle.extrap_max_ms)
                          ? oracle.extrap_max_ms : dt_ms;
            bfpp_oracle_extrapolate(&local, clamped);
        }

        /* Render */
        oracle_render_snapshot(&local, ctx->program, &ctx->unis);

        /* Async readback to tape */
        if (first_frame) {
            /* First frame: just initiate the read, no map yet */
#ifdef GL_PIXEL_PACK_BUFFER
            int fb_size = ctx->width * ctx->height * 3;
            glBindBuffer(GL_PIXEL_PACK_BUFFER, ctx->pbo[ctx->pbo_index]);
            glReadPixels(0, 0, ctx->width, ctx->height,
                         GL_RGB, GL_UNSIGNED_BYTE, NULL);
            glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
            ctx->pbo_index = 1 - ctx->pbo_index;
            (void)fb_size;
#endif
            first_frame = 0;
        } else {
            oracle_gpu_readback(ctx);

            /* Signal FB pipeline to present */
            bfpp_fb_request_flush();
        }
    }

    /* Cleanup GPU resources */
    oracle_gpu_cleanup_fbo(ctx);

    return NULL;
}

/*
 * bfpp_oracle_start_gpu_threads — launch oracle render threads.
 * Called when oracle mode is enabled via bfpp_scene_mode_intrinsic.
 *
 * NOTE: This is a simplified single-GPU launcher. Multi-GPU oracle
 * mode would use bfpp_mgpu_* to create per-GPU EGL contexts and
 * launch one thread per GPU. That integration is deferred.
 *
 * For single-GPU: the GL context must be shareable or transferred.
 * In practice, the existing bfpp_rt_3d.c context (SDL hidden window)
 * can't be shared across threads without EGL. So oracle mode on
 * single-GPU reuses the main thread's context via direct calls,
 * and the "thread" is just a flag that oracle_render_snapshot is
 * called from the existing present path.
 */
static void oracle_start_gpu_threads(void)
{
    /* For now, oracle mode sets a flag. The actual thread launch
     * requires EGL context sharing which is in bfpp_rt_3d_multigpu.c.
     * When multi-GPU is active, those threads call into oracle
     * acquire/extrapolate/render. */
    oracle_gpu_count = 0;

    /* TODO: When EGL multi-GPU is active:
     *   for (int i = 0; i < bfpp_mgpu_gpu_count(); i++) {
     *       oracle_gpu_ctx[i].gpu_index = i;
     *       oracle_gpu_ctx[i].width = ...;
     *       oracle_gpu_ctx[i].height = ...;
     *       oracle_gpu_ctx[i].tape = ...;
     *       oracle_gpu_ctx[i].fb_offset = ...;
     *       pthread_create(&oracle_gpu_threads[i], NULL,
     *                      oracle_gpu_thread, &oracle_gpu_ctx[i]);
     *       oracle_gpu_count++;
     *   }
     */
}

/*
 * oracle_stop_gpu_threads — signal and join all oracle GPU threads.
 */
static void oracle_stop_gpu_threads(void)
{
    /* Threads check oracle.initialized in their loop condition.
     * Setting it to 0 causes them to exit. But we don't want to
     * tear down the whole oracle — just the threads. Use a separate
     * flag if needed. For now, join any running threads. */
    for (int i = 0; i < oracle_gpu_count; i++) {
        pthread_join(oracle_gpu_threads[i], NULL);
    }
    oracle_gpu_count = 0;
}

/* ── Section I: Intrinsic Wrappers ───────────────────────────── */

/* When threading is active, bfpp_err is _Thread_local. Otherwise plain int.
 * Both cases have external linkage. */
extern int bfpp_err;

/*
 * tape_read_u32 — read unsigned 32-bit from tape at addr. Little-endian.
 */
static inline uint32_t tape_read_u32(const uint8_t *tape, int addr)
{
    return (uint32_t)tape[addr]
         | ((uint32_t)tape[addr + 1] << 8)
         | ((uint32_t)tape[addr + 2] << 16)
         | ((uint32_t)tape[addr + 3] << 24);
}

/*
 * tape_q16_to_float — read Q16.16 fixed-point from tape at addr.
 */
static inline float tape_q16_to_float(const uint8_t *tape, int addr)
{
    int32_t q = (int32_t)tape_read_u32(tape, addr);
    return (float)q / 65536.0f;
}

/*
 * bfpp_scene_publish_intrinsic — publish current scene state.
 * Called from generated C via the __scene_publish intrinsic.
 * No tape parameters needed.
 */
void bfpp_scene_publish_intrinsic(uint8_t *tape, int ptr)
{
    (void)tape;
    (void)ptr;
    bfpp_oracle_publish();
}

/*
 * bfpp_scene_mode_intrinsic — enable/disable oracle mode.
 * tape[ptr..ptr+3]: uint32 enable flag (0 = off, nonzero = on).
 *
 * When enabled: starts oracle GPU render threads (if multi-GPU active),
 *   enables temporal extrapolation.
 * When disabled: stops oracle GPU threads, disables extrapolation.
 */
void bfpp_scene_mode_intrinsic(uint8_t *tape, int ptr)
{
    uint32_t enable = tape_read_u32(tape, ptr);

    if (enable) {
        oracle.extrap_enabled = 1;
        oracle_start_gpu_threads();
    } else {
        oracle.extrap_enabled = 0;
        oracle_stop_gpu_threads();
    }
}

/*
 * bfpp_scene_extrap_ms_intrinsic — set max extrapolation lookahead.
 * tape[ptr..ptr+3]: Q16.16 max_ms value.
 */
void bfpp_scene_extrap_ms_intrinsic(uint8_t *tape, int ptr)
{
    float max_ms = tape_q16_to_float(tape, ptr);
    bfpp_oracle_set_extrap_max(max_ms);
}
