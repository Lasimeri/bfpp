# BF++ Language Specification

**Version**: 0.1.0
**Status**: Draft
**Date**: 2026-02-13

---

## 1. Overview

BF++ is a Brainfuck-derived language that retains symbolic minimalism while adding operators for system calls, file I/O, networking, error handling, and subroutines. Programs are transpiled to C via a Rust-based compiler, then compiled to native binaries via gcc/clang.

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

Standard BF loops (`[`/`]`) remain the primary control flow mechanism. Combined with subroutines and error propagation, they provide sufficient control flow for systems programming.

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
