# BF++ Usage Guide

Comprehensive reference for the BF++ compiler (`bfpp`) and the standalone BF interpreter (`bf-interpreter`).

---

## Part 1: BF++ Compiler (bfpp)

### Installation

BF++ is a Rust project that transpiles BF++ source to C, then invokes a C compiler to produce a native binary. Requirements:

- Rust toolchain (cargo, rustc)
- A C compiler (`cc`, `gcc`, or `clang`) on PATH
- SDL2 development libraries (only if using `--framebuffer`)

Build from source:

```sh
cd bfpp
cargo build --release
```

The binary is at `target/release/bfpp`. Optionally add it to PATH:

```sh
cp target/release/bfpp ~/.local/bin/
```

---

### Basic Usage

Compile a `.bfpp` file to a native binary:

```sh
bfpp input.bfpp
```

This produces an executable named `input` (derived from the input filename stem).

Specify the output binary name:

```sh
bfpp input.bfpp -o myprogram
```

Emit the generated C source instead of compiling:

```sh
bfpp input.bfpp --emit-c
```

This writes `input.c` to disk (or `myprogram.c` if `-o myprogram` is given) and exits without invoking the C compiler. Useful for inspecting the generated code or cross-compiling manually.

Run the compiled binary:

```sh
./input
```

---

### Writing BF++ Programs

BF++ source files use `.bfpp` extension. The language is a strict superset of Brainfuck: all valid BF programs run unmodified. Non-instruction characters (except inside strings) are silently ignored, serving as inline comments. Line comments start with `;`.

```bfpp
+++ ; this is a comment
```

---

#### 1. Core BF Operations

The original 8 Brainfuck instructions:

| Op  | Name       | Semantics                                     |
|-----|------------|-----------------------------------------------|
| `>` | Move right | `ptr += 1`                                    |
| `<` | Move left  | `ptr -= 1`                                    |
| `+` | Increment  | `tape[ptr] += 1` (wraps at 255)               |
| `-` | Decrement  | `tape[ptr] -= 1` (wraps at 0)                 |
| `.` | Output     | Write `tape[ptr]` as byte to stdout            |
| `,` | Input      | Read one byte from stdin into `tape[ptr]`      |
| `[` | Loop start | If `tape[ptr] == 0`, jump past matching `]`    |
| `]` | Loop end   | If `tape[ptr] != 0`, jump back to matching `[` |

Brackets must be balanced. Unmatched brackets are a compile-time error.

**Hello World (classic BF):**

```bfpp
; Classic BF hello world -- works unmodified in BF++
++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.
```

**Cat program (echo stdin to stdout):**

```bfpp
,[.,]
```

---

#### 2. String Literals

`"..."` writes ASCII bytes sequentially to the tape starting at the current pointer, advancing the pointer past the last byte written.

Supported escape sequences:

| Escape | Meaning            |
|--------|--------------------|
| `\0`   | Null byte (0x00)   |
| `\n`   | Newline (0x0A)     |
| `\r`   | Carriage return    |
| `\t`   | Tab (0x09)         |
| `\\`   | Literal backslash  |
| `\"`   | Literal quote      |
| `\xHH` | Hex byte value     |

**Hello World with string literals:**

```bfpp
!#pr{
  [.>]    ; print bytes until null
  ^
}

"Hello, World!\n\0"
<<<<<<<<<<<<<<<
!#pr
```

How it works:
1. The string literal writes 15 bytes (`Hello, World!\n\0`) to cells 0-14, leaving ptr at cell 15.
2. `<<<<<<<<<<<<<<<` moves ptr back to cell 0 (the start of the string).
3. `!#pr` calls the subroutine, which outputs each byte until hitting the null terminator.

**Hex escapes for arbitrary byte values:**

```bfpp
"\x41\x42\x43"    ; writes bytes 0x41, 0x42, 0x43 (ABC)
```

Multi-line strings are allowed -- embedded newlines become `\n` bytes in the output.

---

#### 3. Subroutines

**Definition:** `!#name{ body ^ }`

```bfpp
!#pr{
  [.>]    ; print bytes until null
  ^       ; return
}
```

**Call:** `!#name`

```bfpp
!#pr    ; calls the subroutine defined above
```

Rules:
- Names begin with `#` followed by alphanumeric characters and/or BF operator symbols (`> < + - . , @ * % $ ~ \ | & ^ _ /`).
- `^` returns from the current subroutine. If the error register is non-zero at return, the error propagates to the caller.
- The call stack is separate from the data stack. Default max depth: 256 frames. Overflow is fatal.
- Recursion is fully supported.

**Calling convention:**
1. Caller places arguments in tape cells at current `ptr` before the call.
2. Subroutine reads arguments from tape, performs work.
3. Subroutine leaves return value in `tape[ptr]`.
4. Subroutine sets error register via `e` if an error occurs, then `^` returns.

**Recursive factorial example:**

```bfpp
!#fac{
  ; if n <= 1, return 1
  $                   ; save n
  -                   ; n-1
  [                   ; if n-1 != 0 (n > 1)
    !#fac             ; factorial(n-1), result in cell
    ~ >               ; pop original n into next cell
    < !#m*            ; multiply: cell[ptr] = (n-1)! * n
    ^                 ; return
  ]
  ; base case: n <= 1
  ~                   ; restore n
  [-] +               ; cell = 1
  ^                   ; return 1
}

; Compute 5!
+++++ !#fac           ; tape[ptr] = 120
```

---

#### 4. Stack Operations

| Op  | Name | Semantics                                         |
|-----|------|---------------------------------------------------|
| `$` | Push | Push `tape[ptr]` onto the auxiliary data stack     |
| `~` | Pop  | Pop top of stack into `tape[ptr]`                  |

The data stack is a separate memory region (default 4,096 entries, each 64 bits wide). LIFO semantics. Stack underflow sets the error register to 6 (ERR_INVALID_ARG). Overflow sets it to 4 (ERR_OOM).

**Saving and restoring values:**

```bfpp
+++++ $               ; push 5 onto stack
[-]                   ; clear cell
+++++++++++ $         ; push 10 onto stack
[-]                   ; clear cell
~ .                   ; pop 10, print as byte
~ .                   ; pop 5, print as byte
```

Common pattern -- preserving a value across a destructive loop:

```bfpp
$                     ; save current cell to stack
[-]                   ; clear cell (or do other destructive work)
~                     ; restore saved value
```

---

#### 5. Extended Memory

| Op  | Name            | Semantics                                                        |
|-----|-----------------|------------------------------------------------------------------|
| `@` | Absolute addr   | `ptr = tape[ptr]` -- jump pointer to address stored in cell      |
| `*` | Dereference     | Next op targets `tape[tape[ptr]]` instead of `tape[ptr]`         |
| `%` | Cell width cycle| Cycle cell bit-width: 8 -> 16 -> 32 -> 64 -> 8                  |
| `T` | Tape addr       | Push `&tape[ptr]` (raw pointer to current cell) onto the stack   |

**Absolute addressing (`@`):**

```bfpp
; Set cell[0] = 5, cell[5] = 65 ('A')
+++++ >>>>>           ; move to cell 5
[-] +++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++  ; cell[5] = 65
<<<<<<                ; back to cell 0
; cell[0] already contains 5
@                     ; ptr = tape[0] = 5 -- now pointing at cell 5
.                     ; prints 'A'
```

**Dereference (`*`):**

`*` is a prefix modifier that applies to the next single operation. It makes that operation target `tape[tape[ptr]]` -- the cell whose address is stored in the current cell.

```bfpp
; tape[0] = 5, tape[5] = 65
*+                    ; increment tape[tape[0]] = tape[5] -> 66
*.                    ; output tape[tape[0]] = tape[5] = 'B'
```

**Cell width cycling (`%`):**

Cells default to 8-bit. `%` cycles the width at the current position: 8 -> 16 -> 32 -> 64 -> 8. Multi-byte cells use little-endian byte order and occupy consecutive tape positions.

```bfpp
%                     ; cell at ptr is now 16-bit (uses 2 bytes)
%%                    ; cell at ptr is now 64-bit (8->16->32)
%%%                   ; 64-bit (8->16->32->64)
```

16-bit cells can hold values 0-65535. 64-bit cells are required for syscall arguments (addresses, large values).

---

#### 6. Error Handling

BF++ has a dedicated error register (`bfpp_err`), a 64-bit value separate from the tape.

| Op       | Name         | Semantics                                                    |
|----------|--------------|--------------------------------------------------------------|
| `E`      | Error read   | `tape[ptr] = bfpp_err` -- copy error register to cell       |
| `e`      | Error write  | `bfpp_err = tape[ptr]` -- set error register from cell      |
| `?`      | Propagate    | If `bfpp_err != 0`, return from current subroutine immediately |
| `R{...}` | Result block | Execute body; if error occurs, jump to matching `K{...}`    |
| `K{...}` | Catch block  | Executes only if preceding `R{...}` produced an error       |

**Error code table:**

| Code | Name               | Description                    | C errno mapping                |
|------|--------------------|--------------------------------|--------------------------------|
| 0    | OK                 | No error                       | --                             |
| 1    | ERR_GENERIC        | Unspecified error               | unmapped errnos                |
| 2    | ERR_NOT_FOUND      | File/resource not found         | ENOENT                         |
| 3    | ERR_PERMISSION     | Permission denied               | EACCES, EROFS                  |
| 4    | ERR_OOM            | Out of memory / stack overflow  | ENOMEM                         |
| 5    | ERR_CONN_REFUSED   | Connection refused              | ECONNREFUSED                   |
| 6    | ERR_INVALID_ARG    | Invalid argument                | EINVAL, EBADF, stack underflow |
| 7    | ERR_TIMEOUT        | Operation timed out             | ETIMEDOUT                      |
| 8    | ERR_EXISTS         | Resource already exists          | EEXIST                         |
| 9    | ERR_BUSY           | Resource busy                   | EBUSY, EAGAIN                  |
| 10   | ERR_PIPE           | Broken pipe                     | EPIPE                          |
| 11   | ERR_CONN_RESET     | Connection reset                | ECONNRESET                     |
| 12   | ERR_ADDR_IN_USE    | Address already in use          | EADDRINUSE                     |
| 13   | ERR_NOT_CONNECTED  | Not connected                   | ENOTCONN                       |
| 14   | ERR_INTERRUPTED    | Interrupted                     | EINTR                          |
| 15   | ERR_IO             | I/O error                       | EIO                            |
| 16   | ERR_NOLIB          | FFI: library load failed        | dlopen failure                 |
| 17   | ERR_NOSYM          | FFI: symbol not found           | dlsym failure                  |
| 16-255 | --               | Reserved for future standard use| --                             |
| 256+ | --                 | User-defined error codes        | --                             |

**Propagation with `?`:**

The `?` operator works like Rust's `?` on `Result`: if the error register is set, immediately return from the current subroutine with the error preserved.

```bfpp
!#risky_op ?          ; call subroutine, propagate error if any
; execution only reaches here if no error
```

**Result/Catch blocks (`R{...}K{...}`):**

Provides local error handling. `R{...}` executes its body. If the error register becomes non-zero during execution, control jumps to the `K{...}` block. `K{...}` must immediately follow `R{...}`.

**Working example:**

```bfpp
; Subroutine that sets an error
!#fail{
  ++++++ e   ; set error register to 6 (ERR_INVALID_ARG)
  ^
}

; Subroutine that chains and propagates
!#chain{
  !#fail ?   ; call fail, propagate error
  ; unreachable if error
  ^
}

; Top-level: catch the error
R{
  !#chain
}K{
  E          ; load error code into cell (should be 6)
  ; Add 48 to convert to ASCII digit: 6 + 48 = 54 = '6'
  ++++++++++++++++++++++++++++++++++++++++++++++++ .
  ; Print newline
  [-] ++++++++++ .
}
```

Output: `6` followed by a newline.

---

#### 7. Bitwise Operations

All bitwise ops operate on the current cell in-place, using `tape[ptr+1]` as the second operand where applicable.

| Op  | Name        | Semantics                                          |
|-----|-------------|-----------------------------------------------------|
| `\|`| Bitwise OR  | `tape[ptr] \|= tape[ptr+1]`                        |
| `&` | Bitwise AND | `tape[ptr] &= tape[ptr+1]`                         |
| `x` | Bitwise XOR | `tape[ptr] ^= tape[ptr+1]`                         |
| `s` | Shift left  | `tape[ptr] <<= tape[ptr+1]`                        |
| `r` | Shift right | `tape[ptr] >>= tape[ptr+1]` (logical, zero-fill)   |
| `n` | Bitwise NOT | `tape[ptr] = ~tape[ptr]`                            |

**Example -- masking with AND:**

```bfpp
; Compute 0xFF & 0x0F = 0x0F
; Set cell[0] = 0xFF (255)
[-] ++++++++++++++++++++++++++++++++++++++++++++++++
    ++++++++++++++++++++++++++++++++++++++++++++++++
    ++++++++++++++++++++++++++++++++++++++++++++++++
    ++++++++++++++++++++++++++++++++++++++++++++++++
    ++++++++++++++++++++++++++++++++++++++++++++++++
    +++++++++
; Set cell[1] = 0x0F (15)
> [-] +++++++++++++++
< &                   ; cell[0] = 0xFF & 0x0F = 0x0F
```

**Example -- shift left:**

```bfpp
; Shift 1 left by 4 = 16
[-] +                 ; cell[0] = 1
> [-] ++++            ; cell[1] = 4
< s                   ; cell[0] = 1 << 4 = 16
```

---

#### 8. System Calls

The `\` operator executes a raw Linux syscall. Arguments are read from the tape starting at `ptr`:

| Offset   | Content        |
|----------|----------------|
| `ptr+0`  | Syscall number |
| `ptr+8`  | Arg 1          |
| `ptr+16` | Arg 2          |
| `ptr+24` | Arg 3          |
| `ptr+32` | Arg 4          |
| `ptr+40` | Arg 5          |
| `ptr+48` | Arg 6          |

After execution, `tape[ptr]` contains the return value. Error register is set on failure via errno mapping.

Arguments are at 8-byte intervals (one 64-bit cell each). For syscall usage, you typically need to cycle cells to 64-bit width with `%%%` (three `%` ops: 8->16->32->64).

**File descriptor directed I/O (shorthand):**

| Op     | Semantics                                            |
|--------|------------------------------------------------------|
| `.{N}` | Write `tape[ptr]` to file descriptor N               |
| `,{N}` | Read one byte from file descriptor N into `tape[ptr]`|
| `.{*}` | Write to fd stored in `tape[ptr+1]` (indirect)       |
| `,{*}` | Read from fd stored in `tape[ptr+1]` (indirect)      |

```bfpp
; Write '#' to stderr (fd 2)
[-] +++++++++++++++++++++++++++++++++++
.{2}

; Read a byte from fd 3
,{3}
```

---

#### 9. FFI (Foreign Function Interface)

Call C functions from shared libraries:

```
\ffi "library" "function"
```

- Loads shared library `library` via `dlopen`.
- Calls function `function` via `dlsym`.
- Parameters read from tape at `ptr`.
- Return value written to `tape[ptr]`.
- On failure: error register set to 16 (ERR_NOLIB) or 17 (ERR_NOSYM).

The compiler links `-ldl` automatically when FFI is used.

**Example -- calling libm's `ceil`:**

```bfpp
\ffi "libm.so.6" "ceil"
```

---

#### 10. Framebuffer

Enable with `--framebuffer WxH`:

```sh
bfpp program.bfpp --framebuffer 80x60
```

This maps a pixel buffer into the tape as `W * H * 3` bytes (RGB pixels). The framebuffer occupies the upper region of the tape (starting at 0xA000).

| Op  | Semantics                                   |
|-----|---------------------------------------------|
| `F` | Flush framebuffer -- render pixels to display |

Requires SDL2 development libraries. The compiler links `-lSDL2` automatically.

Tape size must accommodate the framebuffer plus at least 256 bytes of working space:

```sh
; 80x60 RGB = 14,400 bytes. Default 64K tape is sufficient.
bfpp game.bfpp --framebuffer 80x60
```

---

#### 11. Preprocessor

The `!include` directive splices another file's contents into the source before lexing:

```bfpp
!include "io.bfpp"
!include "math.bfpp"
```

**Resolution order** (first match wins):

1. Relative to the directory of the file containing the `!include`
2. Each `--include PATH` provided on the command line, in order
3. `./stdlib/` relative to the current working directory
4. `stdlib/` relative to the `bfpp` executable's directory

**Cycle detection:** Re-including an already-visited file is silently skipped, supporting diamond-shaped include graphs.

**Max include depth:** 64 levels.

---

### Using the Standard Library

The stdlib is written in BF++ itself, located in the `stdlib/` directory. Include it using `!include` with the `--include` flag pointing to the stdlib directory:

```sh
bfpp myprogram.bfpp --include stdlib/
```

Or, if running from the project root, `./stdlib/` is searched automatically.

---

#### Module: io.bfpp

Basic I/O for stdin/stdout. Depends on `math.bfpp` (included automatically).

```bfpp
!include "io.bfpp"
```

| Subroutine   | Symbol | Description                                        |
|--------------|--------|----------------------------------------------------|
| print_string | `!#.>` | Print null-terminated string at ptr to stdout       |
| print_int    | `!#.+` | Print cell value as decimal ASCII to stdout         |
| read_line    | `!#,<` | Read from stdin until newline/EOF, null-terminates  |
| read_int     | `!#,+` | Read decimal integer from stdin into cell           |

**Usage:**

```bfpp
!include "io.bfpp"

; Print a string
"Hello, World!\n\0"
<<<<<<<<<<<<<<<
!#.>

; Print a number
[-] +++++ +++++ +++++ +++++ +++++ +++++ +++++ +++++ +++++ +++++ +++++ +++++
; cell = 60
!#.+    ; prints "60"
```

---

#### Module: math.bfpp

Unsigned arithmetic. Arguments at `tape[ptr]` and `tape[ptr+1]`, result in `tape[ptr]`.

```bfpp
!include "math.bfpp"
```

| Subroutine | Symbol  | Description                                     |
|------------|---------|--------------------------------------------------|
| multiply   | `!#m*`  | `tape[ptr] = tape[ptr] * tape[ptr+1]`           |
| divide     | `!#m/`  | `tape[ptr] = tape[ptr] / tape[ptr+1]` (err 6 if B=0) |
| modulo     | `!#m%`  | `tape[ptr] = tape[ptr] % tape[ptr+1]` (err 6 if B=0) |
| power      | `!#mcaret` | `tape[ptr] = tape[ptr] ^ tape[ptr+1]`        |

**Workspace requirements:** multiply uses ptr+2..ptr+3; divide/modulo use ptr+2..ptr+4 (actually touches ptr+5); power uses ptr+2..ptr+8. Ensure these cells are zero before calling.

**Usage:**

```bfpp
!include "math.bfpp"

; 7 * 6 = 42
[-] +++++++ > [-] ++++++ <
!#m*
; tape[ptr] = 42

; 42 / 6 = 7
> [-] ++++++ <
!#m/
; tape[ptr] = 7
```

---

#### Module: string.bfpp

Null-terminated string operations.

```bfpp
!include "string.bfpp"
```

| Subroutine | Symbol | Description                                          |
|------------|--------|------------------------------------------------------|
| strlen     | `!#sl` | Walk to null terminator. Distance = string length.   |
| strcmp      | `!#sc` | Compare strings (limited -- see notes).              |
| strcpy     | `!#sy` | Copy string (limited -- see notes).                  |
| strcat     | `!#sa` | Append src to dest (adjacent strings only).          |

Note: `strcmp`, `strcpy`, and `strcat` have documented limitations due to BF's single-pointer architecture. `strlen` and adjacent-string `strcat` work correctly. For reliable string comparison and copying, use inline BF with known relative offsets.

---

#### Module: err.bfpp

Error handling utilities. Depends on `io.bfpp`.

```bfpp
!include "err.bfpp"
```

| Subroutine    | Symbol | Description                                     |
|---------------|--------|-------------------------------------------------|
| err_to_string | `!#es` | Print "err:" followed by the error code digit   |
| panic         | `!#ep` | Print message to stderr (fd 2), exit with code 1|
| assert        | `!#ea` | If `tape[ptr] == 0`, panic with "assertion failed" |

**Usage:**

```bfpp
!include "err.bfpp"

; Assert a condition
+++++ !#ea            ; passes (cell != 0)

; Panic with message
"fatal error\n\0"
<<<<<<<<<<<<
!#ep                  ; prints to stderr, exits
```

---

#### Module: file.bfpp

File operations using raw syscalls (Linux x86_64).

```bfpp
!include "file.bfpp"
```

| Subroutine | Symbol | Description                                  |
|------------|--------|----------------------------------------------|
| file_open  | `!#fo` | Execute pre-configured open syscall          |
| file_read  | `!#fr` | Execute pre-configured read syscall          |
| file_write | `!#fw` | Execute pre-configured write syscall         |
| file_close | `!#fc` | Close file descriptor. Input: `tape[ptr]` = fd |

These are thin wrappers around the `\` syscall operator. The caller is responsible for setting up the syscall parameter layout at `ptr` (see System Calls section). `!#fc` (file_close) is the most self-contained -- it takes the fd in `tape[ptr]` and handles the syscall setup internally.

---

#### Module: net.bfpp

TCP networking using raw syscalls (Linux x86_64).

```bfpp
!include "net.bfpp"
```

| Subroutine  | Symbol  | Description                                 |
|-------------|---------|----------------------------------------------|
| tcp_connect | `!#tcp` | Create TCP socket (execute socket syscall)   |
| tcp_listen  | `!#tl`  | Create server socket                         |
| tcp_accept  | `!#ta`  | Accept incoming connection                   |
| tcp_send    | `!#ts`  | Send data on socket (uses SYS_write)         |
| tcp_recv    | `!#tr`  | Receive data from socket (uses SYS_read)     |

All require the caller to set up 64-bit syscall params at `ptr` before calling. See the source comments in `stdlib/net.bfpp` for exact parameter layouts.

---

#### Module: tui.bfpp

Terminal UI via ANSI escape sequences.

```bfpp
!include "tui.bfpp"
```

| Subroutine  | Symbol | Description                                           |
|-------------|--------|-------------------------------------------------------|
| cursor_move | `!#cm` | Move cursor to (row, col). Single-digit 1-9 only.    |
| clear       | `!#cl` | Clear screen.                                         |
| set_color   | `!#co` | Set terminal color (ANSI 0-7 fg, 8-15 bg).           |
| draw_box    | `!#db` | Draw ASCII box at (row, col, width, height).          |

**Color codes:**

| Value | FG Color | Value | BG Color  |
|-------|----------|-------|-----------|
| 0     | Black    | 8     | Black BG  |
| 1     | Red      | 9     | Red BG    |
| 2     | Green    | 10    | Green BG  |
| 3     | Yellow   | 11    | Yellow BG |
| 4     | Blue     | 12    | Blue BG   |
| 5     | Magenta  | 13    | Magenta BG|
| 6     | Cyan     | 14    | Cyan BG   |
| 7     | White    | 15    | White BG  |

**Usage:**

```bfpp
!include "tui.bfpp"

!#cl                  ; clear screen

; Move cursor to row 3, col 5
[-] +++ > [-] +++++ <
!#cm

; Set foreground green
[-] ++
!#co

; Draw a 10x5 box at row 1, col 1
[-] + > [-] + > [-] ++++++++++ > [-] +++++ <<<
!#db
```

---

#### Module: mem.bfpp

Memory management within the tape.

```bfpp
!include "mem.bfpp"
```

| Subroutine | Symbol | Description                                          |
|------------|--------|------------------------------------------------------|
| memcpy     | `!#mc` | Copy N bytes from src to dest (limited -- see notes) |
| memset     | `!#ms` | Fill N bytes at dest with value (limited -- see notes)|
| malloc     | `!#ma` | Bump allocator. Currently returns error 4 (OOM).    |
| free       | `!#mf` | No-op for bump allocator.                            |

Note: `memcpy` and `memset` have fundamental limitations due to BF's single-pointer architecture (documented in source). `malloc` requires compile-time constant support or 16-bit+ cells to address the heap metadata region, which is not yet practical. For memory management, use known tape regions with manual offset tracking.

---

### Compiler Options Reference

| Flag                 | Default  | Description                                          |
|----------------------|----------|------------------------------------------------------|
| `<input>`            | required | Input BF++ source file                               |
| `-o <path>`          | `<stem>` | Output binary name (or C file name with `--emit-c`)  |
| `--emit-c`           | off      | Emit C source instead of compiling to binary         |
| `--tape-size <N>`    | 65536    | Tape size in bytes (must be a power of 2)            |
| `--stack-size <N>`   | 4096     | Data stack size in entries                           |
| `--call-depth <N>`   | 256      | Max subroutine call stack depth                      |
| `--framebuffer <WxH>`| off      | Enable framebuffer mode (e.g., `80x60`). Links SDL2. |
| `--no-optimize`      | off      | Disable all optimization passes                      |
| `-O <level>`         | 1        | Optimization level: 0, 1, or 2                      |
| `--include <path>`   | none     | Additional include search paths (repeatable)         |
| `--cc <compiler>`    | `cc`     | C compiler to invoke                                 |
| `--eof <value>`      | 0        | Value written to cell on EOF (0 or 255)              |

---

### Optimization Levels

| Level | Flag           | Passes                                              | Use when                                      |
|-------|----------------|------------------------------------------------------|-----------------------------------------------|
| O0    | `-O0` or `--no-optimize` | None. AST passes through unchanged.        | Debugging generated C code.                   |
| O1    | `-O1` (default)| Clear-loop (`[-]` -> set 0), error folding (collapse consecutive `?`). | Normal compilation. Safe, lightweight passes. |
| O2    | `-O2`          | All O1 passes + scan-loop (`[>]`/`[<]` -> linear scan) + multiply-move (`[->+++<<]` -> direct arithmetic). | Maximum performance. Use for production builds. |

**What each pass does:**

- **Clear-loop:** `[-]` and `[+]` become `tape[ptr] = 0` (single assignment instead of a loop).
- **Error folding:** Consecutive `? ? ?` collapses to a single `?` (redundant checks eliminated).
- **Scan-loop:** `[>]` becomes a while-loop scanning right for zero; `[<]` scans left.
- **Multiply-move:** `[->>+++<<]` becomes `tape[ptr+2] += tape[ptr] * 3; tape[ptr] = 0` (O(1) instead of O(N) per source cell value).

---

## Part 2: BF Interpreter (bf-interpreter)

A standalone interpreter for standard Brainfuck programs.

### Building

```sh
cd bf-interpreter
cargo build --release
```

Binary is at `target/release/bf-interpreter`.

### Usage

```sh
bf-interpreter program.bf
```

**What it supports:**

- Standard 8 Brainfuck instructions: `> < + - . , [ ]`
- `;` line comments (consumed to end of line)
- 30,000-cell tape (wrapping pointer)
- 8-bit cells with wrapping arithmetic
- EOF returns 0

All non-instruction characters are stripped before execution.

**Library API:**

The interpreter is also a Rust library crate. Import and call:

```rust
use bf_interpreter;

fn main() {
    let source = "++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.";
    match bf_interpreter::run(source, &[]) {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output);
            println!("{}", text);
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}
```

`run(source, input)` takes the BF source string and an input byte slice, returns `Result<Vec<u8>, BfError>`. The output is collected in memory (not printed to stdout).

Available functions:

- `bf_interpreter::run(source: &str, input: &[u8]) -> Result<Vec<u8>, BfError>` -- full execution
- `bf_interpreter::parse_program(source: &str) -> Vec<char>` -- strip comments, filter to BF ops
- `bf_interpreter::build_jumps(program: &[char]) -> Result<Vec<usize>, BfError>` -- bracket jump table

### Examples

**Hello World:**

```bf
++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.
```

Output: `Hello World!`

**Cat program (echo input):**

```bf
,[.,]
```

Reads bytes from stdin and echoes them back until EOF (input byte becomes 0).

**Simple addition (3 + 5 = 8, print as digit):**

```bf
+++ > +++++ <        ; cell[0] = 3, cell[1] = 5
[->+<]               ; add cell[0] to cell[1], cell[0] = 0
> ++++++++++++++++++++++++++++++++++++++++++++++++  ; add 48 ('0')
.                    ; print '8'
```

**Clear cell idiom:**

```bf
[-]                  ; decrement until zero -- standard BF clear pattern
```
