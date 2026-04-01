# BF++

A Brainfuck superset that compiles to native binaries via C11. Full backward compatibility with BF, extended with 30+ operators for systems programming.

```
BF++ source → Rust compiler → C11 → cc → native ELF
```

```brainfuck
; Hello world using BF++ string literals + subroutines
!#pr{ [.>] ^ }
"Hello, World!\n\0"
<<<<<<<<<<<<<<<
!#pr
```

## Quick Start

```sh
cargo build --release
./target/release/bfpp examples/hello_bfpp.bfpp -o hello && ./hello
```

## What BF++ Adds

| Category | Operators | Description |
|----------|-----------|-------------|
| **Data** | `#N` `"..."` `$ ~` `%` `@` `*` | Numeric/string literals, stack, cell width, addressing |
| **Subroutines** | `!#name{...}` `!#name` `^` | Define, call, return |
| **Error handling** | `E e ? R{...}K{...}` | Error register, propagation, try/catch |
| **Bitwise** | `\| & x s r n` | OR, AND, XOR, shift left/right, NOT |
| **I/O** | `.{N}` `,{N}` `\` `\ffi` | File descriptor I/O, syscalls, FFI |
| **Multicore** | `{ } ( ) P Q V` | Dual-tape architecture for parallel transforms |
| **Control** | `?= ?< ?> ?!` `?{...}:{...}` | Non-destructive comparison, if/else |
| **Preprocessor** | `!include` `!define` `!undef` | File inclusion, text macros |

Every classic BF program runs unmodified.

## Features

- **12-pass peephole optimizer** — clear-loop, scan, multiply-move, constant fold, loop unroll, move coalescing, auto-parallelism, GPU loop detection
- **Auto-parallelism** — detects independent loop iterations, dispatches across CPU cores via `bfpp_parallel_for`
- **OpenCL transpiler** — data-parallel loops auto-compiled to GPU kernels with CPU fallback
- **Parallel compilation** — per-subroutine `.c` files compiled concurrently, precompiled headers, LTO
- **3D rendering** — OpenGL 3.3 + software rasterizer fallback, multi-GPU (EGL SFR/AFR), scene oracle
- **TUI runtime** — double-buffered terminal rendering with diff-based ANSI output
- **Threading** — `__spawn`/`__join`, mutexes, barriers, atomics (up to 128 threads)
- **90+ compiler intrinsics** — terminal, TUI, time, threading, 3D, multi-GPU, GPU compute, self-hosting
- **Self-hosting bootstrap** — BF++ compiler written in BF++

## Usage

```sh
bfpp input.bfpp -o output              # compile
bfpp input.bfpp --emit-c               # emit C source
bfpp input.bfpp -O2 --tape-size 131072 # full optimization, 128KB tape
bfpp input.bfpp --framebuffer 640x480  # SDL2 graphics + 3D support
bfpp input.bfpp --include stdlib/      # use standard library
```

| Flag | Default | Description |
|------|---------|-------------|
| `-o <FILE>` | input stem | Output binary |
| `--emit-c` | | Write C source instead of compiling |
| `--tape-size <N>` | 65536 | Tape size (power of 2) |
| `--stack-size <N>` | 4096 | Data stack entries |
| `--call-depth <N>` | 256 | Max recursion depth |
| `--framebuffer <WxH>` | | Enable SDL2 graphics |
| `-O <LEVEL>` | 1 | Optimization: 0/1/2 |
| `--include <PATH>` | | Include search path |
| `--cc <COMPILER>` | cc | C compiler |
| `--eof <VALUE>` | 0 | EOF cell value (0 or 255) |
| `--watch` | | Auto-recompile on change |

## Examples

### Error Handling

```brainfuck
!#fail{ #6 e ^ }         ; set error code 6

R{
  !#fail                  ; call — sets error
  ?                       ; propagate (like Rust's ?)
}K{
  E .                     ; catch: read error code, print
}
```

### Numeric Literals + Cell Width

```brainfuck
%4              ; 32-bit cells
#36864          ; tape[ptr] = 36864
#0x48 .         ; print 'H'
```

### Threading

```brainfuck
!#worker{
  !#__thread_id           ; get thread index
  #48 > [->+<] > .       ; print as ASCII digit
  ^
}
%8 #0 > #0 <              ; sub index 0, start_ptr 0
!#__spawn                  ; spawn thread
!#__join                   ; wait for completion
```

## Architecture

```
source.bfpp
  → Preprocess (!include, !define)
  → Lex (46 token types)
  → Parse (recursive descent, coalescing)
  → Analyze (4 semantic passes, rayon-parallel)
  → Optimize (13 passes, per-sub rayon parallel)
  → Codegen (C11, parallel sub emission, intrinsic detection)
  → CC (parallel per-TU compilation, PCH, LTO)
```

## Standard Library

11 modules in `stdlib/`, all written in BF++:

| Module | Status | Key Functions |
|--------|--------|---------------|
| `io` | Working | `!#.>` print_string, `!#.+` print_int |
| `math` | Working | `!#m*` multiply, `!#m/` divide, `!#m%` modulo |
| `err` | Working | `!#es` err_to_string, `!#ep` panic, `!#ea` assert |
| `graphics` | Working | Pixel ops, fill, hline (SDL2 framebuffer) |
| `3d` | Working | ~45 intrinsics: GL proxy, Q16.16 math, mesh generators |
| `math3d` | Working | 585 lines of pure BF++ vector/matrix math |
| `tui` | Partial | Cursor, clear, color, box drawing |
| `file` | Partial | Syscall wrappers (Linux x86_64) |
| `net` | Partial | TCP socket wrappers |
| `string` | Stub | Limited by BF memory model |
| `mem` | Stub | Limited by 8-bit addressing |

## Runtime Subsystems

Auto-detected and auto-linked based on intrinsic usage:

| Subsystem | Files | Trigger |
|-----------|-------|---------|
| TUI | `bfpp_rt.c/h` | `__tui_*` intrinsics |
| Threading | `bfpp_rt_parallel.c/h` | `__spawn`, `__mutex_*`, etc. |
| Framebuffer | `bfpp_fb_pipeline.c/h` | `--framebuffer` flag |
| 3D Rendering | `bfpp_rt_3d*.c/h` (6 files) | `__gl_*`, `__fp_*`, `__mesh_*` |
| Multi-GPU | `bfpp_rt_3d_multigpu.c/h` | `__gl_multi_gpu` |
| Scene Oracle | `bfpp_rt_3d_oracle.c/h` | `__scene_*` |
| GPU Compute | `bfpp_rt_opencl.c/h` | `__gpu_*` |
| Compression | `bfpp_rt_compress.c/h` | `__net_send_compressed`, `__tape_save`, etc. |
| Terminal FB | `bfpp_fb_terminal.c/h` | `--framebuffer` without display server |

## Building

```sh
cargo build --release

# With GPU-accelerated compilation (optional):
cargo build --release --features gpu
```

Dependencies: `clap 4`, `rayon 1`. Optional: `opencl3 0.9` (GPU-accelerated lexing), `zstd 0.13` (compressed includes).

## Testing

```sh
cargo test                                    # 125 unit tests
./tests/integration/test_runner.sh            # 23 integration tests
```

## Documentation

Detailed documentation in `spec/`:

- [`BFPP_SPEC.md`](spec/BFPP_SPEC.md) — Full language specification
- [`ERROR_CODES.md`](spec/ERROR_CODES.md) — Error codes and errno mapping
- [`STDLIB_REFERENCE.md`](spec/STDLIB_REFERENCE.md) — Standard library reference
- [`MEMORY_MAP.md`](spec/MEMORY_MAP.md) — Tape layout
- [`EXAMPLES.md`](spec/EXAMPLES.md) — Usage examples

## License

MIT
