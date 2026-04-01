# Changelog

## [0.4.0] - 2026-03-31

### Added
- **Preprocessor macros**: `!define NAME VALUE` and `!undef NAME` for compile-time text substitution
- **If/else syntax**: `?{true_body}:{false_body}` — destructive truthiness test on current cell
- **Watch mode**: `--watch` CLI flag recompiles automatically on source file change
- **SDL input events**: `__input_poll`, `__input_mouse_pos`, `__input_key_held` — keyboard/mouse input from SDL window
- **Texture intrinsics**: `__gl_create_texture`, `__gl_texture_data`, `__gl_bind_texture`, `__gl_delete_texture`, `__img_load` (BMP loading via SDL2)
- **Self-hosting intrinsics**: `__mul`, `__div`, `__mod`, `__strcmp`, `__strlen`, `__strcpy`, `__call` (indirect subroutine dispatch), `__hashmap_init/get/set`, `__array_insert/remove` — primitives for writing a BF++ compiler in BF++
- **stdlib/math3d.bfpp** (585 lines): pure BF++ 3D math library (vectors, matrices, transforms — no C intrinsics)
- **5 new optimizer passes** (12 total): conditional eval, loop unrolling, move coalescing, tail return elimination, second constant fold round
- **Editor rewrite**: `editor.bfpp` rewritten to 1141 lines with multicore save, line numbers, and text editing

### Changed
- Redundant `TAPE_MASK` operations eliminated in generated C (cleaner output)
- Pure BF++ mesh rendering and TUI implementations added to existing stdlibs

### Fixed
- **Constant folding**: dead store elimination and arithmetic folding corrected
- **clear_fb fix**: no longer corrupts scratch data region

### Tests
- 114 unit tests, all passing (up from 86 in v0.3.0)
- Zero clippy warnings

## [0.3.0] - 2026-03-31

### Added
- **3D Rendering Subsystem** (~45 intrinsics, 6 new runtime files)
  - Tier 1 — GL Proxies: `__gl_init/cleanup`, buffer/VAO/shader/program management, uniform uploads, draw calls, shadow mapping (`__gl_shadow_enable/disable/quality`)
  - Tier 2 — Q16.16 Fixed-Point Math: `__fp_mul/div/sin/cos/sqrt`, `__mat4_identity/multiply/rotate/translate/perspective`
  - Tier 3 — Mesh Generators: `__mesh_cube/sphere/torus/plane/cylinder`
  - OpenGL 3.3 core profile renders to offscreen FBO
  - PBO double-buffered async readback (eliminates 2-15ms glReadPixels stall)
  - Automatic software rasterizer fallback (edge-function, Blinn-Phong, SSE SIMD)
  - Embedded GLSL shaders: Blinn-Phong vertex/fragment with PCF shadow mapping
  - Frame timing intrinsic: `__gl_frame_time`
- **Multi-GPU Rendering** (transparent to BF++ programs)
  - EGL device enumeration (`eglQueryDevicesEXT`) for per-GPU GL contexts
  - SFR (strip-parallel): each GPU renders its horizontal strip via `glScissor`
  - AFR (alternate frame): round-robin with sequence-ordered presentation queue
  - AUTO mode: adaptive selection based on frame time measurement
  - GL command recording + replay across GPU contexts
  - NUMA-aware buffer allocation (`mbind` when `numaif.h` available)
  - Per-GPU thread pinning (desktop: cores 8+, rack: per-NUMA-node)
  - Frame pacing: deadline-based presentation, dropout recovery, SFR strip rebalancing
  - Intrinsics: `__gl_multi_gpu`, `__gl_gpu_count`
- **Scene Oracle** (CPU-decoupled temporal rendering)
  - Lock-free SPSC triple-buffered scene snapshots (acquire/release ordering)
  - CPU publishes scene state at ~1000Hz, GPUs independently sample + extrapolate
  - Temporal extrapolation: Rodrigues rotation + linear velocity, bounded lookahead
  - Confidence-based freeze when data goes stale
  - Intrinsics: `__scene_publish`, `__scene_mode`, `__scene_extrap_ms`
- **New runtime files**:
  - `runtime/bfpp_rt_3d.c/h` — GL proxy layer (1200+ lines)
  - `runtime/bfpp_rt_3d_shaders.h` — GLSL Blinn-Phong + PCF shadows
  - `runtime/bfpp_rt_3d_math.c` — Q16.16 math, sin LUT, 4×4 matrices
  - `runtime/bfpp_rt_3d_meshgen.c` — 5 mesh generators
  - `runtime/bfpp_rt_3d_software.c` — Software rasterizer with SSE
  - `runtime/bfpp_rt_3d_multigpu.c/h` — Multi-GPU (EGL, SFR/AFR, pacing)
  - `runtime/bfpp_rt_3d_oracle.c/h` — Scene Oracle (lock-free triple buffer)
- **stdlib/3d.bfpp** (485 lines): wrapper subroutines for all 3D/multi-GPU/oracle intrinsics
- **examples/3d_demo.bfpp** (169 lines): rotating cube + orbiting sphere, Blinn-Phong lighting, shadows

### Changed
- `bfpp_gl_present()` now uses PBO double-buffered readback (+1 frame latency, eliminates GPU sync stall)
- `bfpp_err` gets external linkage when 3D intrinsics active (runtime needs cross-TU access)
- `_GNU_SOURCE` guarded with `#ifndef` across all runtime files (prevents redefinition warning with SDL2)
- Compiler auto-links `-lGL -lGLEW -lm` for 3D, `-lEGL` for multi-GPU
- Compiler auto-compiles all required runtime .c files based on intrinsic detection

### Tests
- 86 unit tests, all passing
- Zero clippy warnings
- All C runtime files compile clean with `-Wall -Wextra`

## [0.2.0] - 2026-03-31

### Added
- **Numeric literals** (`#N`, `#0xHH`): set current cell to immediate value, respects cell width
- **Direct cell width** (`%1`, `%2`, `%4`, `%8`): set cell width without cycling, backward compatible with bare `%`
- **Block comments** (`/* ... */`): nestable, unterminated = lex error
- **Compiler intrinsics** (`__` prefix): inline C emission for system integration
  - Terminal: `__term_raw`, `__term_restore`, `__term_size`, `__term_alt_on/off`, `__term_mouse_on/off`
  - Time: `__sleep`, `__time_ms`
  - Environment: `__getenv`
  - Process: `__exit`, `__getpid`
  - I/O: `__poll_stdin`
  - TUI runtime: `__tui_init`, `__tui_cleanup`, `__tui_size`, `__tui_begin`, `__tui_end`, `__tui_put`, `__tui_puts`, `__tui_fill`, `__tui_box`, `__tui_key`
- **C runtime library** (`runtime/bfpp_rt.{h,c}`): double-buffered TUI renderer
  - UTF-8 cell storage for Unicode box drawing
  - Raw terminal mode with atexit crash safety
  - Alternate screen buffer
  - Key polling with escape sequence parsing (arrows, pgup/dn, home/end, delete)
  - Three box styles: ASCII, Unicode single-line, Unicode rounded
  - 256-color support
- **stdlib/graphics.bfpp**: SDL2 framebuffer drawing primitives
  - `!#px` set_pixel, `!#gx` get_pixel, `!#gc` clear_framebuffer
  - `!#fl` fill_rect, `!#rc` draw_rect, `!#hl` draw_hline, `!#vl` draw_vline
- **examples/intrinsics_demo.bfpp**: demonstrates __getenv, __getpid, __time_ms, __sleep, __exit
- **examples/tui_demo.bfpp**: rewritten with #N and tui.bfpp stdlib

### Changed
- All stdlib modules rewritten using `#N` and `%N` operators
- `stdlib/file.bfpp`: self-contained open/read/write/close with internal syscall frame setup
- `stdlib/net.bfpp`: full TCP stack — socket/connect/listen/accept/send/recv with sockaddr_in construction
- `stdlib/tui.bfpp`: 10 functions, arbitrary cursor positioning, 256-color, box drawing
- `stdlib/mem.bfpp`: working malloc (bump allocator), memcpy and memset via `*$`/`*~` indirect access
- `stdlib/err.bfpp`: proper panic via `#60` (SYS_exit), error-to-string via print_int
- `stdlib/io.bfpp`: print_int rewritten with stack-based digit extraction, read_int handles multi-digit input

### Fixed
- Preprocessor escaped quote bug (backslash counting)
- Standalone `R`/`K` without `{` now errors instead of silently dropping
- Multi-byte cell width overlap protection (continuation byte sentinel)
- Framebuffer offset dynamic (`BFPP_FB_OFFSET` at tape end, not hardcoded `0xA000`)
- SDL2 init failure no longer segfaults (null pointer guard on renderer)
- `return` in main emits `return 0;` instead of `return;` (C11 UB fix)
- `?` (Propagate) inside `R{}` block breaks to `K{}` instead of returning from subroutine
- Call stack depth enforced (guard in subroutine prologue)
- String literal pointer advance now masked with `TAPE_MASK`
- MultiplyMove factor type changed from u8 to usize (no overflow on >255)
- Duplicate multiply-move offsets merged
- Power-of-2 tape size validation
- Framebuffer bounds validation
- Error code constants used via C `#define`s (no magic numbers in generated C)
- All clippy warnings resolved
- `Cargo.toml` edition fixed from "2024" to "2021"

### Tests
- 86 unit tests, all passing (up from 47 in 0.1.0)
- Zero compiler warnings, zero clippy warnings

## [0.1.0] - 2026-02-15

### Added
- Initial BF++ transpiler: preprocess, lex, parse, analyze, optimize, codegen pipeline
- 30+ operators: core BF, extended memory, stack, subroutines, syscalls, bitwise, error handling, FFI, framebuffer
- 8 stdlib modules: io, math, string, mem, err, file, net, tui
- Peephole optimizer: clear loop, scan loop, multiply-move, error folding
- Integration test suite with bash runner
- Benchmark suite with hyperfine support
- Spec documentation
