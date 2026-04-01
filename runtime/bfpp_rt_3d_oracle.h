#ifndef BFPP_RT_3D_ORACLE_H
#define BFPP_RT_3D_ORACLE_H

/*
 * bfpp_rt_3d_oracle.h — Scene Oracle: CPU-decoupled temporal rendering.
 *
 * Architecture:
 *   CPU simulation thread writes scene state (transforms, velocities,
 *   camera, lights) into a staging snapshot. On publish, the snapshot
 *   is atomically swapped into a lock-free SPSC triple buffer.
 *
 *   GPU render thread(s) acquire the latest snapshot, extrapolate
 *   forward by the time delta since publication, and render the
 *   predicted frame. This decouples render rate from simulation rate.
 *
 *   Triple buffer: 3 x ~30KB snapshots (256 objects). Fits in L2 on
 *   a single core. _Alignas(64) prevents false sharing on EPYC NUMA.
 *
 *   Extrapolation: linear position + Rodrigues angular rotation.
 *   Confidence degrades linearly with extrapolation distance.
 *   Clamped to extrap_max_ms (default 20ms).
 */

#include <stdint.h>
#include <stdatomic.h>

#define ORACLE_MAX_OBJECTS 256

/* ── Per-object scene state ──────────────────────────────────── */

typedef struct {
    float model_mat[16];         /* current 4x4 model matrix (column-major) */
    float velocity[3];           /* world-space linear velocity             */
    float angular_velocity[3];   /* axis-angle per second (mag = rad/s)     */
    float color[3];              /* object color (RGB, 0-1)                 */
    uint32_t vao_id;             /* associated VAO (0 = not set)            */
    int32_t  vertex_count;       /* draw count                              */
    uint32_t draw_mode;          /* GL mode (0=triangles, 1=lines, 2=pts)   */
    int      active;             /* 0 = inactive/deleted                    */
    float    confidence;         /* 1.0 = exact, degrades w/ extrapolation  */
} bfpp_scene_object_t;

/* ── Immutable scene snapshot (published atomically) ─────────── */

typedef struct {
    _Alignas(64)
    bfpp_scene_object_t objects[ORACLE_MAX_OBJECTS];
    int      object_count;

    float    view_mat[16];
    float    proj_mat[16];
    float    view_pos[3];

    /* Lighting state */
    float    light_pos[4][3];
    float    light_color[4][3];
    float    light_intensity[4];
    int      num_lights;

    /* Per-object dirty tracking: 256 objects, 1 bit each (4 x uint64).
       Set by publish when an object's state changed vs the previous slot
       contents. Consumer can use this to skip unchanged objects in shaders
       or upload paths. Cleared at the start of each publish. */
    uint64_t dirty_mask[4];

    uint64_t timestamp_ns;
    uint64_t frame_seq;
} bfpp_scene_snapshot_t;

/* ── Lifecycle ───────────────────────────────────────────────── */

void bfpp_oracle_init(void);
void bfpp_oracle_cleanup(void);

/* ── CPU-side: scene state setters ───────────────────────────── */

void bfpp_oracle_set_object(int obj_id, const float model[16],
                            const float velocity[3],
                            const float angular_vel[3],
                            const float color[3],
                            uint32_t vao_id, int vertex_count,
                            uint32_t draw_mode);

void bfpp_oracle_set_camera(const float view[16], const float proj[16],
                            const float pos[3]);

void bfpp_oracle_set_light(int idx, const float pos[3],
                           const float color[3], float intensity);

/* CPU-side: publish staging snapshot to triple buffer. */
void bfpp_oracle_publish(void);

/* ── GPU-side: snapshot access ───────────────────────────────── */

/* Acquire latest snapshot (non-blocking). Returns NULL if no data. */
const bfpp_scene_snapshot_t *bfpp_oracle_acquire(void);

/* Release the acquired snapshot. */
void bfpp_oracle_release(void);

/* ── Temporal extrapolation ──────────────────────────────────── */

/* Extrapolate snapshot forward by dt_ms milliseconds (in-place). */
void bfpp_oracle_extrapolate(bfpp_scene_snapshot_t *snap, float dt_ms);

void  bfpp_oracle_set_extrap_max(float max_ms);
float bfpp_oracle_get_extrap_max(void);
int   bfpp_oracle_get_stale_frames(void);

/* ── Intrinsic wrappers (called from generated C) ────────────── */

void bfpp_scene_publish_intrinsic(uint8_t *tape, int ptr);
void bfpp_scene_mode_intrinsic(uint8_t *tape, int ptr);
void bfpp_scene_extrap_ms_intrinsic(uint8_t *tape, int ptr);

#endif /* BFPP_RT_3D_ORACLE_H */
