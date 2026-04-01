# BF++ Language Specification

**Version**: 0.5.0
**Status**: Draft
**Date**: 2026-04-01

---

## 1. Overview

BF++ is a Brainfuck-derived language that retains symbolic minimalism while adding operators for system calls, file I/O, networking, error handling, and subroutines. Programs are transpiled to C via a Rust-based compiler with parallel codegen and analysis, then compiled to native binaries via gcc/clang with parallel CC invocation. Features include 3D rendering (OpenGL 3.3 + software fallback), multi-GPU support, OpenCL GPU compute offloading, AVX2 SIMD acceleration, a terminal framebuffer backend for headless/SSH rendering, and a self-hosting bootstrap compiler.

BF++ is a strict superset of Brainfuck: all valid BF programs are valid BF++ programs with identical semantics.

---

## 2. Lexical Structure

### 2.1 Source Encoding

BF++ source files use UTF-8 encoding. Only ASCII characters are semantically significant. Non-operator characters outside string literals and comments are ignored (whitespace, letters not assigned as operators, etc.).

### 2.2 Comments

`;` begins a line comment. All characters from `;` to end-of-line are ignored.

```
+++ ; this is a comment
```

### 2.2b Block Comments

`/* ... */` encloses a block comment. Block comments may span multiple lines and support nesting.

```
+ /* this is a block comment */ -
+ /* outer /* nested */ still comment */ -
```

Unterminated block comments are a compile-time error.

### 2.3 String Literals

`"..."` encloses a string literal. Standard C escape sequences are supported:

| Escape | Meaning |
|--------|---------|
| `\0`   | Null byte (0x00) |
| `\n`   | Newline (0x0A) |
| `\r`   | Carriage return (0x0D) |
| `\t`   | Tab (0x09) |
| `\\`   | Literal backslash |
| `\"`   | Literal double quote |
| `\xHH` | Hex byte value |

String literals write their ASCII bytes sequentially to the tape starting at the current pointer, advancing the pointer past the last byte written.

### 2.4 Preprocessor Macros

`!define NAME VALUE` defines a text substitution macro. All subsequent occurrences of `NAME` in the source are replaced with `VALUE` before parsing. `!undef NAME` removes the macro definition.

```
!define SCREEN_W 320
!define SCREEN_H 200
#SCREEN_W > #SCREEN_H <   ; expands to: #320 > #200 <

!undef SCREEN_W            ; SCREEN_W is no longer substituted
```

**Semantics**:
- Macros are processed in a single left-to-right pass before operator parsing.
- `VALUE` is the remainder of the line after `NAME` (trimmed).
- Macro names must not conflict with operator characters or intrinsic names.
- `!define` with no value defines the name as empty (useful for conditional guards).
- Redefinition of an existing name silently replaces the previous value.

---

## 3. Operators

### 3.1 Core Operators (BF-Compatible)

| Op | Name | Semantics |
|----|------|-----------|
| `>` | Move right | `ptr += 1` |
| `<` | Move left | `ptr -= 1` |
| `+` | Increment | `tape[ptr] += 1` (wraps on overflow) |
| `-` | Decrement | `tape[ptr] -= 1` (wraps on underflow) |
| `.` | Output | Write `tape[ptr]` as byte to stdout (fd 1) |
| `,` | Input | Read one byte from stdin (fd 0) into `tape[ptr]`. EOF sets cell to 0. |
| `[` | Loop start | If `tape[ptr] == 0`, jump to instruction after matching `]` |
| `]` | Loop end | If `tape[ptr] != 0`, jump back to matching `[` |

Brackets must be balanced. Unmatched brackets are a compile-time error.

### 3.2 Memory & Data Operators

| Op | Name | Semantics |
|----|------|-----------|
| `@` | Absolute address | `ptr = tape[ptr]` — set pointer to the value stored in the current cell |
| `*` | Dereference | Subsequent operation targets `tape[tape[ptr]]` instead of `tape[ptr]`. Modifier applies to the next single operator only. |
| `%` | Cell width cycle | Cycle cell width at current position: 8 → 16 → 32 → 64 → 8 bits. Affects how the cell at `ptr` is interpreted for arithmetic and I/O. |
| `%N` | Direct cell width | Set cell width at current position directly. N must be 1, 2, 4, or 8. See below. |
| `#N` | Numeric literal | Set current cell to immediate value N. Supports decimal and `#0xHH` hex. See below. |
| `"..."` | String literal | Write ASCII bytes to tape starting at `ptr`, advance `ptr` past last written byte. See Section 2.3. |

**Dereference (`*`) details**: `*` is a prefix modifier. `*+` increments `tape[tape[ptr]]`. `*.` outputs `tape[tape[ptr]]`. `*,` reads into `tape[tape[ptr]]`. The modifier is consumed after one operation.

**Cell width (`%`) details**: Cell width is tracked per-cell in a separate metadata array. Multi-byte cells occupy consecutive tape positions (little-endian). A 16-bit cell at position N uses bytes N and N+1. A 32-bit cell uses N..N+3. A 64-bit cell uses N..N+7.

**Direct cell width (`%N`) details**: `%1`, `%2`, `%4`, `%8` set the cell width at the current position to the specified byte count directly, without cycling through intermediate widths. Before setting the new width, old continuation bytes from the previous width are released. If any sub-cell required by the new width is already in use (continuation byte of another cell or an independent wide cell), the operation reverts to width 1 and sets error register to 6 (`ERR_INVALID_ARG`). `%N` is preferred over `%` when the target width is known at write time.

**Numeric literal (`#N`) details**: `#N` sets the current cell to the immediate value N. Supports decimal (`#72`, `#40960`) and hexadecimal (`#0xFF`, `#0x9000`). The value is written respecting the current cell width — writing a value larger than the cell width's range is truncated. `#N` replaces the previous cell value entirely (equivalent to `bfpp_set(ptr, N)`). This operator eliminates the need for long increment chains (`+++...+++`) to set cells to known values.

### 3.3 Stack & Subroutine Operators

| Op | Name | Semantics |
|----|------|-----------|
| `$` | Push | Push `tape[ptr]` onto the data stack. Stack grows upward. |
| `~` | Pop | Pop top of data stack into `tape[ptr]`. Underflow sets error register to 6 (invalid argument). |
| `!name{...}` | Define subroutine | Store the block `{...}` under name `name`. Not executed at definition. |
| `!name` | Call subroutine | Push return context (return address + error state) onto call stack, jump to subroutine body. |
| `^` | Return | Pop return context from call stack, resume execution after the call site. If error register is non-zero, error propagates to caller. |

**Subroutine naming**: Names begin with `#` followed by one or more characters from: `> < + - . , [ ] @ * % $ ~ \ | & x s r n E e ? R K ^` and alphanumeric characters `a-z A-Z 0-9`. The `#` prefix disambiguates subroutine references from other operators.

Examples: `!#>{...}`, `!#rd{...}`, `!#tcp{...}`, `!#.+{...}`

**Call stack**: Separate from data stack. Stores return addresses and saved error register state. Default depth: 256 frames. Overflow is a fatal runtime error.

**Recursion**: Fully supported. Each call pushes a new frame.

### 3.4 System Interface Operators

| Op | Name | Semantics |
|----|------|-----------|
| `\` | Syscall | Execute a system call. `tape[ptr]` = syscall number, `tape[ptr+1..ptr+6]` = arguments (up to 6, 64-bit each). Result written to `tape[ptr]`. On failure, error register set via errno mapping (see ERROR_CODES.md). |
| `.{N}` | Write to fd | Write `tape[ptr]` as byte to file descriptor N. N is a decimal literal or `*` for `tape[ptr+1]`. |
| `,{N}` | Read from fd | Read one byte from file descriptor N into `tape[ptr]`. N is a decimal literal or `*` for `tape[ptr+1]`. |

**Syscall argument layout** (when in 64-bit cell mode):

| Offset | Content |
|--------|---------|
| `ptr+0` | Syscall number |
| `ptr+1` | Arg 1 |
| `ptr+2` | Arg 2 |
| `ptr+3` | Arg 3 |
| `ptr+4` | Arg 4 |
| `ptr+5` | Arg 5 |
| `ptr+6` | Arg 6 |

After execution, `tape[ptr]` contains the return value. Error register is set on failure.

**Note**: Syscall numbers are platform-dependent. See `bfpp_platform.h` for the mapping layer. Standard library subroutines abstract over this.

### 3.5 Bitwise & Arithmetic Operators

| Op | Name | Semantics |
|----|------|-----------|
| `\|` | Bitwise OR | `tape[ptr] \|= tape[ptr+1]` |
| `&` | Bitwise AND | `tape[ptr] &= tape[ptr+1]` |
| `x` | Bitwise XOR | `tape[ptr] ^= tape[ptr+1]` |
| `s` | Shift left | `tape[ptr] <<= tape[ptr+1]` |
| `r` | Shift right | `tape[ptr] >>= tape[ptr+1]` (logical shift, zero-fill) |
| `n` | Bitwise NOT | `tape[ptr] = ~tape[ptr]` |

All bitwise operations respect the current cell width at `ptr`.

### 3.6 Error Handling Operators

| Op | Name | Semantics |
|----|------|-----------|
| `E` | Error read | `tape[ptr] = bfpp_err` — copy error register into current cell |
| `e` | Error write | `bfpp_err = tape[ptr]` — set error register from current cell |
| `?` | Propagate | If `bfpp_err != 0`, immediately return from current subroutine (equivalent to `if (err) return`). No-op at top level. |
| `R{...}` | Result block | Execute block. If error register becomes non-zero during execution, jump to matching `K{...}`. |
| `K{...}` | Catch block | Executes only if preceding `R{...}` produced an error. Error code available via `E`. Must immediately follow a `R{...}` block. |

**Propagation semantics**: `?` checks the error register. If non-zero, the current subroutine returns immediately. The error register value is preserved, allowing the caller to inspect or further propagate it. This is directly analogous to Rust's `?` operator on `Result<T, E>`.

**Result/Catch semantics**: `R{...}K{...}` provides local error handling. The error register is saved before the R block. If the R block sets a non-zero error, execution jumps to the K block with the error code preserved. After the K block (or after the R block if no error), the error register is reset to 0 unless explicitly re-set.

**Nesting**: `R{...}K{...}` blocks can nest. Each level maintains its own error context.

### 3.7 Control Flow

| Op | Name | Semantics |
|----|------|-----------|
| `;` | Comment | Ignore all characters until end of line |
| `?{...}:{...}` | If/Else | Test `tape[ptr]` for truthiness. If non-zero, execute the first block; if zero, execute the second block. **Destructive**: consumes (zeroes) the tested cell. |

Standard BF loops (`[`/`]`) remain the primary control flow mechanism. The `?{...}:{...}` if/else construct provides branching without the BF flag-cell boilerplate.

**If/Else semantics**: `?{true_body}:{false_body}` reads `tape[ptr]`. If non-zero, the true block executes; otherwise the false block executes. The tested cell is consumed (set to zero) regardless of which branch is taken. This is destructive — save the value with `$` before the test if it's needed later.

```bfpp
; If cell != 0, print 'Y'; else print 'Z'
$                         ; save value (destructive test)
?{ #89 . }:{ #90 . }     ; 'Y' if true, 'Z' if false
```

### 3.8 Compiler Intrinsics

Compiler intrinsics are subroutine calls whose names start with `__` (double underscore). Instead of dispatching to a BF++ subroutine body, the compiler replaces the call with inline C code. This bridges the gap between BF++ operators and C-level system APIs that cannot be expressed in pure BF++.

Intrinsics are invoked with standard subroutine call syntax: `!#__name`.

#### 3.8.1 Terminal Control

| Intrinsic | Args | Effect |
|-----------|------|--------|
| `__term_raw` | — | Enter raw terminal mode (disable echo, canonical mode, signals). Sets `bfpp_err` on failure. |
| `__term_restore` | — | Restore terminal to saved state (before raw mode). No-op if not in raw mode. |
| `__term_size` | — | `tape[ptr]` = columns, `tape[ptr+1]` = rows. Sets `bfpp_err` on ioctl failure. |
| `__term_alt_on` | — | Enter alternate screen buffer (`ESC[?1049h`). |
| `__term_alt_off` | — | Exit alternate screen buffer (`ESC[?1049l`). |
| `__term_mouse_on` | — | Enable mouse tracking (`ESC[?1000h`, `ESC[?1006h`). |
| `__term_mouse_off` | — | Disable mouse tracking. |

Terminal intrinsics emit `#include <termios.h>` and `#include <sys/ioctl.h>`. The initial terminal state is captured in a constructor that runs before `main()`, so `__term_restore` always has a known-good state to revert to.

#### 3.8.2 Time

| Intrinsic | Args | Effect |
|-----------|------|--------|
| `__sleep` | `tape[ptr]` = milliseconds | Pause execution for the specified duration. |
| `__time_ms` | — | `tape[ptr]` = monotonic timestamp in milliseconds (`CLOCK_MONOTONIC`). |

#### 3.8.3 Environment

| Intrinsic | Args | Effect |
|-----------|------|--------|
| `__getenv` | `ptr` -> null-terminated var name | Reads the environment variable. Value overwrites the name at `ptr`. Sets `bfpp_err = 2` if not found. |

#### 3.8.4 Process

| Intrinsic | Args | Effect |
|-----------|------|--------|
| `__exit` | `tape[ptr]` = exit code | Terminate the process immediately with the given exit code. |
| `__getpid` | — | `tape[ptr]` = current process ID. |

#### 3.8.5 Non-blocking I/O

| Intrinsic | Args | Effect |
|-----------|------|--------|
| `__poll_stdin` | `tape[ptr]` = timeout in ms | `tape[ptr]` = 1 if data ready on stdin, 0 if timeout. |

#### 3.8.6 TUI Runtime

The TUI intrinsics require the C runtime library (`bfpp_rt.h`). When any `__tui_*` intrinsic is used, the compiler emits `#include "bfpp_rt.h"` and sets the `uses_tui_runtime` flag, which tells the build driver to compile and link the TUI runtime.

| Intrinsic | Args | Effect |
|-----------|------|--------|
| `__tui_init` | — | Initialize TUI: save termios, enter raw mode, alternate screen, hide cursor. Registers atexit cleanup. |
| `__tui_cleanup` | — | Restore terminal: show cursor, exit alternate screen, restore termios. |
| `__tui_size` | — | `tape[ptr]` = columns, `tape[ptr+1]` = rows. |
| `__tui_begin` | — | Begin a frame (clear back buffer for double-buffered rendering). |
| `__tui_end` | — | End frame: diff back buffer against front buffer, emit minimal ANSI updates. |
| `__tui_put` | `tape[ptr]`=row, `[ptr+1]`=col, `[ptr+2]`=char, `[ptr+3]`=fg, `[ptr+4]`=bg | Place a single character in the back buffer. |
| `__tui_puts` | `tape[ptr]`=row, `[ptr+1]`=col, null-terminated string at ptr+2, fg after null, bg after fg | Place a string in the back buffer. |
| `__tui_fill` | `tape[ptr]`=row, `[ptr+1]`=col, `[ptr+2]`=w, `[ptr+3]`=h, `[ptr+4]`=char, `[ptr+5]`=fg, `[ptr+6]`=bg | Fill a rectangular region in the back buffer. |
| `__tui_box` | `tape[ptr]`=row, `[ptr+1]`=col, `[ptr+2]`=w, `[ptr+3]`=h, `[ptr+4]`=style | Draw a box with Unicode box-drawing characters. |
| `__tui_key` | `tape[ptr]` = timeout in ms | `tape[ptr]` = keycode (-1 on timeout). Handles escape sequences for arrow/special keys. |

**Color values**: -1 = default terminal color, 0-7 = standard colors, 8-15 = bright colors, 16-231 = 216-color RGB cube, 232-255 = grayscale ramp.

**Special key constants** (returned by `__tui_key`):

| Code | Key |
|------|-----|
| 1000 | Up |
| 1001 | Down |
| 1002 | Right |
| 1003 | Left |
| 1004 | Home |
| 1005 | End |
| 1006 | Page Up |
| 1007 | Page Down |
| 1008 | Delete |
| 127 | Backspace |
| 13 | Enter |
| 9 | Tab |
| 27 | Escape |

#### 3.8.7 3D Rendering

The 3D rendering subsystem is a three-tier intrinsic architecture for hardware-accelerated (OpenGL 3.3) or software-fallback rendering. All 3D intrinsics take `(uint8_t *tape, int ptr)` internally — parameters are read from `tape[ptr + N*4]`. Numeric values use **Q16.16 fixed-point** format: `65536` = `1.0`, `32768` = `0.5`, `-65536` = `-1.0`.

When any `__gl_*`, `__fp_*`, `__mesh_*`, or `__scene_*` intrinsic is used, the compiler emits includes for the 3D runtime headers and sets the `uses_3d_runtime` flag, which tells the build driver to compile and link the 3D runtime libraries (`bfpp_rt_3d.c`, `bfpp_rt_3d_math.c`, `bfpp_rt_3d_meshgen.c`, `bfpp_rt_3d_software.c`, `bfpp_rt_3d_multigpu.c`, `bfpp_rt_3d_oracle.c`).

**Tier 1 — GL Proxies** (OpenGL 3.3 core, software fallback):

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__gl_init` | `[ptr]`=width, `[ptr+4]`=height | — | Initialize GL context (or software rasterizer). |
| `__gl_cleanup` | — | — | Destroy GL context, free resources. |
| `__gl_create_buffer` | — | `tape[ptr]` = buffer ID | Create a vertex/index buffer object. |
| `__gl_buffer_data` | `[ptr]`=buffer_id, `[ptr+4]`=data_ptr (tape offset), `[ptr+8]`=size_bytes | — | Upload data to buffer. |
| `__gl_delete_buffer` | `[ptr]`=buffer_id | — | Delete buffer object. |
| `__gl_create_vao` | — | `tape[ptr]` = VAO ID | Create vertex array object. |
| `__gl_bind_vao` | `[ptr]`=vao_id | — | Bind VAO. |
| `__gl_vertex_attrib` | `[ptr]`=index, `[ptr+4]`=size, `[ptr+8]`=stride, `[ptr+12]`=offset | — | Configure vertex attribute pointer. |
| `__gl_delete_vao` | `[ptr]`=vao_id | — | Delete VAO. |
| `__gl_create_shader` | `[ptr]`=type (0=vertex, 1=fragment) | `tape[ptr]` = shader ID | Create shader object. |
| `__gl_shader_source` | `[ptr]`=shader_id, `[ptr+4]`=source_ptr (tape offset) | — | Set shader source (null-terminated at tape offset). |
| `__gl_compile_shader` | `[ptr]`=shader_id | `tape[ptr]` = 1 on success, 0 on failure | Compile shader. |
| `__gl_create_program` | — | `tape[ptr]` = program ID | Create shader program. |
| `__gl_attach_shader` | `[ptr]`=program_id, `[ptr+4]`=shader_id | — | Attach shader to program. |
| `__gl_link_program` | `[ptr]`=program_id | `tape[ptr]` = 1 on success, 0 on failure | Link shader program. |
| `__gl_use_program` | `[ptr]`=program_id | — | Bind shader program for rendering. |
| `__gl_uniform_loc` | `[ptr]`=program_id, `[ptr+4]`=name_ptr (tape offset) | `tape[ptr]` = uniform location | Query uniform location by name. |
| `__gl_uniform_1f` | `[ptr]`=location, `[ptr+4]`=value (Q16.16) | — | Set float uniform. |
| `__gl_uniform_3f` | `[ptr]`=location, `[ptr+4]`=x, `[ptr+8]`=y, `[ptr+12]`=z (Q16.16) | — | Set vec3 uniform. |
| `__gl_uniform_4f` | `[ptr]`=location, `[ptr+4..+16]`=x,y,z,w (Q16.16) | — | Set vec4 uniform. |
| `__gl_uniform_mat4` | `[ptr]`=location, `[ptr+4..+67]`=16 floats (Q16.16) | — | Set mat4 uniform (column-major). |
| `__gl_clear` | `[ptr]`=r, `[ptr+4]`=g, `[ptr+8]`=b (Q16.16, 0–65536) | — | Clear color and depth buffers. |
| `__gl_draw_arrays` | `[ptr]`=mode, `[ptr+4]`=first, `[ptr+8]`=count | — | Draw primitives from arrays. |
| `__gl_draw_elements` | `[ptr]`=mode, `[ptr+4]`=count, `[ptr+8]`=index_offset | — | Draw indexed primitives. |
| `__gl_viewport` | `[ptr]`=x, `[ptr+4]`=y, `[ptr+8]`=w, `[ptr+12]`=h | — | Set viewport rectangle. |
| `__gl_depth_test` | `[ptr]`=enable (1=on, 0=off) | — | Enable/disable depth testing. |
| `__gl_present` | — | — | Swap buffers / present frame. |
| `__gl_shadow_enable` | — | — | Enable shadow mapping. |
| `__gl_shadow_disable` | — | — | Disable shadow mapping. |
| `__gl_shadow_quality` | `[ptr]`=quality (shadow map resolution) | — | Set shadow map quality. |

**Tier 2 — Fixed-Point Math** (Q16.16):

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__fp_mul` | `[ptr]`=a, `[ptr+4]`=b (Q16.16) | `tape[ptr]` = a*b (Q16.16) | Fixed-point multiply. |
| `__fp_div` | `[ptr]`=a, `[ptr+4]`=b (Q16.16) | `tape[ptr]` = a/b (Q16.16) | Fixed-point divide. |
| `__fp_sin` | `[ptr]`=angle (Q16.16 radians) | `tape[ptr]` = sin(angle) (Q16.16) | Sine via LUT. |
| `__fp_cos` | `[ptr]`=angle (Q16.16 radians) | `tape[ptr]` = cos(angle) (Q16.16) | Cosine via LUT. |
| `__fp_sqrt` | `[ptr]`=value (Q16.16) | `tape[ptr]` = sqrt(value) (Q16.16) | Square root. |
| `__mat4_identity` | `[ptr]`=dest_ptr (tape offset) | 16 Q16.16 values written at dest | Write 4x4 identity matrix. |
| `__mat4_multiply` | `[ptr]`=a_ptr, `[ptr+4]`=b_ptr, `[ptr+8]`=dest_ptr | 16 Q16.16 values at dest | Matrix multiply A * B. |
| `__mat4_rotate` | `[ptr]`=src_ptr, `[ptr+4]`=angle, `[ptr+8]`=ax, `[ptr+12]`=ay, `[ptr+16]`=az, `[ptr+20]`=dest_ptr | Rotated matrix at dest | Rotate matrix by angle around axis. |
| `__mat4_translate` | `[ptr]`=src_ptr, `[ptr+4]`=tx, `[ptr+8]`=ty, `[ptr+12]`=tz, `[ptr+16]`=dest_ptr | Translated matrix at dest | Apply translation to matrix. |
| `__mat4_perspective` | `[ptr]`=fov, `[ptr+4]`=aspect, `[ptr+8]`=near, `[ptr+12]`=far, `[ptr+16]`=dest_ptr | Perspective matrix at dest | Build perspective projection matrix. All params Q16.16. |

**Tier 3 — Mesh Generators**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__mesh_cube` | `[ptr]`=dest_ptr (tape offset), `[ptr+4]`=size (Q16.16) | Vertex data at dest, `tape[ptr]` = vertex count | Generate unit cube mesh. |
| `__mesh_sphere` | `[ptr]`=dest_ptr, `[ptr+4]`=radius, `[ptr+8]`=slices, `[ptr+12]`=stacks | Vertex data at dest, `tape[ptr]` = vertex count | Generate UV sphere mesh. |
| `__mesh_torus` | `[ptr]`=dest_ptr, `[ptr+4]`=major_r, `[ptr+8]`=minor_r, `[ptr+12]`=slices, `[ptr+16]`=stacks | Vertex data at dest, `tape[ptr]` = vertex count | Generate torus mesh. |
| `__mesh_plane` | `[ptr]`=dest_ptr, `[ptr+4]`=width, `[ptr+8]`=depth | Vertex data at dest, `tape[ptr]` = vertex count | Generate plane (quad). |
| `__mesh_cylinder` | `[ptr]`=dest_ptr, `[ptr+4]`=radius, `[ptr+8]`=height, `[ptr+12]`=slices | Vertex data at dest, `tape[ptr]` = vertex count | Generate cylinder mesh. |

**Multi-GPU**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__gl_multi_gpu` | `[ptr]`=mode (0=disabled, 1=SFR, 2=AFR) | — | Enable multi-GPU rendering mode. SFR = split-frame rendering, AFR = alternate frame rendering. |
| `__gl_gpu_count` | — | `tape[ptr]` = number of GPUs | Query available GPU count. |
| `__gl_frame_time` | — | `tape[ptr]` = frame time in microseconds | Query last frame render time. |

**Scene Oracle** (lock-free triple-buffered scene publishing for decoupled simulation/render):

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__scene_publish` | — | — | Publish current scene state (triple-buffer swap). |
| `__scene_mode` | `[ptr]`=mode (0=normal, 1=extrapolation) | — | Set scene oracle mode. |
| `__scene_extrap_ms` | `[ptr]`=milliseconds | — | Set extrapolation lookahead time. |

**Runtime files**: `bfpp_rt_3d.c/h` (GL proxy layer), `bfpp_rt_3d_math.c` (Q16.16 math with sin LUT), `bfpp_rt_3d_meshgen.c` (mesh generators), `bfpp_rt_3d_software.c` (SSE software rasterizer with Blinn-Phong), `bfpp_rt_3d_shaders.h` (GLSL shaders), `bfpp_rt_3d_multigpu.c/h` (multi-GPU via EGL, SFR/AFR), `bfpp_rt_3d_oracle.c/h` (Scene Oracle with lock-free triple buffer).

#### 3.8.8 SDL Input

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__input_poll` | — | `tape[ptr]`=type (0-5), `tape[ptr+4]`=key, `tape[ptr+8]`=x, `tape[ptr+12]`=y | Poll next SDL event from queue. Type: 0=none, 1=key_down, 2=key_up, 3=mouse_move, 4=mouse_down, 5=mouse_up. |
| `__input_mouse_pos` | — | `tape[ptr]`=x, `tape[ptr+4]`=y | Get cached mouse position (last known from event polling). |
| `__input_key_held` | `[ptr]`=scancode | `tape[ptr]`=0/1 | Check if key is currently held (from SDL keyboard state). Returns 1 if held, 0 if not. |

#### 3.8.9 Textures

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__gl_create_texture` | — | `tape[ptr]`=texture_id | Generate a GL texture object. Returns texture ID. |
| `__gl_texture_data` | `[ptr]`=tex_id, `[ptr+4]`=width, `[ptr+8]`=height, `[ptr+12]`=format (0=RGB, 1=RGBA), `[ptr+16]`=data_addr (tape offset) | — | Upload pixel data from tape to texture. |
| `__gl_bind_texture` | `[ptr]`=unit, `[ptr+4]`=tex_id | — | Bind texture to texture unit. |
| `__gl_delete_texture` | `[ptr]`=tex_id | — | Delete texture object. |
| `__img_load` | `[ptr]`=path_addr (tape offset to null-terminated path), `[ptr+4]`=dest_addr (tape offset) | `tape[ptr+8]`=width, `tape[ptr+12]`=height, `tape[ptr+16]`=channels | Load BMP image from disk into tape memory at dest_addr. |

#### 3.8.10 Self-Hosting Intrinsics

Intrinsics for arithmetic, string operations, indirect calls, and data structures — designed to support BF++ self-hosting (compiler-in-BF++).

**Arithmetic**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__mul` | `tape[ptr]`=a, `tape[ptr+1]`=b | `tape[ptr]` = a * b | Integer multiply. |
| `__div` | `tape[ptr]`=a, `tape[ptr+1]`=b | `tape[ptr]` = a / b, `tape[ptr+1]` = remainder | Integer divide with remainder. |
| `__mod` | `tape[ptr]`=a, `tape[ptr+1]`=b | `tape[ptr]` = a % b | Integer modulo. |

**String Operations**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__strcmp` | `tape[ptr]`=addr_a, `tape[ptr+1]`=addr_b (tape offsets to null-terminated strings) | `tape[ptr]` = -1/0/1 | Compare strings lexicographically. Returns 0 if equal, -1 if a<b, 1 if a>b. |
| `__strlen` | `tape[ptr]`=addr (tape offset to null-terminated string) | `tape[ptr]` = length | Length of null-terminated string (excluding null). |
| `__strcpy` | `tape[ptr]`=dest_addr, `tape[ptr+1]`=src_addr | — | Copy null-terminated string from src to dest (including null terminator). |

**Indirect Calls**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__call` | `tape[ptr]`=subroutine_index | — | Indirect subroutine call via `bfpp_sub_table[tape[ptr]]()`. Enables computed dispatch for switch-like constructs. |

**Hash Map**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__hashmap_init` | `tape[ptr]`=map_addr, `tape[ptr+1]`=capacity | — | Initialize hash map at tape address with given capacity. |
| `__hashmap_get` | `tape[ptr]`=map_addr, `tape[ptr+1]`=key_addr (tape offset to null-terminated key) | `tape[ptr]`=value, `tape[ptr+1]`=found (0/1) | Look up key. If found, value is returned and found=1. If not found, found=0. |
| `__hashmap_set` | `tape[ptr]`=map_addr, `tape[ptr+1]`=key_addr, `tape[ptr+2]`=value | — | Insert or update key-value pair. |

**Array Operations**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__array_insert` | `tape[ptr]`=array_addr, `tape[ptr+1]`=index, `tape[ptr+2]`=elem_size, `tape[ptr+3]`=count, `tape[ptr+4]`=value_addr | — | Insert element at index. Shifts subsequent elements right. |
| `__array_remove` | `tape[ptr]`=array_addr, `tape[ptr+1]`=index, `tape[ptr+2]`=elem_size, `tape[ptr+3]`=count | — | Remove element at index. Shifts subsequent elements left. |

#### 3.8.11 GPU Compute (OpenCL)

GPU compute intrinsics offload parallel operations to OpenCL-capable GPUs. The runtime (`bfpp_rt_opencl.{c,h}`) loads `libOpenCL.so` via `dlopen` at init. Programs degrade gracefully on systems without GPU compute.

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__gpu_init` | — | — | Initialize OpenCL context, select best GPU, create command queue. |
| `__gpu_count` | — | `tape[ptr]` = GPU count | Query number of OpenCL-capable devices. |
| `__gpu_memset` | `[ptr]`=addr, `[ptr+4]`=value, `[ptr+8]`=count | — | Fill GPU-side buffer with value. |
| `__gpu_memcpy` | `[ptr]`=dest, `[ptr+4]`=src, `[ptr+8]`=count | — | Copy between tape memory and GPU memory. |
| `__gpu_sort` | `[ptr]`=addr, `[ptr+4]`=count | Sorted data at addr | GPU-accelerated parallel sort (bitonic sort kernel). |
| `__gpu_reduce` | `[ptr]`=addr, `[ptr+4]`=count, `[ptr+8]`=op (0=sum, 1=min, 2=max) | `tape[ptr]` = result | Parallel reduction across array. |
| `__gpu_transform` | `[ptr]`=addr, `[ptr+4]`=count, `[ptr+8]`=op | Transformed data at addr | Per-element transform kernel. |
| `__gpu_rasterize` | Rasterization params from tape | — | GPU-accelerated triangle rasterization. |
| `__gpu_blur` | `[ptr]`=addr, `[ptr+4]`=w, `[ptr+8]`=h, `[ptr+12]`=radius | Blurred image at addr | GPU box blur kernel. |
| `__gpu_poll` | — | `tape[ptr]` = 0/1 | Check if last async operation completed. |
| `__gpu_wait` | — | — | Block until all pending GPU operations complete. |
| `__gpu_dispatch` | `[ptr]`=kernel_id, params from tape | — | Dispatch a custom OpenCL kernel by ID. |

**Runtime files**: `bfpp_rt_opencl.c/h` (OpenCL context management, kernel dispatch, memory transfer), `bfpp_rt_opencl_kernels.h` (embedded OpenCL kernel source strings).

#### 3.8.12 Terminal Framebuffer Backend

When `--framebuffer WxH` is active and no display server is detected (or `BFPP_TERMINAL_FB=1` is set), the runtime uses a terminal-based framebuffer backend (`bfpp_fb_terminal.{c,h}`) instead of SDL2.

**Features**:
- True-color ANSI rendering (24-bit `ESC[38;2;r;g;bm` sequences)
- Delta encoding: only changed pixels are re-emitted between frames
- Half-block characters (`▀` / `▄`) for 2x vertical resolution
- Adaptive frame rate targeting configurable bandwidth (`BFPP_TERMINAL_BW`, default 256 KB/s)
- No code changes needed — the `F` flush operator works identically on both backends

**Environment variables**:
| Variable | Default | Effect |
|----------|---------|--------|
| `BFPP_TERMINAL_FB` | auto-detect | Force terminal backend when set to `1` |
| `BFPP_TERMINAL_BW` | 256 | Target bandwidth in KB/s |

**Runtime files**: `bfpp_fb_terminal.c/h`.

### 3.9 C Runtime Library (`bfpp_rt.h`)

The C runtime library provides a double-buffered TUI subsystem for programs that need terminal UI beyond what ANSI escape sequences in BF++ can efficiently provide. It is automatically linked when any `__tui_*` intrinsic is used.

**Architecture**:
- Double-buffered cell grid: back buffer is written to via draw primitives, `end_frame` diffs against the front buffer and emits only changed cells as ANSI escape sequences.
- Raw terminal mode with atexit cleanup for crash safety.
- Alternate screen buffer for clean terminal restore on exit.
- Input polling with configurable timeout and escape sequence decoding for special keys (arrows, home/end, page up/down, delete).

**API** (C functions, called indirectly via `__tui_*` intrinsics):

| Function | Signature |
|----------|-----------|
| `bfpp_tui_init` | `void bfpp_tui_init(void)` |
| `bfpp_tui_cleanup` | `void bfpp_tui_cleanup(void)` |
| `bfpp_tui_get_size` | `void bfpp_tui_get_size(int *cols, int *rows)` |
| `bfpp_tui_begin_frame` | `void bfpp_tui_begin_frame(void)` |
| `bfpp_tui_end_frame` | `void bfpp_tui_end_frame(void)` |
| `bfpp_tui_put` | `void bfpp_tui_put(int row, int col, uint8_t ch, int fg, int bg)` |
| `bfpp_tui_puts` | `void bfpp_tui_puts(int row, int col, const char *str, int fg, int bg)` |
| `bfpp_tui_fill` | `void bfpp_tui_fill(int row, int col, int w, int h, uint8_t ch, int fg, int bg)` |
| `bfpp_tui_box` | `void bfpp_tui_box(int row, int col, int w, int h, int style)` |
| `bfpp_tui_poll_key` | `int bfpp_tui_poll_key(int timeout_ms)` |

The runtime is defined in `runtime/bfpp_rt.h` (header) with the implementation compiled and linked by the build driver.

---

## 4. Memory Model

### 4.1 Tape

- Default size: 65,536 bytes (64 KB)
- Configurable via `--tape-size N` compiler flag (N in bytes)
- Zero-initialized at program start
- Pointer starts at position 0
- Moving pointer below 0 or above tape size is a runtime error (sets error register to 6)

### 4.2 Cell Width

- Default: 8-bit (1 byte per cell)
- Switchable per-cell via `%`: 8 → 16 → 32 → 64 → 8
- Multi-byte cells use little-endian byte order
- Cell width metadata tracked in a parallel array

### 4.3 Data Stack

- Separate memory region, default 4,096 entries
- Each entry is 64 bits wide
- Used by `$` (push) and `~` (pop)
- LIFO semantics
- Underflow sets error register to 6
- Overflow sets error register to 4

### 4.4 Call Stack

- Separate from data stack
- Default depth: 256 frames
- Each frame stores: return instruction pointer, saved error register
- Used by subroutine call (`!name`) and return (`^`)
- Overflow is a fatal error (program terminates)

### 4.5 Error Register

- Single 64-bit value
- Separate from tape and stacks
- Initialized to 0 (no error)
- Set by: syscall failures, `e` operator, stack underflow/overflow
- Read by: `E` operator, `?` operator, `R{...}` block exit check

### 4.6 Memory Map

See `MEMORY_MAP.md` for detailed layout.

| Region | Address Range | Purpose |
|--------|---------------|---------|
| General purpose | 0x0000–0x7FFF | User data, strings, computation |
| Syscall parameters | 0x8000–0x80FF | Syscall parameter staging area |
| I/O buffer | 0x8100–0x8FFF | Buffered I/O staging |
| Reserved | 0x9000–0x9FFF | Future use |
| Framebuffer | 0xA000–0xFFFF | Pixel buffer (when `--framebuffer` enabled) |

---

## 5. Subroutine Conventions

### 5.1 Calling Convention

BF++ uses an implicit calling convention based on tape position:

1. **Arguments**: Caller places arguments in tape cells starting at current `ptr` before the call.
2. **Call**: `!#name` pushes return context and jumps.
3. **Body**: Subroutine reads arguments from tape, performs work.
4. **Return value**: Subroutine leaves result in `tape[ptr]` (caller's ptr at call time).
5. **Error**: Subroutine sets error register via `e` if an error occurs, then `^` returns.
6. **Return**: `^` pops return context and resumes caller.

### 5.2 Standard Library Naming Convention

Standard library subroutines use 2-character alphanumeric names after `#`:

- First character: module identifier (e.g., `m` = math, `s` = string, `f` = file)
- Second character: operation identifier

See `STDLIB_REFERENCE.md` for the complete list.

---

## 6. Transpilation Model

### 6.1 Target

BF++ transpiles to C11. The generated C code includes `bfpp_runtime.h` which provides:

- Tape array and pointer
- Data stack and stack pointer
- Call stack (implemented via C function calls — each subroutine becomes a C function)
- Error register
- Syscall abstraction layer
- Optional framebuffer support

### 6.2 Operator → C Mapping

| BF++ | C Output |
|------|----------|
| `>` | `ptr++;` |
| `<` | `ptr--;` |
| `+` | `tape[ptr]++;` |
| `-` | `tape[ptr]--;` |
| `.` | `putchar(tape[ptr]);` |
| `,` | `tape[ptr] = getchar();` |
| `[` | `while (tape[ptr]) {` |
| `]` | `}` |
| `@` | `ptr = tape[ptr];` |
| `*+` | `tape[tape[ptr]]++;` |
| `%` | `bfpp_cycle_width(ptr);` |
| `$` | `bfpp_push(tape[ptr]);` |
| `~` | `tape[ptr] = bfpp_pop();` |
| `^` | `return;` |
| `\` | `bfpp_syscall(tape, ptr);` |
| `E` | `tape[ptr] = bfpp_err;` |
| `e` | `bfpp_err = tape[ptr];` |
| `?` | `if (bfpp_err) return;` |
| `R{` | `bfpp_err = 0; do {` |
| `}K{` | `} while(0); if (bfpp_err) {` |
| `}` (after K) | `}` |
| `\|` | `tape[ptr] \|= tape[ptr+1];` |
| `&` | `tape[ptr] &= tape[ptr+1];` |
| `x` | `tape[ptr] ^= tape[ptr+1];` |
| `s` | `tape[ptr] <<= tape[ptr+1];` |
| `r` | `tape[ptr] >>= tape[ptr+1];` |
| `n` | `tape[ptr] = ~tape[ptr];` |
| `#N` | `bfpp_set(ptr, <value>ULL);` — e.g. `#72` emits `bfpp_set(ptr, 72ULL);` |
| `%N` | `cell_width[ptr] = N;` (with sub-cell release/validation) |
| `/* ... */` | *(removed during lexing)* |
| `!#__name` | *(inline C — see Section 3.8)* |

### 6.3 Subroutine Transpilation

Each subroutine `!#name{...}` becomes a C function:

```c
void bfpp_sub_name(void) {
    // transpiled body
}
```

Calls `!#name` become `bfpp_sub_name();`.

Subroutine names are mangled: symbol characters are mapped to mnemonics (e.g., `>` → `gt`, `.` → `dot`, `+` → `plus`). Alphanumeric characters pass through unchanged.

### 6.4 Compiler Flags

| Flag | Effect |
|------|--------|
| `--tape-size N` | Set tape size to N bytes (default 65536) |
| `--stack-size N` | Set data stack size to N entries (default 4096) |
| `--call-depth N` | Set max call stack depth (default 256) |
| `--framebuffer WxH` | Enable framebuffer mode with given dimensions |
| `--no-optimize` | Disable all optimizer passes |
| `-O1` | Basic optimizations (coalescing, clear loop) |
| `-O2` | All optimizations |
| `-o FILE` | Output binary name |
| `--emit-c` | Output C source instead of compiling |
| `--include PATH` | Add stdlib search path |
| `--watch` | Poll input file every 500ms; recompile on change |
| `--features gpu` | (cargo build flag) Enable OpenCL-accelerated lexing + pattern detection |

### 6.5 Optimizer Passes

The compiler performs up to 14 optimization passes at `-O2`. Passes are applied in order; some are repeated after inlining/unrolling exposes new opportunities.

| # | Pass | Description | Level |
|---|------|-------------|-------|
| 1 | Clear loop detection | `[-]` → `bfpp_set(ptr, 0)` | `-O1` |
| 2 | Constant folding | `#5 > #3 <` with known cell values → propagate constants | `-O1` |
| 3 | Move coalescing | `>>>` → `ptr += 3`; `>>>>><<<` → `ptr += 2`; cancellation | `-O1` |
| 4 | Compile-time conditional evaluation | Resolve `?=`/`?!`/if-else when cell value is known at compile time | `-O2` |
| 5 | Scan loop optimization | `[>]` / `[<]` → memchr-based scan | `-O2` |
| 6 | Multiply-move detection | `[->+<]` patterns → direct assignment with factor | `-O2` |
| 7 | Loop unrolling | `#N [- body]` unrolled to N copies when N≤16 and body has no side effects | `-O2` |
| 8 | Auto-parallelism | Loops with ≥64 provably independent iterations rewritten to `ParallelLoop` → `bfpp_parallel_for` (see §6.10) | `-O2` |
| 9 | GPU loop upgrade | `ParallelLoop` nodes with GPU-safe bodies upgraded to `GpuLoop` → OpenCL kernel dispatch with CPU fallback (see §6.10) | `-O2` |
| 10 | Dead code elimination | Remove unreachable code after unconditional returns/errors | `-O2` |
| 11 | Subroutine inlining | Inline small subroutines (body ≤ threshold) at call site | `-O2` |
| 12 | Second constant folding | Re-run constant folding after inline/unroll expose new patterns | `-O2` |
| 13 | Second move coalescing | Re-run move/increment coalescing on newly folded code | `-O2` |
| 14 | Error folding | Fold error register writes followed by immediate propagation | `-O2` |

### 6.6 Parallel Compilation

The compiler supports parallel compilation for improved throughput:

1. **Parallel codegen**: Subroutine bodies are emitted concurrently using `rayon` `par_iter`. Each subroutine body is generated into an independent buffer, then concatenated in definition order.
2. **Parallel analysis**: Analyzer passes 2 (duplicate definition detection) and 4 (empty FFI name rejection) run concurrently via `rayon::join`.
3. **Parallel CC invocation**: The generated C code is split into per-subroutine translation units (`.c` files), compiled in parallel via threaded `cc -c` invocations, then linked in a final pass.

### 6.7 CC Flags

| Flag | Condition |
|------|-----------|
| `-O2 -Wall` | Always |
| `-mavx2 -mfma` | x86_64 targets |
| `-lSDL2` | Framebuffer mode |
| `-ldl` | FFI usage |
| `-lGL -lGLEW -lm` | 3D intrinsics |
| `-lEGL` | Multi-GPU intrinsics |
| `-lOpenCL` | GPU compute intrinsics (via `dlopen`) |
| `-lpthread` | Threading intrinsics |
| `-lz` | Compressed I/O intrinsics |

### 6.8 GPU-Accelerated Compilation

When built with `--features gpu`, the compiler uses OpenCL to accelerate lexing:
- **Character classification kernel**: each source byte is classified into a token type code in parallel on the GPU.
- **Pattern detection kernel**: identifies multi-character patterns (clear loops, scan loops, multiply-move).
- Falls back to CPU when source is under 10KB or OpenCL is unavailable.
- Produces identical results to the CPU path.

Requires the `opencl3` crate (optional dependency).

### 6.9 Bootstrap Compiler

The `bootstrap/` directory contains a self-hosting BF++ compiler written in BF++:

| File | Lines | Purpose |
|------|-------|---------|
| `bfpp_self.bfpp` | 157 | Main compiler driver |
| `parse_num.bfpp` | 92 | Numeric literal + cell width parser |
| `parse_str.bfpp` | 75 | String literal parser with escape sequences |
| `parse_sub.bfpp` | 241 | Subroutine definition/call parser |

The bootstrap compiler uses the self-hosting intrinsics (`__mul`, `__div`, `__mod`, `__strcmp`, `__strlen`, `__strcpy`, `__call`, `__hashmap_*`, `__array_*`) for efficient parsing and code generation. It parses a subset of BF++ and emits C output.

### 6.10 Auto-Parallelism and GPU Loop Offloading

The optimizer detects loops with provably independent iterations and rewrites them for parallel execution. This is fully automatic — no source-level annotation is required.

**ParallelLoop (CPU multi-threading)**:

Detection criteria:
- A `SetValue(N)` immediately precedes a `Loop` (establishes trip count)
- Trip count ≥ 64 (below this threshold, pthread dispatch overhead exceeds benefit)
- Loop body ends with `MoveRight(stride)` where stride ≥ 2
- No I/O operations (`.`, `,`), subroutine calls, or nested loops in the body
- All cell accesses are within `[0, stride)` relative to the iteration pointer — no cross-iteration aliasing

Codegen: the loop body is extracted into a file-scope C function (`_par_body_N`) and dispatched via `bfpp_parallel_for(base_ptr, trip_count, stride, body_fn)`. The runtime distributes iterations across available CPU cores. Falls back to sequential execution when `total < 2*ncpu`.

Runtime: `bfpp_rt_parallel.{c,h}`.

**GpuLoop (OpenCL offloading)**:

A `ParallelLoop` node is upgraded to `GpuLoop` when the loop body is GPU-safe:
- Body contains only arithmetic operations (`+`, `-`, `&`, `|`, `x`, `s`, `r`, `n`) and cell accesses
- No stack operations, error handling, or tape pointer movement beyond the stride

Codegen: emits a dual-path — an OpenCL kernel dispatch with a CPU `bfpp_parallel_for` fallback. The kernel source is generated at compile time and embedded in the C output. At runtime, if OpenCL is available and the GPU context is initialized, the kernel path is taken; otherwise the CPU fallback executes.

```bfpp
; Auto-parallelized loop: 1000 elements, stride 8
; Optimizer detects this as ParallelLoop → uses bfpp_parallel_for
; If body is GPU-safe, upgraded to GpuLoop → OpenCL kernel
#1000
[- >++++< >>>>>>>>]
```

### 6.11 Compressed I/O Intrinsics

Compressed I/O intrinsics provide zlib-based compression for network, file, and tape checkpoint operations. When any compressed I/O intrinsic is used, the compiler emits `#include "bfpp_rt_compress.h"` and links with `-lz`.

**Network compression**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__net_send_compressed` | `[ptr]`=fd, `[ptr+4]`=data_addr, `[ptr+8]`=len | — | Compress data with zlib and send over socket. Wire format: `[4-byte compressed_len][compressed_data]`. |
| `__net_recv_compressed` | `[ptr]`=fd, `[ptr+4]`=dest_addr, `[ptr+8]`=max_len | `tape[ptr+12]` = decompressed size | Receive compressed data from socket and decompress into tape. |

**File compression**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__file_write_compressed` | `[ptr]`=fd, `[ptr+4]`=data_addr, `[ptr+8]`=len | — | Compress and write to file. Header: `[original_size:4][compressed_size:4][compressed_data]`. |
| `__file_read_compressed` | `[ptr]`=fd, `[ptr+4]`=dest_addr, `[ptr+8]`=max_len | `tape[ptr+12]` = decompressed size | Read compressed data from file and decompress. |

**Tape checkpoints**:

| Intrinsic | Input Tape Layout | Output | Effect |
|-----------|-------------------|--------|--------|
| `__tape_save` | `ptr` -> null-terminated file path | — | Save entire tape to file (trailing zeros stripped, remainder compressed). |
| `__tape_load` | `ptr` -> null-terminated file path | `tape[ptr]` = bytes loaded | Load tape checkpoint from file, decompress, restore tape contents. |

Runtime files: `bfpp_rt_compress.{c,h}`. Requires zlib (`-lz`).

---

## 7. FFI (Future — M10)

```
\ffi "libname" "funcname"
```

- Loads shared library `libname`
- Calls function `funcname`
- Parameters read from tape at `ptr` (count and types determined by a preceding setup cell)
- Return value written to `tape[ptr]`
- Errors mapped to error register

---

## 8. Conformance

A conforming BF++ implementation must:

1. Implement all operators in Sections 3.1–3.7
2. Support the memory model in Section 4
3. Correctly transpile all standard BF programs without modification
4. Map syscall errors to BF++ error codes per `ERROR_CODES.md`
5. Provide the standard library modules listed in `STDLIB_REFERENCE.md`
