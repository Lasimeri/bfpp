# BF++ Standard Library Reference

**Version**: 0.3.0

---

## Overview

The BF++ standard library is written in BF++ itself. Each module defines subroutines using `!#name{...}` syntax. Programs include stdlib modules via the `--include` compiler flag or by placing them in the search path.

All stdlib subroutines follow the convention:
- **Arguments**: placed in tape cells at current `ptr` before call
- **Return value**: left in `tape[ptr]` after return
- **Errors**: set via error register; callers should use `?` or `R{...}K{...}`

**Note on `#N` and `%N`**: Many stdlib modules have been rewritten to use `#N` (numeric literal) and `%N` (direct cell width) operators. These replace long increment chains (`+++...+++`) and cell-width cycling, making the source substantially more readable and the generated C more efficient. Modules that have been updated are marked below.

---

## Module: `io.bfpp`

Basic I/O operations on stdin/stdout. Depends on `math.bfpp`.

**Status**: Working. Uses `!#m%` and `!#m/` from math.bfpp for digit extraction in print_int. `!#,+` uses `!#m*` for decimal accumulation.

| Subroutine | Symbol | Args | Returns | Description | Status |
|------------|--------|------|---------|-------------|--------|
| print_string | `!#.>` | `ptr` -> null-terminated string | — | Print string at current pointer to stdout. Advances ptr to null terminator. | Working |
| print_int | `!#.+` | `tape[ptr]` = integer value | — | Print cell value as decimal ASCII to stdout. Uses stack for digit reversal. Workspace: P+1..P+8. | Working |
| read_line | `!#,<` | `ptr` -> buffer start | — | Read from stdin until newline or EOF. Writes to tape at ptr. Null-terminates. | Working (basic) |
| read_int | `!#,+` | — | `tape[ptr]` = parsed integer | Read decimal integer from stdin. Accumulates via multiply-by-10-and-add. Workspace: P+0..P+6. | Working |

---

## Module: `file.bfpp`

File operations via raw Linux x86_64 syscalls. Rewritten with `#N`, `%N`, `$`/`~`, and `T` (push tape address).

**Status**: Working. Uses `%8` cells for 64-bit syscall argument layout. The `T` operator pushes `&tape[ptr]` (C pointer) to the stack for passing buffer addresses to syscalls.

| Subroutine | Symbol | Args | Returns | Errors | Status |
|------------|--------|------|---------|--------|--------|
| file_close | `!#fc` | `tape[ptr]` = fd | — | errno-mapped | Working |
| file_open | `!#fo` | `ptr` -> null-terminated path, flags byte after null, mode byte after flags | `tape[ptr]` = fd | 2 (not found), 3 (permission) | Working |
| file_read | `!#fr` | `tape[ptr]` = fd, `tape[ptr+1]` = count. Caller must push buffer address via `T` before calling. | `tape[ptr]` = bytes read | 6 (invalid fd), 15 (I/O) | Working |
| file_write | `!#fw` | `tape[ptr]` = fd, `tape[ptr+1]` = count. Caller must push buffer address via `T` before calling. | `tape[ptr]` = bytes written | 6 (invalid fd), 15 (I/O) | Working |

**Flags for file_open**:

| Value | Mode |
|-------|------|
| 0 | Read only (`O_RDONLY`) |
| 1 | Write only, create/truncate (`O_WRONLY \| O_CREAT \| O_TRUNC`) — use `%2 #0x241` for the flags cell |
| 2 | Append (`O_WRONLY \| O_CREAT \| O_APPEND`) |
| 3 | Read/write (`O_RDWR`) |

**Syscall layout**: All file operations construct a 4-slot syscall frame at `%8` cell width (P+0, P+8, P+16, P+24) matching the `\` operator's expected layout. Arguments are pushed to the stack and popped into the correct slots.

---

## Module: `net.bfpp`

TCP networking via raw Linux x86_64 syscalls. Rewritten with `#N`, `%N`, `$`/`~`, and `T`. Constructs `sockaddr_in` structures and syscall frames on the tape.

**Status**: Working. All functions use `%8` cells for 64-bit syscall arguments and `%1` cells for sockaddr_in byte-level fields.

| Subroutine | Symbol | Args | Returns | Errors | Status |
|------------|--------|------|---------|--------|--------|
| tcp_socket | `!#tcp` | ptr at clean 32-byte area | `tape[ptr]` = socket fd (`%8` cell) | errno-mapped | Working |
| tcp_connect | `!#tc` | `tape[ptr]`=fd, `[ptr+1]`=IP_A, `[ptr+2]`=IP_B, `[ptr+3]`=IP_C, `[ptr+4]`=IP_D, `[ptr+5]`=port_hi, `[ptr+6]`=port_lo (`%1` cells) | — (check `bfpp_err`) | 5 (conn refused), 7 (timeout) | Working |
| tcp_listen | `!#tl` | `tape[ptr]`=port_hi, `[ptr+1]`=port_lo (`%1` cells) | `tape[ptr]` = server fd (`%8` cell) | 12 (addr in use) | Working |
| tcp_accept | `!#ta` | `tape[ptr]` = server fd (`%8` cell) | `tape[ptr]` = client fd (`%8` cell) | 6 (invalid fd) | Working |
| tcp_send | `!#ts` | `tape[ptr]` = fd (`%8`), `tape[ptr+8]` = count (`%8`). Caller must push buffer address via `T`. | `tape[ptr]` = bytes sent | 10 (pipe), 11 (conn reset) | Working |
| tcp_recv | `!#tr` | `tape[ptr]` = fd (`%8`), `tape[ptr+8]` = max count (`%8`). Caller must push buffer address via `T`. | `tape[ptr]` = bytes read | 11 (conn reset) | Working |

**Network byte order**: Port is passed as two bytes (high, low) in big-endian order. IP address is passed as 4 individual octets. The sockaddr_in structure is constructed at P+8..P+23 with `sin_family=AF_INET`, port, and IP address in network byte order.

**Syscall chaining**: `!#tl` (tcp_listen) executes 3 syscalls sequentially — socket(), bind(), listen() — reusing the same syscall frame area (P+24..P+55) for each call.

---

## Module: `string.bfpp`

Null-terminated string operations.

**Status**: Defined. Implementation uses loop-based byte scanning.

| Subroutine | Symbol | Args | Returns | Description | Status |
|------------|--------|------|---------|-------------|--------|
| strlen | `!#sl` | `ptr` -> string | `tape[ptr]` = length | Count bytes until null terminator | Working |
| strcmp | `!#sc` | `ptr` -> string A, `tape[ptr-2..ptr-1]` = address of string B | `tape[ptr]` = result (0=equal, 1=A>B, 255=A<B) | Lexicographic comparison | Working |
| strcpy | `!#sy` | `ptr` -> dest, `tape[ptr-2..ptr-1]` = src address | — | Copy string from src to dest (including null) | Working |
| strcat | `!#sa` | `ptr` -> dest (existing string), `tape[ptr-2..ptr-1]` = src address | — | Append src to dest | Working |

---

## Module: `math.bfpp`

Loop-based unsigned arithmetic. No dependencies.

**Status**: Working. All functions use the esolangs divmod algorithm or loop-based multiplication. Note: divmod touches 6 cells (ptr+0..ptr+5), not 5 -- the algorithm's internal branching reaches one cell beyond the declared workspace.

| Subroutine | Symbol | Args | Returns | Description | Workspace | Status |
|------------|--------|------|---------|-------------|-----------|--------|
| multiply | `!#m*` | `tape[ptr]` = A, `tape[ptr+1]` = B | `tape[ptr]` = A * B | Unsigned multiplication. Clears ptr+1. | ptr+2..ptr+3 | Working |
| divide | `!#m/` | `tape[ptr]` = A, `tape[ptr+1]` = B | `tape[ptr]` = A / B | Unsigned division. Error 6 if B=0. | ptr+2..ptr+5 | Working |
| modulo | `!#m%` | `tape[ptr]` = A, `tape[ptr+1]` = B | `tape[ptr]` = A % B | Unsigned modulo. Error 6 if B=0. | ptr+2..ptr+5 | Working |
| power | `!#mcaret` | `tape[ptr]` = base, `tape[ptr+1]` = exp | `tape[ptr]` = base^exp | Unsigned exponentiation via repeated multiply. | ptr+2..ptr+8 | Working |

---

## Module: `mem.bfpp`

Memory management within the tape. Rewritten with `#N`, `%N`, `*$`, and `*~` (deref-push/pop).

**Status**: Working. Uses `#N` and `%N` extensively. Heap allocator uses deref operators for indirect memory access.

| Subroutine | Symbol | Args | Returns | Description | Status |
|------------|--------|------|---------|-------------|--------|
| heap_init | `!#mi` | — | — | Initialize heap metadata: sets `tape[0x9000] = 0x1000`. Must be called once before `!#ma`. | Working |
| malloc | `!#ma` | `tape[ptr]` = size | `tape[ptr]` = allocated address (16-bit) | Bump allocator. Reads next-free from `tape[0x9000]`, advances it by size. | Working |
| free | `!#mf` | `tape[ptr]` = address | — | No-op (bump allocator). To reset heap, re-call `!#mi`. | Stub |
| memcpy | `!#mc` | `tape[ptr]` = dest addr, `tape[ptr+1]` = src addr, `tape[ptr+2]` = count | — | Copy count bytes using `*$` (deref-push) and `*~` (deref-pop) per byte. Non-overlapping only. | Working |
| memset | `!#ms` | `tape[ptr]` = dest addr, `tape[ptr+1]` = value, `tape[ptr+2]` = count | — | Fill count cells at dest with value using `*~` (deref-pop). Value preserved across iterations. | Working |

**Heap design**: Bump allocator with metadata at `tape[0x9000]` (16-bit next-free pointer). Allocatable range starts at `0x1000`. No bounds checking, no compaction, no defragmentation. Freed memory is never reclaimed. Suitable for small, bounded allocations. To reset, call `!#mi` again.

---

## Module: `tui.bfpp`

Terminal UI via ANSI escape sequences. Fully rewritten with `#N` numeric literals. Depends on `io.bfpp` (for `!#.+` print_int).

**Status**: Working. All ANSI sequences are emitted using `#N .` patterns (e.g., `#27 . #91 .` for `ESC [`). The `#N` operator eliminated hundreds of increment chains, making the source readable and maintainable.

| Subroutine | Symbol | Args | Returns | Description | Workspace | Status |
|------------|--------|------|---------|-------------|-----------|--------|
| cursor_move | `!#cm` | `tape[ptr]` = row, `tape[ptr+1]` = col | — | Move cursor to (row, col) using `ESC[row;colH`. Uses stack to preserve args across print_int calls. | P+0..P+9 | Working |
| set_color | `!#co` | `tape[ptr]` = fg (0-255), `tape[ptr+1]` = bg (0-255) | — | Set 256-color mode: `ESC[38;5;{fg}m ESC[48;5;{bg}m`. | P+0..P+9 | Working |
| clear_screen | `!#cl` | — | — | Clear screen: `ESC[2J ESC[H`. | P+0 | Working |
| cursor_hide | `!#ch` | — | — | Hide cursor: `ESC[?25l`. | P+0 | Working |
| cursor_show | `!#cs` | — | — | Show cursor: `ESC[?25h`. | P+0 | Working |
| color_reset | `!#cr` | — | — | Reset colors: `ESC[0m`. | P+0 | Working |
| draw_box | `!#db` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = w, `[ptr+3]` = h | — | Draw ASCII box with `+` corners, `-` horizontal, `\|` vertical. Minimum 2x2. | P+0..P+13 | Working |
| draw_hline | `!#dl` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = len, `[ptr+3]` = char | — | Draw horizontal line of `char` repeated `len` times. | P+0..P+9 | Working |
| draw_vline | `!#dv` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = len, `[ptr+3]` = char | — | Draw vertical line, one char per row. | P+0..P+14 | Working |
| read_key | `!#kb` | — | `tape[ptr]` = char code | Read single keypress (requires raw terminal mode). | P+0 | Working (basic) |

**Color codes** (256-color mode via `!#co`):

| Range | Description |
|-------|-------------|
| 0-7 | Standard colors (black, red, green, yellow, blue, magenta, cyan, white) |
| 8-15 | Bright/bold variants |
| 16-231 | 216-color RGB cube (6x6x6) |
| 232-255 | Grayscale ramp (dark to light) |

---

## Module: `err.bfpp`

Error handling utilities. Rewritten with `#N`, `%N`. Depends on `io.bfpp`.

**Status**: Working. Uses `#60` for `SYS_exit`, `.{2}` for stderr output.

| Subroutine | Symbol | Args | Returns | Description | Status |
|------------|--------|------|---------|-------------|--------|
| err_to_string | `!#es` | Error code in error register | Prints "err:" prefix + numeric code to stdout | Read error register, print as "err:N" via `!#.+`. | Working |
| panic | `!#ep` | `ptr` -> message string (optional) | Does not return | Print message to stderr (`.{2}`), then `SYS_exit(1)` via `\`. | Working |
| assert | `!#ea` | `tape[ptr]` = condition | — | If condition == 0, panic with "assertion failed\\n". | Working |

---

## Module: `graphics.bfpp`

SDL2 framebuffer drawing primitives. Requires `--framebuffer WxH` compiler flag. Depends on `math.bfpp`.

**Status**: Working (set_pixel, get_pixel, clear_fb, fill_rect, draw_hline). Stubs for draw_rect and draw_line due to architectural limitations.

All functions require `%4` (32-bit) cell width because `BFPP_FB_OFFSET` exceeds 16-bit cell range. The framebuffer offset and screen width must be passed as parameters since BF++ source cannot reference C `#define` values.

After calling any function that uses `@` to jump into the framebuffer, `ptr` ends inside the framebuffer region. Callers must re-establish `ptr` position after the call.

| Subroutine | Symbol | Args | Returns | Description | Workspace | Status |
|------------|--------|------|---------|-------------|-----------|--------|
| set_pixel | `!#px` | P+0=fb_offset, P+1=width, P+2=x, P+3=y, P+4=r, P+5=g, P+6=b | RGB written to framebuffer. ptr ends at pixel+2 in FB. | Compute pixel address, `@` jump, write RGB. Uses stack for r/g/b transfer. | P+7..P+15 | Working |
| get_pixel | `!#gx` | P+0=fb_offset, P+1=width, P+2=x, P+3=y | Stack: r, g, b (pop order: b, g, r). ptr ends at pixel+2. | Compute pixel address, `@` jump, push RGB to stack. | P+4..P+12 | Working |
| clear_fb | `!#gc` | P+0=fb_offset, P+1=width, P+2=height, P+3=r, P+4=g, P+5=b | Entire FB filled. ptr ends inside FB. | Compute total pixels, sliding-window fill. | P+6..P+10 | Working |
| fill_rect | `!#fl` | P+0=fb_offset, P+1=screen_width, P+2=rect_x, P+3=rect_y, P+4=rect_w, P+5=rect_h, P+6=r, P+7=g, P+8=b | Rectangle filled. ptr ends inside FB. | Compute start address + pixel count, sliding-window fill. | P+9..P+18 | Working (linear fill) |
| draw_rect | `!#rc` | — | — | Outline rectangle. | — | Stub |
| draw_hline | `!#lh` | P+0=fb_offset, P+1=screen_width, P+2=x, P+3=y, P+4=length, P+5=(unused), P+6=r, P+7=g, P+8=b | Horizontal line drawn. ptr ends inside FB. | Sets P+5=1, delegates to `!#fl`. | (same as fl) | Working |
| draw_line | `!#ln` | — | — | Bresenham's line algorithm. | — | Stub |

**Limitations**:
- `fill_rect` performs a linear fill starting at `(rect_x, rect_y)`. Correct when `rect_x == 0` or `rect_w == screen_width`. Narrow rectangles not starting at x=0 wrap at row boundaries, producing visual artifacts.
- `draw_rect` (outline) is not implementable as a single BF++ function due to the `@` operator being a one-way jump. Workaround: call `!#fl` with `rh=1` for horizontal edges and `rw=1` for vertical edges.
- `draw_line` (Bresenham's) requires signed arithmetic and per-pixel loop with return to param area, which is blocked by `@`. Not implementable in pure BF++. Use `!#lh` for horizontal lines or `!#px` per pixel with manual coordinate stepping.
- `clear_fb` overwrites the first ~4 bytes at `BFPP_FB_OFFSET` with scratch data (counter + template), affecting ~1.3 pixels at typical resolutions.

**Usage example** -- draw a red pixel at (10, 20):
```bfpp
%4                          ; 32-bit cells required
#40960 >                    ; P+0 = fb_offset (0xA000)
#320 >                      ; P+1 = width
#10 >                       ; P+2 = x
#20 >                       ; P+3 = y
#255 >                      ; P+4 = r
#0 >                        ; P+5 = g
#0                          ; P+6 = b
<<<<<<                      ; back to P+0
!#px                        ; set_pixel
F                           ; flush to screen
```

---

## Module: `3d.bfpp`

3D rendering wrappers for all `__gl_*`, `__fp_*`, `__mesh_*`, and `__scene_*` compiler intrinsics. Provides thin subroutines that set up tape parameters and call the corresponding intrinsic. 485 lines.

**Status**: Working. All subroutines are thin wrappers (tape setup + intrinsic call + return). Numeric values use Q16.16 fixed-point (65536 = 1.0).

**Tier 1 — GL Proxy Wrappers**:

| Subroutine | Symbol | Intrinsic Called | Description |
|------------|--------|------------------|-------------|
| gl_init | `\g3i` | `__gl_init` | Initialize GL context. `tape[ptr]`=width, `[ptr+4]`=height. |
| gl_cleanup | `\g3c` | `__gl_cleanup` | Destroy GL context and free resources. |
| gl_create_buffer | `\gcb` | `__gl_create_buffer` | Create buffer. Returns buffer ID in `tape[ptr]`. |
| gl_buffer_data | `\gbd` | `__gl_buffer_data` | Upload data to buffer. |
| gl_delete_buffer | `\gdb` | `__gl_delete_buffer` | Delete buffer. |
| gl_create_vao | `\gva` | `__gl_create_vao` | Create VAO. Returns VAO ID. |
| gl_bind_vao | `\gbv` | `__gl_bind_vao` | Bind VAO. |
| gl_vertex_attrib | `\gat` | `__gl_vertex_attrib` | Configure vertex attribute. |
| gl_delete_vao | `\gdv` | `__gl_delete_vao` | Delete VAO. |
| gl_create_shader | `\gcs` | `__gl_create_shader` | Create shader (0=vertex, 1=fragment). |
| gl_shader_source | `\gss` | `__gl_shader_source` | Set shader source. |
| gl_compile_shader | `\gcc` | `__gl_compile_shader` | Compile shader. Returns 1/0. |
| gl_create_program | `\gcp` | `__gl_create_program` | Create program. Returns program ID. |
| gl_attach_shader | `\gas` | `__gl_attach_shader` | Attach shader to program. |
| gl_link_program | `\glp` | `__gl_link_program` | Link program. Returns 1/0. |
| gl_use_program | `\gup` | `__gl_use_program` | Bind program. |
| gl_uniform_loc | `\gul` | `__gl_uniform_loc` | Query uniform location. |
| gl_uniform_1f | `\gu1` | `__gl_uniform_1f` | Set float uniform (Q16.16). |
| gl_uniform_3f | `\gu3` | `__gl_uniform_3f` | Set vec3 uniform (Q16.16). |
| gl_uniform_4f | `\gu4` | `__gl_uniform_4f` | Set vec4 uniform (Q16.16). |
| gl_uniform_mat4 | `\gum` | `__gl_uniform_mat4` | Set mat4 uniform (Q16.16). |
| gl_clear | `\gcl` | `__gl_clear` | Clear color+depth buffers. |
| gl_draw_arrays | `\gda` | `__gl_draw_arrays` | Draw from arrays. |
| gl_draw_elements | `\gde` | `__gl_draw_elements` | Draw indexed. |
| gl_viewport | `\gvp` | `__gl_viewport` | Set viewport. |
| gl_depth_test | `\gdt` | `__gl_depth_test` | Enable/disable depth test. |
| gl_present | `\g3p` | `__gl_present` | Swap buffers / present frame. |
| gl_shadow_enable | `\gse` | `__gl_shadow_enable` | Enable shadow mapping. |
| gl_shadow_disable | `\gsd` | `__gl_shadow_disable` | Disable shadow mapping. |
| gl_shadow_quality | `\gsq` | `__gl_shadow_quality` | Set shadow map quality. |

**Tier 2 — Fixed-Point Math Wrappers**:

| Subroutine | Symbol | Intrinsic Called | Description |
|------------|--------|------------------|-------------|
| fp_mul | `\fpm` | `__fp_mul` | Q16.16 multiply. |
| fp_div | `\fpd` | `__fp_div` | Q16.16 divide. |
| fp_sin | `\fps` | `__fp_sin` | Sine (LUT-based). |
| fp_cos | `\fpc` | `__fp_cos` | Cosine (LUT-based). |
| fp_sqrt | `\fpq` | `__fp_sqrt` | Square root. |
| mat4_identity | `\m4i` | `__mat4_identity` | Write identity matrix. |
| mat4_multiply | `\m4m` | `__mat4_multiply` | Matrix multiply. |
| mat4_rotate | `\m4r` | `__mat4_rotate` | Rotate matrix around axis. |
| mat4_translate | `\m4t` | `__mat4_translate` | Translate matrix. |
| mat4_perspective | `\m4p` | `__mat4_perspective` | Build perspective projection. |

**Tier 3 — Mesh Generator Wrappers**:

| Subroutine | Symbol | Intrinsic Called | Description |
|------------|--------|------------------|-------------|
| mesh_cube | `\mcb` | `__mesh_cube` | Generate cube mesh. Returns vertex count. |
| mesh_sphere | `\msp` | `__mesh_sphere` | Generate UV sphere. Returns vertex count. |
| mesh_torus | `\mto` | `__mesh_torus` | Generate torus. Returns vertex count. |
| mesh_plane | `\mpl` | `__mesh_plane` | Generate plane quad. Returns vertex count. |
| mesh_cylinder | `\mcy` | `__mesh_cylinder` | Generate cylinder. Returns vertex count. |

**Multi-GPU & Scene Oracle Wrappers**:

| Subroutine | Symbol | Intrinsic Called | Description |
|------------|--------|------------------|-------------|
| multi_gpu | `\mgpu` | `__gl_multi_gpu` | Set multi-GPU mode (0=off, 1=SFR, 2=AFR). |
| gpu_count | `\gcnt` | `__gl_gpu_count` | Query GPU count. |
| frame_time | `\gft` | `__gl_frame_time` | Query last frame time (microseconds). |
| scene_publish | `\spub` | `__scene_publish` | Publish scene state (triple-buffer swap). |
| scene_mode | `\smod` | `__scene_mode` | Set scene oracle mode. |
| scene_extrap | `\sext` | `__scene_extrap_ms` | Set extrapolation lookahead (ms). |
