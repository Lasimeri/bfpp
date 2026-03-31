# BF++

A Brainfuck superset transpiler that compiles to C, adding syscalls, subroutines, error handling, bitwise ops, stack operations, FFI, and an optional SDL2 framebuffer. Written in Rust. Produces self-contained, single-file C programs with an embedded runtime.

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
| `F` | Framebuffer flush | Flush tape framebuffer region to SDL2 window. No-op if framebuffer not enabled |

### FFI

| Symbol | Name | Semantics |
|--------|------|-----------|
| `\ffi "lib" "func"` | Foreign call | `dlopen(lib)` -> `dlsym(func)` -> call with 6 args from tape layout (same as syscall). Result to `tape[ptr]`. Sets `ERR_NOLIB`/`ERR_NOSYM` on failure |

### Other

| Symbol | Name | Semantics |
|--------|------|-----------|
| `;` | Comment | Line comment (to end of line) |
| `!include "file"` | Include | Preprocessor directive: splice file contents into source before lexing |

---

## Architecture

Compilation pipeline:

```
source.bfpp
    |
    v
[Preprocess] -- expand !include directives, cycle detection, resolve search paths
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
| Preprocess | `preprocess.rs` | Line-by-line `!include` expansion. Resolves relative to source dir, then `--include` paths, then `./stdlib/`, then exe-adjacent `stdlib/`. Cycle detection via canonical path HashSet. Max depth 64 |
| Lex | `lexer.rs` | Peek-based character dispatcher. Multi-char tokens (strings, subroutines, fd specs, FFI) consume inline. Backslash lookahead cloning for `\ffi` vs `\` disambiguation |
| Parse | `parser.rs` | `parse_block`/`parse_single` recursive descent. `BlockEnd` enum tracks context (`]` vs `}` vs EOF). Consecutive movement/arithmetic tokens coalesced via `count_consecutive`. `*` recursively wraps the next single op. `R{...}K{...}` pairing enforced here |
| Analyze | `analyzer.rs` | Four passes: (1) collect sub defs/calls into HashSets, check for undefined calls; (2) detect duplicate defs with separate `seen` set; (3) warn on top-level `^`; (4) reject empty FFI names |
| Optimize | `optimizer.rs` | Ordered peephole passes: clear-loop -> scan-loop -> multiply-move -> error-folding. Each pass recurses into all block-containing nodes |
| Codegen | `codegen.rs` | Emits C header (includes, defines, runtime state, helper functions, errno mapping, syscall wrapper, constructor, optional SDL2 framebuffer, optional dlfcn), forward-declares subs, emits sub bodies with call-depth guards, then main(). Subroutine names mangled for C identifier compatibility |

---

## Building

```sh
cargo build --release
```

Binary at `target/release/bfpp`. Only dependency: `clap 4` (derive feature).

Runtime requirements for generated programs: a C compiler (gcc/clang), POSIX libc. Optional: SDL2 (framebuffer mode), libdl (FFI mode).

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
| `--framebuffer <WxH>` | none | Enable SDL2 framebuffer with given dimensions (e.g., `80x60`). Links `-lSDL2` |
| `--no-optimize` | false | Disable all optimizer passes |
| `-O <LEVEL>` | 1 | Optimization level: 0=none, 1=basic, 2+=full |
| `--include <PATH>` | none | Additional include search path (repeatable) |
| `--cc <COMPILER>` | `cc` | C compiler command |
| `--eof <VALUE>` | 0 | Value written to cell on EOF during `,` (0 or 255) |

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

### Error Handling

```brainfuck
!#fail{
  ++++++ e   ; set error register to 6 (ERR_INVALID_ARG)
  ^
}

R{
  !#fail     ; call subroutine that sets error
}K{
  E          ; load error code into cell
  ++++++++++++++++++++++++++++++++++++++++++++++++ .  ; add 48 -> ASCII '6', print
}
```

---

## Standard Library

8 modules, all written in BF++. Include via `!include "module.bfpp"` or `--include stdlib/`.

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

### Calling Convention

All stdlib functions follow the same pattern:
- **Arguments**: placed in tape cells at current `ptr` before call
- **Return value**: left in `tape[ptr]` after return
- **Errors**: set via error register; callers use `?` or `R{...}K{...}`
- **Workspace**: functions document which cells relative to ptr they clobber

---

## Optimization

Three levels controlled by `-O` flag (overridden by `--no-optimize`):

| Level | Flag | Passes | Description |
|-------|------|--------|-------------|
| None | `-O0` | -- | AST passes through unchanged |
| Basic | `-O1` | clear-loop, error-folding | `[-]`/`[+]` -> `Clear` (cell = 0). Consecutive `?` collapsed to one |
| Full | `-O2` | clear-loop, scan-loop, multiply-move, error-folding | Adds: `[>]` -> `ScanRight`, `[<]` -> `ScanLeft`. `[->>+++<<]` patterns -> `MultiplyMove` (straight-line arithmetic instead of O(n) loop) |

### Optimizer Details

| Pass | Pattern | Replacement | Speedup |
|------|---------|-------------|---------|
| Clear loop | `[-]` or `[+]` | `tape[ptr] = 0` | Eliminates loop (up to 255 iterations -> 1 assignment) |
| Scan loop | `[>]` or `[<]` | `while(tape[ptr]) ptr++/--` | Semantically equivalent but enables future memchr optimization |
| Multiply-move | `[->>+++<<]` | `tape[ptr+2] += tape[ptr]*3; tape[ptr]=0` | O(N*M) loop -> O(M) straight-line. Detects balanced decrement-move-increment patterns. Merges duplicate target offsets |
| Error folding | `???` | `?` | Consecutive propagate checks are redundant. N-1 branch instructions eliminated |

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
[#includes: stdio, stdlib, string, stdint, errno, unistd, fcntl, socket, syscall, (dlfcn), (SDL2)]
[#defines: TAPE_SIZE, TAPE_MASK, STACK_SIZE, CALL_DEPTH, BFPP_ERR_* codes, (FB dims)]
[Static globals: tape[], ptr, bfpp_err, stack[], sp, bfpp_call_depth, cell_width[]]
[Helper functions: bfpp_get/set (cell-width-aware), bfpp_push/pop, bfpp_cycle_width]
[errno -> BFPP_ERR mapping: bfpp_errno_to_code()]
[Syscall wrapper: bfpp_syscall_exec() -- reads 7 cells, issues syscall, maps errno]
[Constructor: bfpp_init() -- memset tape/cell_width/stack via __attribute__((constructor))]
[(SDL2 framebuffer: bfpp_fb_init/flush/cleanup)]
[Forward declarations: void bfpp_sub_NAME(void)]
[Subroutine bodies: each with call-depth guard (prologue/epilogue)]
[int main(void) { ... }]
```

Key runtime properties:
- **Tape wrapping**: `ptr = (ptr + N) & TAPE_MASK` -- bitmask, not modulo (requires power-of-2 tape size)
- **Cell width**: parallel `cell_width[]` array. `bfpp_get`/`bfpp_set` use `memcpy` for safe unaligned multi-byte access. Width 0 = continuation byte (accessing it sets `ERR_INVALID_ARG`)
- **Call depth**: each subroutine entry increments `bfpp_call_depth`, checks against `CALL_DEPTH`, and decrements on exit/return. Overflow aborts
- **Subroutine names**: mangled for C compatibility (`>` -> `gt`, `*` -> `star`, `.` -> `dot`, etc.)
- **CC flags**: `-O2 -Wall -Wno-unused-variable -Wno-unused-function`. Plus `-lSDL2` if framebuffer, `-ldl` if FFI

---

## Testing

### Unit Tests

```sh
cargo test
```

Tests in each module: lexer (token emission for all operator classes, comments, strings, hex escapes, fd specs, FFI), parser (coalescing, nesting, bracket matching, deref, R/K pairing), analyzer (undefined subs, duplicates), optimizer (clear-loop, scan-loop, multiply-move, error-folding), codegen (hello world generation, sub codegen, error handling codegen, name mangling, tape addr, framebuffer, FFI), preprocessor (no-op, include resolution, cycle detection, escape handling, string-interior includes).

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
  Cargo.toml              -- crate metadata, clap dependency
  src/
    main.rs               -- CLI (clap derive), compilation pipeline orchestration
    ast.rs                -- AstNode enum, FdSpec, Program struct
    lexer.rs              -- Single-pass tokenizer, string/fd/sub/FFI parsers
    parser.rs             -- Recursive descent, coalescing, R/K pairing
    analyzer.rs           -- 4-pass semantic validation
    optimizer.rs          -- Peephole passes (clear, scan, multiply-move, error-fold)
    codegen.rs            -- AST -> C source, runtime emission, name mangling
    error_codes.rs        -- Error code constants (Rust), errno mapping (C source)
    preprocess.rs         -- !include expansion, path resolution, cycle detection
  stdlib/
    io.bfpp               -- print_string, print_int, read_line, read_int
    math.bfpp             -- multiply, divide, modulo, power
    file.bfpp             -- open, read, write, close (syscall wrappers)
    net.bfpp              -- TCP socket, listen, accept, send, recv
    string.bfpp           -- strlen, strcmp (stub), strcpy (stub), strcat (adjacent only)
    mem.bfpp              -- memcpy (stub), memset (stub), malloc (stub), free (no-op)
    tui.bfpp              -- cursor_move, clear, set_color, draw_box
    err.bfpp              -- err_to_string, panic, assert
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
  tests/
    integration/
      test_runner.sh      -- Integration test harness
      expected_*.txt      -- Expected output files
      test_*.bfpp         -- Test source files
      classic_bf/         -- Classic BF compatibility tests
  benchmarks/             -- Performance benchmarks
  runtime/                -- Runtime support files
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
- **8-bit default cells**: Syscall args, addresses, and heap operations require 64-bit cells (`%%%` to cycle width three times). Setting large values (>255) in cells requires repeated `+` or string literal tricks.
- **Platform-specific syscall numbers**: `file.bfpp` and `net.bfpp` use Linux x86_64 syscall numbers (read=0, write=1, open=2, close=3, socket=41, etc.).
- **Framebuffer resolution**: Bounded by tape size. Default 64KB tape supports ~90x90 pixels max. Increase with `--tape-size`.
- **TUI single-digit coordinates**: `!#cm` cursor_move supports row/col 1-9 only.
