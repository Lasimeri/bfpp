#ifndef BFPP_RT_3D_H
#define BFPP_RT_3D_H

/*
 * bfpp_rt_3d.h — Public API for the BF++ 3D rendering subsystem.
 *
 * Architecture:
 *   - OpenGL 3.3 core profile renders to an offscreen FBO.
 *   - glReadPixels bridges GL output into tape[FB_OFFSET] (RGB24),
 *     then calls bfpp_fb_request_flush() to present via the FB pipeline.
 *   - Backend auto-detected: GPU (OpenGL) preferred, software fallback
 *     if GL context creation fails.
 *   - All numeric values from BF++ tape are Q16.16 fixed-point in 32-bit
 *     cells (divide by 65536.0f to get float). Little-endian byte order.
 *   - Resource limits: 16 buffers, 16 VAOs, 16 shaders, 8 programs,
 *     16 textures, 4 shadow-casting lights.
 *
 * Tier 1 functions all take (uint8_t *tape, int ptr) as first two args
 * and read parameters from tape at offsets relative to ptr (4 bytes/cell).
 */

#include <stdint.h>

/* ── Lifecycle ───────────────────────────────────────────────── */

/* Initialize the 3D subsystem. Tries GPU (OpenGL 3.3), falls back
 * to software rasterizer. Attaches to the FB pipeline at tape[fb_offset]. */
void bfpp_3d_init(int width, int height, uint8_t *tape, int fb_offset);

/* Tear down all GL/software state, free tracked resources. */
void bfpp_3d_cleanup(void);

/* Returns 1 if GPU (OpenGL) backend is active, 0 if software. */
int bfpp_3d_is_gpu(void);

/* ── Tier 1: GL Proxies ──────────────────────────────────────── */
/* All read params from tape[ptr + N*4] in Q16.16 or uint32.     */

/* Buffer management */
void bfpp_gl_create_buffer(uint8_t *tape, int ptr);
void bfpp_gl_buffer_data(uint8_t *tape, int ptr);
void bfpp_gl_delete_buffer(uint8_t *tape, int ptr);

/* VAO management */
void bfpp_gl_create_vao(uint8_t *tape, int ptr);
void bfpp_gl_bind_vao(uint8_t *tape, int ptr);
void bfpp_gl_vertex_attrib(uint8_t *tape, int ptr);
void bfpp_gl_delete_vao(uint8_t *tape, int ptr);

/* Shader management */
void bfpp_gl_create_shader(uint8_t *tape, int ptr);
void bfpp_gl_shader_source(uint8_t *tape, int ptr);
void bfpp_gl_compile_shader(uint8_t *tape, int ptr);
void bfpp_gl_create_program(uint8_t *tape, int ptr);
void bfpp_gl_attach_shader(uint8_t *tape, int ptr);
void bfpp_gl_link_program(uint8_t *tape, int ptr);
void bfpp_gl_use_program(uint8_t *tape, int ptr);

/* Uniforms */
void bfpp_gl_uniform_loc(uint8_t *tape, int ptr);
void bfpp_gl_uniform_1f(uint8_t *tape, int ptr);
void bfpp_gl_uniform_3f(uint8_t *tape, int ptr);
void bfpp_gl_uniform_4f(uint8_t *tape, int ptr);
void bfpp_gl_uniform_mat4(uint8_t *tape, int ptr);

/* Drawing */
void bfpp_gl_clear(uint8_t *tape, int ptr);
void bfpp_gl_draw_arrays(uint8_t *tape, int ptr);
void bfpp_gl_draw_elements(uint8_t *tape, int ptr);
void bfpp_gl_viewport(uint8_t *tape, int ptr);
void bfpp_gl_depth_test(uint8_t *tape, int ptr);
void bfpp_gl_present(uint8_t *tape, int ptr);

/* Shadow mapping */
void bfpp_gl_shadow_enable(uint8_t *tape, int ptr);
void bfpp_gl_shadow_disable(uint8_t *tape, int ptr);
void bfpp_gl_shadow_quality(uint8_t *tape, int ptr);

/* ── Tier 2: Fixed-point math ────────────────────────────────── */
/* Defined in bfpp_rt_3d_math.c. All operate on Q16.16 values    */
/* stored on the BF++ tape. Parameters read from tape[ptr+N*4].  */

void bfpp_fp_mul(uint8_t *tape, int ptr);
void bfpp_fp_div(uint8_t *tape, int ptr);
void bfpp_fp_sin(uint8_t *tape, int ptr);
void bfpp_fp_cos(uint8_t *tape, int ptr);
void bfpp_fp_sqrt(uint8_t *tape, int ptr);

void bfpp_mat4_identity(uint8_t *tape, int ptr);
void bfpp_mat4_multiply(uint8_t *tape, int ptr);
void bfpp_mat4_rotate(uint8_t *tape, int ptr);
void bfpp_mat4_translate(uint8_t *tape, int ptr);
void bfpp_mat4_perspective(uint8_t *tape, int ptr);

/* ── Tier 3: Mesh generators ─────────────────────────────────── */
/* Defined in bfpp_rt_3d_meshgen.c. Write vertex data to tape.    */

void bfpp_mesh_cube(uint8_t *tape, int ptr);
void bfpp_mesh_sphere(uint8_t *tape, int ptr);
void bfpp_mesh_torus(uint8_t *tape, int ptr);
void bfpp_mesh_plane(uint8_t *tape, int ptr);
void bfpp_mesh_cylinder(uint8_t *tape, int ptr);

/* ── Software rasterizer ─────────────────────────────────────── */
/* Defined in bfpp_rt_3d_software.c. Fallback when no GL context. */

void bfpp_sw_init(int width, int height, uint8_t *tape, int fb_offset);
void bfpp_sw_cleanup(void);
void bfpp_sw_clear(uint8_t *tape, int ptr);
void bfpp_sw_draw_triangles(uint8_t *tape, int ptr);
void bfpp_sw_present(uint8_t *tape, int ptr);
void bfpp_sw_set_mvp(uint8_t *tape, int ptr);
void bfpp_sw_set_light(uint8_t *tape, int ptr);
void bfpp_sw_set_color(uint8_t *tape, int ptr);

/* ── Multi-GPU + Frame timing ───────────────────────────────── */
/* Defined in bfpp_rt_3d.c (frame_time) and bfpp_rt_3d_multigpu.c */

void bfpp_gl_multi_gpu(uint8_t *tape, int ptr);
void bfpp_gl_gpu_count(uint8_t *tape, int ptr);
void bfpp_gl_frame_time(uint8_t *tape, int ptr);

/* ── Scene Oracle ───────────────────────────────────────────── */
/* Defined in bfpp_rt_3d_oracle.c. Lock-free CPU-decoupled rendering. */

void bfpp_scene_publish_intrinsic(uint8_t *tape, int ptr);
void bfpp_scene_mode_intrinsic(uint8_t *tape, int ptr);
void bfpp_scene_extrap_ms_intrinsic(uint8_t *tape, int ptr);

#endif /* BFPP_RT_3D_H */
