# BF++

A Brainfuck superset transpiler that compiles to C, adding syscalls, subroutines, error handling, bitwise ops, stack operations, FFI, numeric literals, preprocessor macros (`!define`/`!undef`), if/else syntax (`?{...}:{...}`), compiler intrinsics (terminal, TUI, process, I/O, SDL input, textures), self-hosting intrinsics (arithmetic, strings, hash maps, indirect calls), OpenCL GPU compute offloading (12 `__gpu_*` intrinsics), optional GPU-accelerated compilation (`--features gpu`), optional SDL2 framebuffer graphics, 3D rendering (OpenGL 3.3 + software rasterizer fallback), multi-GPU support (EGL multi-context SFR/AFR/AUTO), AVX2 SIMD acceleration, terminal framebuffer backend (headless/SSH rendering), and a self-hosting bootstrap compiler. Written in Rust with parallel compilation (rayon). Produces self-contained, single-file C programs with an embedded runtime. Includes external C runtime libraries for double-buffered TUI rendering, 3D rendering, multi-GPU coordination, OpenCL compute, and terminal framebuffer rendering.

---

## Operator Reference

### Core (BF-Compatible)

| Symbol | Name | Semantics |
|--------|------|-----------|
| `>` | Move right | `ptr++` (wraps via bitmask) |
| `<` | Move left | `ptr--` (wraps via bitmask) |
| `+` | Increment | `tape[ptr]++` (wrapping) |
| `-` | Decrement | `tape[ptr]--` (wrapping) |
| `.` | Output | `putchar(tape[ptr])` |
| `,` | Input | `tape[ptr] = getchar()` (EOF configurable) |
| `[` | Loop start | `while (tape[ptr]) {` |
| `]` | Loop end | `}` |

### Memory and Data

| Symbol | Name | Semantics |
|--------|------|-----------|
| `@` | Absolute address | `ptr = tape[ptr]` -- jump pointer to address stored in current cell |
| `*` | Dereference | Prefix modifier: saves ptr, sets `ptr = tape[ptr]`, executes next op, restores ptr. `*+` increments `tape[tape[ptr]]` |
| `%` | Cell width cycle | Cycle cell bit-width at ptr: 8 -> 16 -> 32 -> 64 -> 8. Multi-byte cells use little-endian layout in consecutive tape bytes |
| `%1` `%2` `%4` `%8` | Direct cell width | Set cell width at ptr to 1/2/4/8 bytes without cycling. `%4` = 32-bit. Avoids needing `%%%` to reach a known width |
| `#N` | Numeric literal (decimal) | `tape[ptr] = N` -- set current cell to immediate value N. Respects cell width (e.g., `#36864` in a `%4` cell) |
| `#0xHH` | Numeric literal (hex) | `tape[ptr] = 0xHH` -- hex variant. `#0xFF` sets cell to 255. Supports full 64-bit range |
| `"..."` | String literal | Write bytes to tape at ptr, advance ptr past last byte. Supports `\0 \n \r \t \\ \" \xHH` escapes |

### Stack

| Symbol | Name | Semantics |
|--------|------|-----------|
| `$` | Push | Push `tape[ptr]` onto 64-bit auxiliary data stack |
| `~` | Pop | Pop top of data stack into `tape[ptr]` |

### Subroutines

| Symbol | Name | Semantics |
|--------|------|-----------|
| `!#name{...}` | Define | Define named subroutine with body. Becomes a C function. Not executed at definition |
| `!#name` | Call | Invoke subroutine by name. Pushes call frame, checks depth limit |
| `^` | Return | Early return from subroutine (decrements call depth). At top level: `return 0` from main |

### System Interface

| Symbol | Name | Semantics |
|--------|------|-----------|
| `\` | Syscall | Raw syscall. Number at `tape[ptr]`, args at `ptr+8,+16,...,+48` (64-bit each). Result written to `tape[ptr]`. Errno mapped to error register |
| `.{N}` | Write to fd | `write(N, &tape[ptr], 1)` -- N is decimal literal |
| `.{*}` | Write to indirect fd | `write(tape[ptr+1], &tape[ptr], 1)` -- fd from adjacent cell |
| `,{N}` | Read from fd | `read(N, &tape[ptr], 1)` |
| `,{*}` | Read from indirect fd | `read(tape[ptr+1], &tape[ptr], 1)` |

### Bitwise and Arithmetic

| Symbol | Name | Semantics |
|--------|------|-----------|
| `\|` | OR | `tape[ptr] \|= tape[ptr+1]` |
| `&` | AND | `tape[ptr] &= tape[ptr+1]` |
| `x` | XOR | `tape[ptr] ^= tape[ptr+1]` |
| `s` | Shift left | `tape[ptr] <<= tape[ptr+1]` |
| `r` | Shift right | `tape[ptr] >>= tape[ptr+1]` |
| `n` | NOT | `tape[ptr] = ~tape[ptr]` |

### Error Handling

| Symbol | Name | Semantics |
|--------|------|-----------|
| `E` | Error read | `tape[ptr] = bfpp_err` |
| `e` | Error write | `bfpp_err = tape[ptr]` |
| `?` | Propagate | If `bfpp_err != 0`, return from current subroutine. In `R{}` block: `break`. Analogous to Rust's `?` |
| `R{...}K{...}` | Result/Catch | Try/catch. R body executes; if error register is set, control transfers to K body. Implemented via `do{...}while(0)` + `break` in generated C |

### Framebuffer and Tape Address

| Symbol | Name | Semantics |
|--------|------|-----------|
| `T` | Tape address | Push `&tape[ptr]` (C pointer) onto stack. Used to pass buffer addresses to syscalls/FFI |
| `F` | Framebuffer flush | Request framebuffer flush (non-blocking). Sets atomic flag; presenter thread renders at vsync cadence. No-op if framebuffer not enabled |

### Dual-Tape (Multicore Data Transformation)

Separate read/write tapes for parallel data processing. Each thread gets its own R/W tape pair; the primary `tape[]` stays shared.

| Symbol | Name | Semantics |
|--------|------|-----------|
| `{` | Read tape | `tape[ptr] = rtape[rptr]` — copy from read tape into current cell (standalone `{` only, not opening a block) |
| `}` | Write tape | `wtape[wptr] = tape[ptr]` — copy current cell to write tape (standalone `}` only, not closing a block) |
| `(` | Read ptr right | `rptr++` (wraps via bitmask) |
| `)` | Read ptr left | `rptr--` (wraps via bitmask) |
| `P` | Transfer | `wtape[wptr] = rtape[rptr]` — direct copy between tapes |
| `Q` | Swap tapes | Swap read and write tape contents + pointers |
| `V` | Sync pointers | `rptr = wptr` — synchronize read pointer to write pointer position |

### FFI

| Symbol | Name | Semantics |
|--------|------|-----------|
| `\ffi "lib" "func"` | Foreign call | `dlopen(lib)` -> `dlsym(func)` -> call with 6 args from tape layout (same as syscall). Result to `tape[ptr]`. Sets `ERR_NOLIB`/`ERR_NOSYM` on failure |

### Other

| Symbol | Name | Semantics |
|--------|------|-----------|
| `;` | Line comment | Line comment (to end of line) |
| `/* ... */` | Block comment | Nestable block comment. `/* outer /* inner */ still comment */` is valid. Unterminated block comment is a lex error |
| `!include "file"` | Include | Preprocessor directive: splice file contents into source before lexing |
| `!define NAME VALUE` | Define macro | Preprocessor: all subsequent occurrences of NAME are replaced with VALUE before lexing |
| `!undef NAME` | Undefine macro | Preprocessor: remove a previously defined macro |
| `?{...}:{...}` | If/else | Destructive truthiness test: if `tape[ptr]` is nonzero, execute true body; else execute false body. Cell is consumed (set to 0) |

---

## Architecture

Compilation pipeline:

```
source.bfpp
    |
    v
[Preprocess] -- expand !include directives, !define/!undef macros, cycle detection, resolve search paths
    |
    v
[Lex] -- single-pass character dispatch -> flat token stream. Unrecognized chars = comments
    |
    v
[Parse] -- recursive descent -> AST. Coalesces consecutive +/-/>/<  into counted nodes
    |
    v
[Analyze] -- semantic validation: undefined subs, duplicate defs, empty FFI names, return context
    |
    v
[Optimize] -- peephole passes on AST (configurable level)
    |
    v
[Codegen] -- AST -> single self-contained C file with embedded runtime
    |
    v
[CC] -- invoke C compiler (default: cc -O2 -Wall) -> native binary
```

### Stage Details

| Stage | File | Key Mechanism |
|-------|------|---------------|
| Preprocess | `preprocess.rs` | Line-by-line `!include` expansion, `!define`/`!undef` macro substitution. Resolves relative to source dir, then `--include` paths, then `./stdlib/`, then exe-adjacent `stdlib/`. Cycle detection via canonical path HashSet. Max depth 64 |
| Lex | `lexer.rs` | Peek-based character dispatcher. Multi-char tokens (strings, subroutines, fd specs, FFI, numeric literals, block comments) consume inline. Backslash lookahead cloning for `\ffi` vs `\` disambiguation. `#N`/`#0xHH` parsed as decimal/hex immediates. `/* */` with nesting depth counter. `%N` disambiguated by lookahead for 1/2/4/8. Optional GPU acceleration via OpenCL (`--features gpu`) for parallel character classification on large sources |
| Parse | `parser.rs` | `parse_block`/`parse_single` recursive descent. `BlockEnd` enum tracks context (`]` vs `}` vs EOF). Consecutive movement/arithmetic tokens coalesced via `count_consecutive`. `*` recursively wraps the next single op. `R{...}K{...}` pairing enforced here |
| Analyze | `analyzer.rs` | Four passes: (1) collect sub defs/calls into HashSets, check for undefined calls; (2) detect duplicate defs with separate `seen` set; (3) warn on top-level `^`; (4) reject empty FFI names. Passes 2+4 run in parallel via `rayon::join` |
| Optimize | `optimizer.rs` | 12 ordered peephole passes: clear-loop, scan-loop, multiply-move, error-folding, constant-fold, conditional eval, loop unrolling, move coalescing, tail return elimination, second fold round. Per-subroutine optimization parallelized via rayon. Each pass recurses into all block-containing nodes |
| Codegen | `codegen.rs` | Emits C header (includes, defines, runtime state, helper functions, errno mapping, syscall wrapper, constructor, optional SDL2 framebuffer, optional dlfcn), forward-declares subs, emits sub bodies with call-depth guards, then main(). Subroutine bodies emitted in parallel via rayon `par_iter`. Subroutine names mangled for C identifier compatibility. `__`-prefixed sub calls are intercepted as compiler intrinsics (inline C emission). Intrinsic usage detection drives conditional `#include` emission and runtime linking |
| CC | `main.rs` | Parallel compilation: per-subroutine `.c` files compiled concurrently via threaded `cc -c`, then linked. Passes `-mavx2 -mfma` on x86_64 |

---

## Building

```sh
cargo build --release

# With GPU-accelerated compilation (optional, requires OpenCL runtime):
cargo build --release --features gpu
```

Binary at `target/release/bfpp`. Dependencies: `clap 4` (derive), `rayon 1` (parallel codegen/analysis). Optional: `opencl3 0.9` (GPU-accelerated lexing, enabled via `--features gpu`).

Runtime requirements for generated programs: a C compiler (gcc/clang), POSIX libc. Optional: SDL2 (framebuffer mode), libdl (FFI mode), libGL + libGLEW (3D intrinsics), libEGL (multi-GPU intrinsics), libm (3D math), libOpenCL (GPU compute intrinsics). Programs using `__tui_*` intrinsics require `runtime/bfpp_rt.{h,c}` (compiled and linked automatically by the `bfpp` driver). Programs using 3D intrinsics require `runtime/bfpp_rt_3d*.{h,c}` (6 files, auto-linked). Programs using `__gpu_*` intrinsics require `runtime/bfpp_rt_opencl.{c,h}` (auto-linked). The terminal framebuffer backend (`runtime/bfpp_fb_terminal.{c,h}`) is auto-linked when `--framebuffer` is active and no display server is detected (or `BFPP_TERMINAL_FB=1`). On x86_64, `-mavx2 -mfma` flags are passed to the C compiler automatically.

---

## Usage

```
bfpp [OPTIONS] <INPUT>
```

| Flag | Default | Description |
|------|---------|-------------|
| `<INPUT>` | required | Input `.bfpp` source file |
| `-o <FILE>` | input stem | Output binary name |
| `--emit-c` | false | Write generated C source to disk instead of compiling |
| `--tape-size <N>` | 65536 | Tape size in bytes. Must be power of 2 (bitmask wrapping) |
| `--stack-size <N>` | 4096 | Data stack entries (64-bit each) |
| `--call-depth <N>` | 256 | Max subroutine recursion depth. Overflow is fatal |
| `--framebuffer <WxH>` | none | Enable SDL2 framebuffer with given dimensions (e.g., `80x60`). Links `-lSDL2`. Also enables 3D rendering support |
| `--render-threads <N>` | 8 | Number of render threads for framebuffer/3D pipeline |
| `--no-optimize` | false | Disable all optimizer passes |
| `-O <LEVEL>` | 1 | Optimization level: 0=none, 1=basic, 2+=full |
| `--include <PATH>` | none | Additional include search path (repeatable) |
| `--cc <COMPILER>` | `cc` | C compiler command |
| `--eof <VALUE>` | 0 | Value written to cell on EOF during `,` (0 or 255) |
| `--watch` | false | Watch mode: recompile automatically when source files change |

### Examples

```sh
# Compile and run hello world
bfpp examples/hello_bfpp.bfpp -o hello && ./hello

# Emit C source for inspection
bfpp examples/hello.bfpp --emit-c

# Compile with stdlib, full optimization, large tape
bfpp program.bfpp -O2 --tape-size 131072 --include stdlib/ -o program

# Framebuffer mode (links SDL2)
bfpp game.bfpp --framebuffer 80x60 --tape-size 131072 -o game

# FFI (links libdl automatically)
bfpp ffi_demo.bfpp -o ffi_demo

# 3D rendering (links GL, GLEW, math; requires SDL2 framebuffer)
bfpp examples/3d_demo.bfpp --include stdlib --framebuffer 640x480 --tape-size 1048576 -o 3d_demo

# Build the bootstrap compiler (BF++ compiler written in BF++)
bfpp bootstrap/bfpp_self.bfpp --include bootstrap --include stdlib --tape-size 1048576 -o bfpp_bootstrap

# GPU-accelerated compilation (requires OpenCL)
cargo build --release --features gpu
```

---

## Quick Start

### Classic BF Hello World

```brainfuck
; BF++ is a strict superset of Brainfuck
++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.
```

### BF++ Hello World (String Literals + Subroutines)

```brainfuck
; Define a print-string subroutine
!#pr{
  [.>]    ; print bytes until null
  ^
}

; Write string to tape, rewind, call print
"Hello, World!\n\0"
<<<<<<<<<<<<<<<
!#pr
```

### Numeric Literals and Direct Cell Width

```brainfuck
/* Set a 32-bit cell to a large value */
%4          ; 32-bit cell (direct, no cycling)
#36864      ; tape[ptr] = 36864

/* Hex literal for ASCII */
#0x48 . #0 ; print 'H', then clear cell

/* Mix with arithmetic */
#65 +++++ . ; tape[ptr] = 65 + 5 = 70 = 'F', print
```

### Error Handling

```brainfuck
!#fail{
  #6 e       ; set error register to 6 (ERR_INVALID_ARG)
  ^
}

R{
  !#fail     ; call subroutine that sets error
}K{
  E          ; load error code into cell
  #48 >      ; put 48 ('0') in next cell
  <[->+<]>.  ; add error code to 48 -> ASCII digit, print
}
```

---

## Standard Library

11 modules, all written in BF++. Include via `!include "module.bfpp"` or `--include stdlib/`. All modules use `#N`/`%N` operators where applicable.

### Module Status

| Module | File | Functions | Status |
|--------|------|-----------|--------|
| **I/O** | `io.bfpp` | `!#.>` print_string, `!#.+` print_int, `!#,<` read_line, `!#,+` read_int | Working. print_int uses math.bfpp for divmod |
| **Math** | `math.bfpp` | `!#m*` multiply, `!#m/` divide, `!#m%` modulo, `!#mcaret` power | Working. Loop-based algorithms, 8-bit cells. Uses esolangs divmod |
| **File** | `file.bfpp` | `!#fo` open, `!#fr` read, `!#fw` write, `!#fc` close | Thin syscall wrappers. Caller must set up 64-bit cells and syscall param layout. `!#fc` (close) is self-contained; others require manual setup |
| **Net** | `net.bfpp` | `!#tcp` socket, `!#tl` listen, `!#ta` accept, `!#ts` send, `!#tr` recv | Thin syscall wrappers. Caller constructs sockaddr_in and syscall params manually. Linux x86_64 syscall numbers |
| **String** | `string.bfpp` | `!#sl` strlen, `!#sc` strcmp, `!#sy` strcpy, `!#sa` strcat | `!#sl` works (scan right to null). `!#sa` works for adjacent strings only. `!#sc` and `!#sy` are stubs -- not implementable in BF's single-pointer model without additional primitives |
| **Memory** | `mem.bfpp` | `!#mc` memcpy, `!#ms` memset, `!#ma` malloc, `!#mf` free | Stubs. `!#ma` always returns ERR_OOM (4) -- bump allocator not feasible with 8-bit default cells (heap address >255). `!#mf` is a no-op. `!#mc`/`!#ms` fail due to @/stack round-trip limitations |
| **TUI** | `tui.bfpp` | `!#cm` cursor_move, `!#cl` clear, `!#co` set_color, `!#db` draw_box | Working. ANSI escape sequences. `!#cm` limited to single-digit row/col (1-9). `!#db` draws box with +/-/\| characters |
| **Error** | `err.bfpp` | `!#es` err_to_string, `!#ep` panic, `!#ea` assert | Working. `!#es` prints single-digit error codes (0-9). `!#ep` uses SYS_exit via `\`. `!#ea` calls `!#ep` on assertion failure |
| **Graphics** | `graphics.bfpp` | `!#px` set_pixel, `!#gx` get_pixel, `!#gc` clear_fb, `!#fl` fill_rect, `!#lh` draw_hline, `!#rc` draw_rect (stub), `!#ln` draw_line (stub) | SDL2 framebuffer primitives. Requires `--framebuffer WxH` and `%4` (32-bit cells). `!#px`/`!#gx`/`!#gc`/`!#fl` working. `!#rc`/`!#ln` are stubs (not implementable due to `@` single-jump constraint). Includes math.bfpp for address computation |
| **3D** | `3d.bfpp` | ~45 intrinsics across 3 tiers: GL proxy intrinsics, Q16.16 fixed-point math, mesh generators | OpenGL 3.3 core profile with software rasterizer fallback. Blinn-Phong shading, PCF shadow mapping. Renders to offscreen FBO, async PBO readback to tape[FB_OFFSET]. Multi-GPU support via EGL (SFR/AFR/AUTO). Scene oracle for lock-free triple-buffered CPU-GPU data transfer. 485 lines |
| **Math3D** | `math3d.bfpp` | Pure BF++ 3D math: vectors, matrices, transforms | 585 lines of pure BF++ math — no C intrinsics. Provides vector/matrix operations for 3D computation using only BF++ operators and stdlib |

### Calling Convention

All stdlib functions follow the same pattern:
- **Arguments**: placed in tape cells at current `ptr` before call
- **Return value**: left in `tape[ptr]` after return
- **Errors**: set via error register; callers use `?` or `R{...}K{...}`
- **Workspace**: functions document which cells relative to ptr they clobber

---

## Optimization

Three levels controlled by `-O` flag (overridden by `--no-optimize`). 12 optimizer passes total.

| Level | Flag | Passes | Description |
|-------|------|--------|-------------|
| None | `-O0` | -- | AST passes through unchanged |
| Basic | `-O1` | clear-loop, error-folding, constant-fold | `[-]`/`[+]` -> `Clear` (cell = 0). Consecutive `?` collapsed to one. Dead stores and arithmetic folded |
| Full | `-O2` | All 12 passes | Adds: scan-loop, multiply-move, conditional eval, loop unrolling, move coalescing, tail return elimination, second fold round |

### Optimizer Details

| Pass | Pattern | Replacement | Speedup |
|------|---------|-------------|---------|
| Clear loop | `[-]` or `[+]` | `tape[ptr] = 0` | Eliminates loop (up to 255 iterations -> 1 assignment) |
| Scan loop | `[>]` or `[<]` | `while(tape[ptr]) ptr++/--` | Semantically equivalent but enables future memchr optimization |
| Multiply-move | `[->>+++<<]` | `tape[ptr+2] += tape[ptr]*3; tape[ptr]=0` | O(N*M) loop -> O(M) straight-line. Detects balanced decrement-move-increment patterns. Merges duplicate target offsets |
| Error folding | `???` | `?` | Consecutive propagate checks are redundant. N-1 branch instructions eliminated |
| Constant fold | `#5 +++` | `#8` | Dead store elimination + arithmetic folding. Adjacent set/inc/dec collapsed |
| Conditional eval | `?{const}:{const}` | Direct branch | Compile-time evaluation of if/else with known constant conditions |
| Loop unrolling | Small fixed-count loops | Unrolled body | Eliminates loop overhead for short iteration counts |
| Move coalescing | `> > > <` | `>>` | Merges adjacent pointer moves, cancels opposing moves |
| Tail return elimination | `!#sub ... ^` at end | Optimized return | Eliminates redundant return at end of subroutine body |
| Second fold round | Post-optimization constants | Fold again | Re-applies constant folding after other passes expose new opportunities |

Pass ordering matters: clear-loop runs first so `[-]` is reduced before multiply-move pattern matching (prevents false matches).

---

## Error Handling

### Error Register

Single global `int bfpp_err`. Value 0 = no error. Set by syscall failures, stack over/underflow, cell-width violations, `e` operator.

### Error Codes

| Code | Name | POSIX errno Sources |
|------|------|---------------------|
| 0 | `OK` | -- |
| 1 | `ERR_GENERIC` | Unmapped errnos |
| 2 | `ERR_NOT_FOUND` | `ENOENT` |
| 3 | `ERR_PERMISSION` | `EACCES`, `EROFS` |
| 4 | `ERR_OOM` | `ENOMEM`, stack overflow |
| 5 | `ERR_CONN_REFUSED` | `ECONNREFUSED` |
| 6 | `ERR_INVALID_ARG` | `EINVAL`, `EBADF`, stack underflow, continuation byte access |
| 7 | `ERR_TIMEOUT` | `ETIMEDOUT` |
| 8 | `ERR_EXISTS` | `EEXIST` |
| 9 | `ERR_BUSY` | `EBUSY`, `EAGAIN` |
| 10 | `ERR_PIPE` | `EPIPE` |
| 11 | `ERR_CONN_RESET` | `ECONNRESET` |
| 12 | `ERR_ADDR_IN_USE` | `EADDRINUSE` |
| 13 | `ERR_NOT_CONNECTED` | `ENOTCONN` |
| 14 | `ERR_INTERRUPTED` | `EINTR` |
| 15 | `ERR_IO` | `EIO` |
| 16 | `ERR_NOLIB` | FFI: dlopen failed |
| 17 | `ERR_NOSYM` | FFI: dlsym failed |
| 16-255 | -- | Reserved for future standard use |
| 256+ | -- | User-defined |

### R{}/K{} Implementation

Generated C uses `do { ... } while(0)` for the R block. `?` inside R emits `break`, transferring control to the K block. Error register is saved/restored across nesting so R/K blocks compose correctly:

```c
{
    int saved_err = bfpp_err;
    bfpp_err = BFPP_OK;
    do {
        /* R body -- ? emits: if (bfpp_err) break; */
    } while(0);
    if (bfpp_err) {
        /* K body */
    }
    if (!bfpp_err) bfpp_err = saved_err;
}
```

---

## Generated C Runtime

The codegen emits a single self-contained `.c` file. No external runtime library. Structure:

```
[#includes: stdio, stdlib, string, stdint, errno, unistd, fcntl, socket, syscall]
[(conditional) termios, sys/ioctl.h  -- if __term_* intrinsics used]
[(conditional) time.h               -- if __sleep/__time_ms used]
[(conditional) poll.h               -- if __poll_stdin used]
[(conditional) dlfcn.h              -- if FFI used]
[(conditional) SDL2/SDL.h           -- if framebuffer enabled]
[(conditional) bfpp_rt.h            -- if __tui_* intrinsics used]
[#defines: TAPE_SIZE, TAPE_MASK, STACK_SIZE, CALL_DEPTH, BFPP_ERR_* codes, (FB dims)]
[Static globals: tape[], ptr, bfpp_err, stack[], sp, bfpp_call_depth, cell_width[]]
[Helper functions: bfpp_get/set (cell-width-aware), bfpp_push/pop, bfpp_cycle_width, bfpp_set_width]
[errno -> BFPP_ERR mapping: bfpp_errno_to_code()]
[Syscall wrapper: bfpp_syscall_exec() -- reads 7 cells, issues syscall, maps errno]
[Constructor: bfpp_init() -- memset tape/cell_width/stack via __attribute__((constructor))]
[(conditional) terminal state: bfpp_saved_termios, bfpp_term_raw flag]
[(conditional) SDL2 framebuffer: bfpp_fb_init/flush/cleanup]
[Forward declarations: void bfpp_sub_NAME(void)]
[Subroutine bodies: each with call-depth guard (prologue/epilogue)]
[int main(void) { ... }  -- intrinsic calls emitted inline]
```

Key runtime properties:
- **Tape wrapping**: `ptr = (ptr + N) & TAPE_MASK` -- bitmask, not modulo (requires power-of-2 tape size)
- **Cell width**: parallel `cell_width[]` array. `bfpp_get`/`bfpp_set` use `memcpy` for safe unaligned multi-byte access. Width 0 = continuation byte (accessing it sets `ERR_INVALID_ARG`)
- **Call depth**: each subroutine entry increments `bfpp_call_depth`, checks against `CALL_DEPTH`, and decrements on exit/return. Overflow aborts
- **Subroutine names**: mangled for C compatibility (`>` -> `gt`, `*` -> `star`, `.` -> `dot`, etc.)
- **CC flags**: `-O2 -Wall -Wno-unused-variable -Wno-unused-function`. Plus `-mavx2 -mfma` on x86_64, `-lSDL2` if framebuffer, `-ldl` if FFI, `-lGL -lGLEW -lm` if 3D intrinsics, `-lEGL` if multi-GPU intrinsics, `-lOpenCL` if GPU compute intrinsics. Parallel compilation: per-subroutine `.c` files compiled concurrently via threaded `cc -c`

---

## Compiler Intrinsics

Subroutine calls with a `__` prefix are intercepted by the compiler and emitted as inline C code instead of BF++ subroutine call/return sequences. No `!#__name{...}` definition is needed -- these are built-in. The codegen detects which intrinsics a program uses and conditionally includes the required C headers (`<termios.h>`, `<sys/ioctl.h>`, `<poll.h>`, `<time.h>`, etc.).

### Terminal Control

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__term_raw` | -- | Enter raw terminal mode (disable echo, canonical mode, signals). Sets `bfpp_err = ERR_IO` on failure |
| `!#__term_restore` | -- | Restore original terminal settings (saved at program start). No-op if not in raw mode |
| `!#__term_size` | -- | `tape[ptr]` = columns, `tape[ptr+1]` = rows. Uses `ioctl(TIOCGWINSZ)` |
| `!#__term_alt_on` | -- | Enter alternate screen buffer (`ESC[?1049h`) |
| `!#__term_alt_off` | -- | Exit alternate screen buffer (`ESC[?1049l`) |
| `!#__term_mouse_on` | -- | Enable mouse tracking (`ESC[?1000h`, SGR mode `ESC[?1006h`) |
| `!#__term_mouse_off` | -- | Disable mouse tracking |

### Time

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__sleep` | `tape[ptr]` = milliseconds | Sleep for N milliseconds (`usleep`) |
| `!#__time_ms` | -- | `tape[ptr]` = monotonic timestamp in milliseconds (`clock_gettime(CLOCK_MONOTONIC)`) |

### Environment and Process

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__getenv` | Null-terminated var name at `tape[ptr]` | Value written at `tape[ptr]` (overwrites name). Sets `ERR_NOT_FOUND` if undefined |
| `!#__exit` | `tape[ptr]` = exit code | `exit(code)` -- terminates the program immediately |
| `!#__getpid` | -- | `tape[ptr]` = current process ID |

### Non-Blocking I/O

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__poll_stdin` | `tape[ptr]` = timeout in ms | `tape[ptr]` = 1 if data available on stdin, 0 on timeout. Uses `poll()` |

### TUI Runtime Intrinsics

These require the C runtime library (`runtime/bfpp_rt.{h,c}`). The compiler auto-detects their usage and links the runtime.

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__tui_init` | -- | Initialize TUI: save termios, enter raw mode, alternate screen, hide cursor. Registers `atexit` cleanup |
| `!#__tui_cleanup` | -- | Restore terminal: show cursor, exit alternate screen, restore termios |
| `!#__tui_size` | -- | `tape[ptr]` = columns, `tape[ptr+1]` = rows |
| `!#__tui_begin` | -- | Begin frame: clear the back buffer for drawing |
| `!#__tui_end` | -- | End frame: diff back buffer against front buffer, emit minimal ANSI updates |
| `!#__tui_put` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = char, `[ptr+3]` = fg, `[ptr+4]` = bg | Write single character to back buffer at (row, col) with colors |
| `!#__tui_puts` | `tape[ptr]` = row, `[ptr+1]` = col, null-terminated string at `ptr+2`, fg/bg after null | Write string to back buffer |
| `!#__tui_fill` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = w, `[ptr+3]` = h, `[ptr+4]` = ch, `[ptr+5]` = fg, `[ptr+6]` = bg | Fill rectangular region in back buffer |
| `!#__tui_box` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = w, `[ptr+3]` = h, `[ptr+4]` = style | Draw box outline with border characters |
| `!#__tui_key` | `tape[ptr]` = timeout in ms | `tape[ptr]` = keycode (ASCII or `BFPP_KEY_*` constants for arrows/special keys). Returns -1 on timeout |

Color values: -1 = terminal default, 0-7 = standard ANSI, 8-15 = bright, 16-231 = 256-color RGB cube, 232-255 = grayscale.

### Threading Intrinsics

Require `-pthread` and `runtime/bfpp_rt_parallel.{h,c}` (auto-linked when any threading intrinsic is detected). Up to 128 threads, 256 mutexes, 64 barriers.

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__spawn` | `tape[ptr]` = subroutine index (0-based definition order), `tape[ptr+8]` = start_ptr | `tape[ptr]` = thread_id (64-bit). Requires `%8` cell width at ptr. Creates a new thread running the indexed subroutine |
| `!#__join` | `tape[ptr]` = thread_id | Blocks until the thread finishes |
| `!#__yield` | -- | `sched_yield()` — yield CPU to other threads |
| `!#__thread_id` | -- | `tape[ptr]` = thread index (0 = main, 1+ = spawned order) |
| `!#__num_cores` | -- | `tape[ptr]` = number of available CPU cores |
| `!#__mutex_init` | `tape[ptr]` = mutex_id (0-255) | Initialize mutex (also auto-inits on first lock) |
| `!#__mutex_lock` | `tape[ptr]` = mutex_id | Lock mutex (blocks if held by another thread) |
| `!#__mutex_unlock` | `tape[ptr]` = mutex_id | Unlock mutex |
| `!#__atomic_load` | `tape[ptr]` = address | `tape[ptr]` = value at address (atomic, width-aware) |
| `!#__atomic_store` | `tape[ptr]` = value, `tape[ptr+1]` = address | Atomically store value at address |
| `!#__atomic_add` | `tape[ptr]` = value, `tape[ptr+1]` = address | `tape[ptr]` = old value. Atomically adds value to address |
| `!#__atomic_cas` | `tape[ptr]` = expected, `tape[ptr+1]` = desired, `tape[ptr+2]` = address | `tape[ptr]` = 1 if swapped, 0 if failed |
| `!#__barrier_init` | `tape[ptr]` = barrier_id (0-63), `tape[ptr+1]` = count | Initialize barrier with participant count |
| `!#__barrier_wait` | `tape[ptr]` = barrier_id | Block until all participants arrive |

**Threading model**: Shared tape (`tape[]`) is the communication channel. Per-thread state (`ptr`, `sp`, `bfpp_err`, `bfpp_call_depth`, `cell_width`) is `_Thread_local` — each thread gets its own copy, initialized by the thread entry wrapper. Region-partitioned memory: programmer manages which tape regions each thread owns; atomics for cross-region communication.

### Framebuffer Pipeline Intrinsics

Available when `--framebuffer WxH` is active. The pipeline uses 8 render threads + 1 presenter thread (owns SDL). `F` is non-blocking; the presenter thread renders at vsync cadence.

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__fb_sync` | -- | Block until the next frame is presented to the display (vsync wait point) |
| `!#__fb_pixel_nt` | `tape[ptr]` = x, `[ptr+1]` = y, `[ptr+2]` = r, `[ptr+3]` = g, `[ptr+4]` = b | Non-temporal pixel write to framebuffer (cache-bypassing) |

### 3D Rendering Intrinsics

Available when `--framebuffer WxH` is active and 3D stdlib (`!include "3d.bfpp"`) is used. Requires `libGL`, `libGLEW`, and `-lm`. Falls back to a software rasterizer when OpenGL is unavailable. ~45 intrinsics across 3 tiers:

**Tier 1 -- GL Proxy Intrinsics:** Direct OpenGL 3.3 core profile operations (shader compilation, buffer management, draw calls, uniform setting, texture binding). These map to GL calls and are intercepted by the compiler to emit inline C.

**Tier 2 -- Q16.16 Fixed-Point Math:** Fixed-point arithmetic intrinsics with sin/cos lookup tables. Provides multiply, divide, sin, cos, sqrt, matrix operations -- all in Q16.16 format suitable for tape-based computation without floating point.

**Tier 3 -- Mesh Generators:** Procedural geometry generation (cube, sphere, plane, etc.) that populate vertex/index buffers on the tape.

**Rendering pipeline:** Renders to an offscreen FBO, uses PBO for async readback, writes pixels to `tape[FB_OFFSET]`, then the existing framebuffer pipeline (`F` flush) presents via SDL2.

**Shading:** Blinn-Phong lighting with PCF (Percentage Closer Filtering) shadow mapping.

**Software fallback:** When GL context creation fails, all GL proxy intrinsics dispatch to an edge-function software rasterizer with perspective-correct interpolation. Same API, no code changes needed.

### Multi-GPU Intrinsics

Requires `libEGL` (typically included with NVIDIA drivers; `libegl-dev` on Debian). Provides:

- **EGL device enumeration** for per-GPU GL context creation
- **Split-Frame Rendering (SFR):** Strip-parallel -- each GPU renders horizontal strips
- **Alternate-Frame Rendering (AFR):** GPUs alternate full frames
- **AUTO mode:** Runtime selection based on GPU count and workload
- **GL command recording + replay** across GPU contexts
- **NUMA-aware buffer allocation** and per-GPU thread pinning
- **Frame pacing** with dropout recovery

### Scene Oracle

Lock-free SPSC triple-buffered scene snapshot system:

- CPU publishes scene state at ~1000Hz
- Each GPU independently samples and extrapolates via Rodrigues rotation + linear velocity
- Temporal extrapolation decouples simulation rate from render rate
- Runtime files: `bfpp_rt_3d_oracle.{c,h}`

### SDL Input Intrinsics

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__input_poll` | -- | Poll SDL event queue, update internal input state |
| `!#__input_mouse_pos` | -- | `tape[ptr]` = mouse X, `tape[ptr+1]` = mouse Y |
| `!#__input_key_held` | `tape[ptr]` = SDL scancode | `tape[ptr]` = 1 if key held, 0 if not |

### Texture Intrinsics

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__gl_create_texture` | -- | `tape[ptr]` = new texture ID |
| `!#__gl_texture_data` | `tape[ptr]` = tex ID, `[ptr+1]` = width, `[ptr+2]` = height, `[ptr+3]` = data ptr | Upload pixel data to texture |
| `!#__gl_bind_texture` | `tape[ptr]` = texture ID, `[ptr+1]` = texture unit | Bind texture to unit |
| `!#__gl_delete_texture` | `tape[ptr]` = texture ID | Delete texture |
| `!#__img_load` | Null-terminated BMP path at `tape[ptr]` | Load BMP image via SDL2, `tape[ptr]` = texture ID |

### Self-Hosting Intrinsics

Primitives for writing a BF++ compiler in BF++. These provide efficient operations that would be prohibitively slow in pure BF++.

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__mul` | `tape[ptr]`, `tape[ptr+1]` | `tape[ptr]` = product |
| `!#__div` | `tape[ptr]`, `tape[ptr+1]` | `tape[ptr]` = quotient |
| `!#__mod` | `tape[ptr]`, `tape[ptr+1]` | `tape[ptr]` = remainder |
| `!#__strcmp` | Null-terminated strings at `tape[ptr]` and `tape[ptr+1]` (as addresses) | `tape[ptr]` = 0 if equal, nonzero otherwise |
| `!#__strlen` | Null-terminated string at `tape[ptr]` | `tape[ptr]` = length |
| `!#__strcpy` | Source addr at `tape[ptr]`, dest addr at `tape[ptr+1]` | Copies string |
| `!#__call` | `tape[ptr]` = subroutine index | Indirect subroutine dispatch (call by index) |
| `!#__hashmap_init` | -- | Initialize a hash map, `tape[ptr]` = map handle |
| `!#__hashmap_get` | `tape[ptr]` = map handle, key at `tape[ptr+1]` | `tape[ptr]` = value |
| `!#__hashmap_set` | `tape[ptr]` = map handle, key at `tape[ptr+1]`, value at `tape[ptr+2]` | Insert/update entry |
| `!#__array_insert` | Array addr + index + value | Insert element at index |
| `!#__array_remove` | Array addr + index | Remove element at index |

### GPU Compute Intrinsics (OpenCL)

Available when `__gpu_*` intrinsics are used. Requires OpenCL runtime (`libOpenCL.so`). Programs can offload parallel computation to GPU hardware. The compiler auto-links `runtime/bfpp_rt_opencl.{c,h}` and `-lOpenCL` (via `dlopen`).

| Intrinsic | Input | Output / Effect |
|-----------|-------|-----------------|
| `!#__gpu_init` | -- | Initialize OpenCL context + command queue |
| `!#__gpu_count` | -- | `tape[ptr]` = number of OpenCL-capable GPUs |
| `!#__gpu_memset` | `tape[ptr]` = addr, `[ptr+1]` = value, `[ptr+2]` = count | Fill GPU-side buffer |
| `!#__gpu_memcpy` | `tape[ptr]` = dest, `[ptr+1]` = src, `[ptr+2]` = count | Copy between tape and GPU memory |
| `!#__gpu_sort` | `tape[ptr]` = addr, `[ptr+1]` = count | GPU-accelerated parallel sort |
| `!#__gpu_reduce` | `tape[ptr]` = addr, `[ptr+1]` = count, `[ptr+2]` = op | Parallel reduction (sum, min, max) |
| `!#__gpu_transform` | `tape[ptr]` = addr, `[ptr+1]` = count, `[ptr+2]` = op | Per-element transform kernel |
| `!#__gpu_rasterize` | Rasterization params from tape | GPU-accelerated rasterization |
| `!#__gpu_blur` | `tape[ptr]` = addr, `[ptr+1]` = w, `[ptr+2]` = h, `[ptr+3]` = radius | GPU box blur |
| `!#__gpu_poll` | -- | `tape[ptr]` = 1 if last async op completed, 0 if pending |
| `!#__gpu_wait` | -- | Block until pending GPU operations complete |
| `!#__gpu_dispatch` | `tape[ptr]` = kernel_id, params from tape | Dispatch a custom OpenCL kernel |

---

## TUI Runtime Library

`runtime/bfpp_rt.{h,c}` -- a C runtime library providing double-buffered terminal rendering. Compiled and linked automatically when any `__tui_*` intrinsic is used.

### Architecture

- **Double-buffered**: maintains `front[]` and `back[]` cell arrays. `begin_frame` clears the back buffer. Drawing primitives write to `back[]`. `end_frame` diffs against `front[]` and emits only changed cells as ANSI escape sequences, then swaps.
- **Cell format**: each cell stores a UTF-8 character (up to 4 bytes), foreground color, and background color. Box-drawing characters (3-byte UTF-8) occupy one terminal column.
- **Cursor optimization**: if the next changed cell is adjacent (same row, col+1), the cursor move sequence is omitted -- the terminal auto-advances. Reduces output by ~80% on typical screens.
- **Input handling**: `poll_key` decodes ANSI escape sequences for arrow keys, Home/End, PgUp/PgDn, Delete into `BFPP_KEY_*` constants (1000+offset). Regular keys return their ASCII value.

### Key Constants

| Constant | Value | Key |
|----------|-------|-----|
| `BFPP_KEY_UP` | 1000 | Up arrow |
| `BFPP_KEY_DOWN` | 1001 | Down arrow |
| `BFPP_KEY_RIGHT` | 1002 | Right arrow |
| `BFPP_KEY_LEFT` | 1003 | Left arrow |
| `BFPP_KEY_HOME` | 1004 | Home |
| `BFPP_KEY_END` | 1005 | End |
| `BFPP_KEY_PGUP` | 1006 | Page Up |
| `BFPP_KEY_PGDN` | 1007 | Page Down |
| `BFPP_KEY_DEL` | 1008 | Delete |

---

## Testing

114 unit tests + 23 integration tests, all passing. Zero clippy warnings.

### Unit Tests

```sh
cargo test
```

Tests in each module: lexer (token emission for all operator classes, line comments, block comments, nested block comments, strings, hex escapes, fd specs, FFI, numeric literals, direct cell width), parser (coalescing, nesting, bracket matching, deref, R/K pairing), analyzer (undefined subs, duplicates), optimizer (clear-loop, scan-loop, multiply-move, error-folding), codegen (hello world generation, sub codegen, error handling codegen, name mangling, tape addr, framebuffer, FFI, intrinsics -- sleep, exit, tui, terminal), preprocessor (no-op, include resolution, cycle detection, escape handling, string-interior includes).

### Integration Tests

```sh
# Requires: cargo build --release
./tests/integration/test_runner.sh [path-to-bfpp-binary]
```

Shell-based runner. Compiles `.bfpp` sources, runs binaries, compares stdout against expected output files. Tests:

| Test | What it validates |
|------|-------------------|
| `hello_classic` | Classic BF compatibility (original hello world) |
| `hello_bfpp` | String literals + subroutines |
| `error_handling` | R/K blocks, `?` propagation, error codes |
| `tape_addr` | T operator (push tape address to stack) |
| `include` | `!include` directive expansion |
| `stdlib_math` | `!#m*` multiply |
| `stdlib_io` | `!#.>` print_string |
| `stdlib_string` | String module loading |
| `ffi` | `\ffi` dlopen/dlsym call to libc |
| `classic_bf/*` | Any additional classic BF programs in subdirectory |

---

## Project Structure

```
bfpp/
  Cargo.toml              -- crate metadata: clap, rayon, opencl3 (optional)
  src/
    main.rs               -- CLI (clap derive), compilation pipeline orchestration, parallel CC
    ast.rs                -- AstNode enum, FdSpec, Program struct
    lexer.rs              -- Single-pass tokenizer, string/fd/sub/FFI parsers
    parser.rs             -- Recursive descent, coalescing, R/K pairing
    analyzer.rs           -- 4-pass semantic validation (passes 2+4 parallel via rayon)
    optimizer.rs          -- 12 peephole passes (clear, scan, multiply-move, error-fold, etc.)
    codegen.rs            -- AST -> C source, runtime emission, name mangling (parallel emission)
    error_codes.rs        -- Error code constants (Rust), errno mapping (C source)
    preprocess.rs         -- !include expansion, path resolution, cycle detection
    gpu.rs                -- OpenCL GPU-accelerated lexing + pattern detection (optional, --features gpu)
  stdlib/
    io.bfpp               -- print_string, print_int, read_line, read_int
    math.bfpp             -- multiply, divide, modulo, power
    file.bfpp             -- open, read, write, close (syscall wrappers)
    net.bfpp              -- TCP socket, listen, accept, send, recv
    string.bfpp           -- strlen, strcmp (stub), strcpy (stub), strcat (adjacent only)
    mem.bfpp              -- memcpy (stub), memset (stub), malloc (stub), free (no-op)
    tui.bfpp              -- cursor_move, clear, set_color, draw_box
    err.bfpp              -- err_to_string, panic, assert
    graphics.bfpp         -- SDL2 framebuffer: set_pixel, get_pixel, clear_fb, fill_rect, draw_hline
    3d.bfpp               -- 3D rendering: ~45 intrinsics (GL proxy, Q16.16 math, mesh generators)
    math3d.bfpp           -- Pure BF++ 3D math (585 lines: vectors, matrices, transforms)
  bootstrap/
    bfpp_self.bfpp        -- Self-hosting BF++ compiler (main driver)
    parse_num.bfpp        -- Numeric literal parser for bootstrap compiler
    parse_str.bfpp        -- String literal parser for bootstrap compiler
    parse_sub.bfpp        -- Subroutine definition/call parser for bootstrap compiler
  spec/
    BFPP_SPEC.md          -- Full language specification
    ERROR_CODES.md        -- Error code table and errno mapping
    MEMORY_MAP.md         -- Tape region layout (general, syscall, I/O, framebuffer)
    STDLIB_REFERENCE.md   -- Stdlib function signatures and calling conventions
    EXAMPLES.md           -- Usage examples
  examples/
    hello.bfpp            -- Classic BF hello world
    hello_bfpp.bfpp       -- BF++ string literal + subroutine hello
    cat.bfpp              -- stdin -> stdout (,[.,])
    error_handling.bfpp   -- R/K demo with error propagation
    framebuffer_demo.bfpp -- SDL2 framebuffer example
    tui_demo.bfpp         -- ANSI terminal UI demo
    3d_demo.bfpp          -- 3D rendering demo (OpenGL + software fallback)
    editor.bfpp           -- Terminal text editor (TUI intrinsics, multicore save)
    thread_test.bfpp      -- Threading intrinsics demo
    intrinsics_demo.bfpp  -- Compiler intrinsics demo (getenv, getpid, time, sleep)
  tests/
    integration/
      test_runner.sh      -- Integration test harness
      expected_*.txt      -- Expected output files (22 files)
      test_*.bfpp         -- Test source files (23 programs)
      classic_bf/         -- Classic BF compatibility tests
  benchmarks/             -- Performance benchmarks
  runtime/
    bfpp_rt.h             -- TUI runtime header (API, key constants, color defines)
    bfpp_rt.c             -- TUI runtime impl (double-buffered renderer, input decoder)
    bfpp_rt_3d.h          -- 3D rendering header (GL proxy, software rasterizer API)
    bfpp_rt_3d.c          -- 3D rendering impl (FBO, PBO readback, Blinn-Phong, PCF shadows)
    bfpp_rt_3d_shaders.h  -- Embedded GLSL shaders (Blinn-Phong + PCF shadows)
    bfpp_rt_3d_math.c     -- Q16.16 fixed-point math (sin/cos LUT, 4x4 matrices)
    bfpp_rt_3d_meshgen.c  -- Mesh generators (cube, sphere, torus, plane, cylinder)
    bfpp_rt_3d_software.c -- Software rasterizer fallback (edge-function, AVX2 + SSE SIMD)
    bfpp_rt_3d_multigpu.h -- Multi-GPU header (EGL contexts, SFR/AFR/AUTO, command replay)
    bfpp_rt_3d_multigpu.c -- Multi-GPU impl (NUMA-aware alloc, thread pinning, frame pacing)
    bfpp_rt_3d_oracle.h   -- Scene oracle header (lock-free SPSC triple buffer)
    bfpp_rt_3d_oracle.c   -- Scene oracle impl (temporal extrapolation, Rodrigues rotation)
    bfpp_fb_pipeline.h    -- Framebuffer pipeline header (render threads, presenter)
    bfpp_fb_pipeline.c    -- Framebuffer pipeline impl (8 render threads, vsync presenter)
    bfpp_fb_terminal.h    -- Terminal framebuffer header (headless/SSH rendering)
    bfpp_fb_terminal.c    -- Terminal framebuffer impl (true-color ANSI, delta encoding, adaptive fps)
    bfpp_rt_opencl.h      -- OpenCL GPU compute header (12 compute intrinsics)
    bfpp_rt_opencl.c      -- OpenCL GPU compute impl (kernel dispatch, memory management)
    bfpp_rt_opencl_kernels.h -- Embedded OpenCL kernel source strings
    bfpp_rt_parallel.h    -- Threading runtime header (spawn, join, mutex, barrier, atomics)
    bfpp_rt_parallel.c    -- Threading runtime impl (up to 128 threads, 256 mutexes)
```

---

## Memory Map (Default 64KB Tape)

| Region | Address Range | Size | Purpose |
|--------|---------------|------|---------|
| General purpose | `0x0000`-`0x7FFF` | 32 KB | User data, strings, computation. Pointer starts at 0 |
| Syscall params | `0x8000`-`0x80FF` | 256 B | Syscall number + 6 args (8 bytes each) + scratch |
| I/O buffer | `0x8100`-`0x8FFF` | 3840 B | Stdlib file/net read/write staging |
| Reserved | `0x9000`-`0x9FFF` | 4 KB | Future use (heap metadata, etc.) |
| Framebuffer | `0xA000`-`0xFFFF` | 24 KB | RGB888 pixel data (when `--framebuffer` enabled) |

Data stack (4096 x 64-bit entries), call stack (256 frames), and cell-width metadata array are separate from the tape.

---

## Known Limitations

- **Stdlib multi-address operations**: `strcmp`, `strcpy`, `memcpy`, `memset`, `malloc` are stubs or severely constrained. BF's single-pointer architecture makes operations requiring two independent memory positions fundamentally difficult. `@` is a one-way jump that destroys positional context, and stack LIFO ordering prevents interleaving return addresses with data.
- **8-bit default cells**: Syscall args, addresses, and heap operations require wider cells (`%4` for 32-bit or `%8` for 64-bit). Large values are set with `#N` (e.g., `%4 #36864`), eliminating the need for repeated `+` increments.
- **Platform-specific syscall numbers**: `file.bfpp` and `net.bfpp` use Linux x86_64 syscall numbers (read=0, write=1, open=2, close=3, socket=41, etc.).
- **Framebuffer resolution**: Bounded by tape size. Default 64KB tape supports ~90x90 pixels max. Increase with `--tape-size`.
- **TUI single-digit coordinates**: `!#cm` cursor_move supports row/col 1-9 only.
