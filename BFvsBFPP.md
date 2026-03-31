# BF vs BF++: Complete Comparison

## Overview

**Standard Brainfuck** is a minimalist esoteric language with exactly 8 operators. It is Turing-complete but provides no I/O beyond single-byte stdin/stdout, no error handling, no subroutines, no bitwise operations, and no system interface. Programs operate on a fixed 30,000-cell tape of 8-bit values with a single pointer.

**BF++** is a strict superset of Brainfuck that retains symbolic minimalism while adding 30+ operators for system calls, file I/O, networking, error handling, subroutines, bitwise arithmetic, FFI, and a framebuffer. Programs are transpiled to C11 via a Rust-based compiler, then compiled to native binaries. BF++ includes a standard library written in BF++ itself, a preprocessor with `!include` support, and a multi-pass optimizer.

---

## Compatibility

- Every valid BF program is a valid BF++ program with identical semantics.
- BF++ adds `;` for line comments. Standard BF already ignores non-operator characters, so BF programs containing comments (any text that isn't one of `><+-.,[]`) remain valid.
- The 8 core BF operators have unchanged semantics in BF++.

---

## Feature Comparison Table

| Feature | Standard BF | BF++ | Notes |
|---------|------------|------|-------|
| **Operators** | 8 (`><+-.,[]`) | 30+ | All BF ops preserved, extended set added |
| **Cell width** | 8-bit fixed | 8/16/32/64-bit per cell (`%`) | Per-cell width metadata tracked in parallel array |
| **Tape size** | 30,000 cells fixed | 65,536 bytes default, configurable (`--tape-size`) | Power-of-2 sizes supported |
| **Pointer movement** | Sequential only (`>` `<`) | Sequential + absolute (`@`) + dereference (`*`) | `@` = jump to address in cell; `*` = indirect access |
| **I/O** | stdin byte (`,`), stdout byte (`.`) | stdin/stdout + fd-directed (`.{N}` `,{N}`), indirect fd (`.{*}` `,{*}`), string literals (`"..."`) | Arbitrary file descriptors; runtime-determined fds |
| **Control flow** | Loops only (`[]`) | Loops + subroutines (`!#name{...}` / `!#name`) + early return (`^`) | Full recursion support |
| **Arithmetic** | Increment/decrement by 1 | Inc/dec (coalesced) + bitwise OR, AND, XOR, shift, NOT | Bitwise ops use adjacent cell as second operand |
| **Error handling** | None | Error register (`E`/`e`), propagation (`?`), try/catch (`R{...}K{...}`) | Modeled after Rust's `Result`/`?` pattern |
| **System interface** | None | Raw syscalls (`\`), FFI (`\ffi "lib" "func"`), framebuffer (`F`) | Platform-abstracted via runtime header |
| **Stack** | None | Auxiliary data stack (`$` push, `~` pop) | 4,096 entries, 64-bit each, separate from tape |
| **Call stack** | None | 256-frame call stack for subroutines | Stores return address + saved error state |
| **Preprocessing** | None | `!include "file"` with search paths and cycle detection | Text-level expansion before lexing |
| **Optimization** | None (typically interpreted) | Clear-loop, scan-loop, multiply-move, error folding | `-O1` (basic), `-O2` (full) |
| **Standard library** | None | 8 modules: io, math, string, mem, err, file, net, tui | Written in BF++ itself |
| **Compilation** | Typically interpreted | Transpiled to C11, compiled via `cc`. `--emit-c` available | Also supports direct C output for inspection |
| **Comments** | Non-operator chars ignored (implicit) | `;` line comments (explicit) | BF's implicit comment behavior preserved |
| **Memory layout** | Flat, unstructured | Structured regions: general purpose, syscall params, I/O buffer, framebuffer | Conventions enforced by stdlib, not hardware |
| **String handling** | Manual byte-by-byte placement | String literals with escape sequences (`\n`, `\t`, `\xHH`, etc.) | Written sequentially to tape from pointer |

---

## Detailed Feature Comparison

### Data Model

**Standard BF:**
- 8-bit unsigned cells (0-255, wrapping)
- 30,000-cell tape
- No resize or reconfiguration

**BF++:**
- Configurable cell width per cell: 8, 16, 32, or 64 bits via `%` (cycles through widths)
- Default tape size: 65,536 bytes, configurable via `--tape-size N`
- Multi-byte cells use little-endian byte order and occupy consecutive tape positions
- Cell width metadata tracked in a parallel array (1 byte per cell)
- `T` operator: store current pointer address into cell (tape address introspection)

```
; BF: cell is always 8-bit, max value 255
+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
; 65 = 'A'

; BF++: switch to 16-bit cell, can hold values up to 65535
%                     ; cycle to 16-bit
++++++++++++++++++++  ; can now accumulate beyond 255
```

---

### Memory Access

**Standard BF:**
- Sequential movement only: `>` (right), `<` (left)
- No random access; reaching cell N requires N moves from position 0

**BF++:**
- Sequential movement: `>` and `<` (identical to BF)
- Absolute addressing: `@` sets `ptr = tape[ptr]` (jump pointer to address stored in current cell)
- Dereference: `*` treats current cell as a pointer; the next operation targets `tape[tape[ptr]]`
- Stack: `$` (push cell value), `~` (pop into cell); separate 4,096-entry data stack

**BF -- Jump to cell 100 (requires 100 moves):**
```
>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>
>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>>
; now at cell 100
```

**BF++ -- Jump to cell 100 (3 operators):**
```
; Assuming current cell contains 100:
[-]++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
@                     ; ptr = tape[ptr] = 100
```

**BF++ -- Pointer dereference:**
```
; cell[0] = 5, cell[5] = 65 ('A')
; Set cell[0] as pointer to cell[5]:
*+                    ; increments tape[tape[ptr]], i.e., tape[5]
*.                    ; outputs tape[tape[ptr]], i.e., tape[5] = 'A'
```

---

### I/O

**Standard BF:**
- `,` reads one byte from stdin into current cell
- `.` writes current cell as one byte to stdout
- No file I/O, no fd selection, no string handling

**BF++:**
- `,` and `.` unchanged (stdin/stdout byte I/O)
- `.{N}` writes current cell to file descriptor N (e.g., `.{2}` = stderr)
- `,{N}` reads from file descriptor N into current cell
- `.{*}` / `,{*}` use indirect fd: the fd number comes from `tape[ptr+1]` at runtime
- `"..."` string literals with escape sequences: `\n`, `\r`, `\t`, `\0`, `\\`, `\"`, `\xHH`
- String literals write bytes to tape sequentially starting at pointer, advancing past last byte

**BF -- Print "Hi" (manual byte placement):**
```
++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++.
; 72 = 'H'
[-]
+++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++.
; 105 = 'i'
```

**BF++ -- Print "Hi" (string literal):**
```
"Hi"                  ; writes H, i to tape
<<                    ; back to start
[.>]                  ; print each byte
```

**BF++ -- Write to stderr:**
```
"Error!\n"
<<<<<<<
[.{2}>]               ; each byte to fd 2 (stderr)
```

---

### Control Flow

**Standard BF:**
- `[` / `]` loops are the only control flow mechanism
- No subroutines, no function calls, no early exit

**BF++:**
- `[` / `]` loops preserved with identical semantics
- Subroutine definition: `!#name{ body }` -- stores named code block
- Subroutine call: `!#name` -- push return context, jump to body
- Early return: `^` -- pop return context, resume after call site
- Full recursion supported; each call pushes a new frame (256-frame limit)
- Error propagation (`?`) provides conditional early return

**BF -- "Subroutine" (not possible; must inline code everywhere):**
```
; To print a null-terminated string, you inline [.>] at every call site
; No way to define it once and reuse
[.>]       ; print string at location A
; ... later ...
[.>]       ; must repeat at location B
```

**BF++ -- Subroutine definition and reuse:**
```
!#pr{
  [.>]                ; print bytes until null
  ^                   ; return
}

"Hello\0"
<<<<<<
!#pr                  ; call print subroutine

"World\0"
<<<<<<
!#pr                  ; reuse same subroutine
```

**BF++ -- Recursive factorial:**
```
!#fac{
  $                   ; save n
  -                   ; n-1
  [                   ; if n > 1
    !#fac             ; factorial(n-1)
    ~ >               ; pop original n
    < !#m*            ; multiply
    ^
  ]
  ~ [-] +            ; base case: return 1
  ^
}

+++++ !#fac           ; compute 5! = 120
```

---

### Arithmetic

**Standard BF:**
- `+` increments current cell by 1
- `-` decrements current cell by 1
- No multiplication, division, bitwise operations, or multi-cell arithmetic
- Multiplication requires loops: e.g., `[->>+++<<]` multiplies by 3

**BF++:**
- `+` / `-` preserved, coalesced by parser (e.g., `++++` becomes a single Increment(4) node)
- Bitwise OR: `|` -- `tape[ptr] |= tape[ptr+1]`
- Bitwise AND: `&` -- `tape[ptr] &= tape[ptr+1]`
- Bitwise XOR: `x` -- `tape[ptr] ^= tape[ptr+1]`
- Shift left: `s` -- `tape[ptr] <<= tape[ptr+1]`
- Shift right: `r` -- `tape[ptr] >>= tape[ptr+1]` (logical, zero-fill)
- Bitwise NOT: `n` -- `tape[ptr] = ~tape[ptr]`
- All bitwise ops respect current cell width
- Stdlib provides multiply (`!#m*`), divide (`!#m/`), modulo (`!#m%`), power (`!#m^`)

**BF -- Multiply cell by 3 (loop-based):**
```
[->>+++<<]            ; for each decrement of cell[0], add 3 to cell[2]
                      ; result in cell[2], cell[0] destroyed
```

**BF++ -- Bitwise AND (direct operator):**
```
; cell[0] = 0xFF, cell[1] = 0x0F
; Result: cell[0] = 0x0F
&                     ; tape[ptr] &= tape[ptr+1]
```

---

### Error Handling

**Standard BF:**
- No error handling mechanism
- Undefined behavior on pointer out-of-bounds
- No way to detect or recover from failures

**BF++:**
- 64-bit error register (`bfpp_err`), separate from tape and stacks
- `E` reads error register into current cell
- `e` writes current cell into error register
- `?` propagates: if error register is non-zero, immediately returns from current subroutine (analogous to Rust's `?` on `Result`)
- `R{...}K{...}` try/catch blocks: R block executes; if it sets an error, execution jumps to K block with error preserved
- 16 standard error codes (0-15) mapped from POSIX errno, user-defined codes from 256+
- Syscall failures automatically set the error register
- Stack underflow/overflow set specific error codes

**BF -- Error handling (impossible):**
```
; No mechanism exists. If something goes wrong,
; behavior is undefined. No recovery possible.
```

**BF++ -- Error propagation and try/catch:**
```
!#risky{
  ++++++ e            ; set error to 6 (ERR_INVALID_ARG)
  ^                   ; return with error
}

!#chain{
  !#risky ?           ; call risky; if error, propagate immediately
  "OK\0" !#.>        ; only reached if no error
  ^
}

R{
  !#chain             ; try the chain
}K{
  E                   ; load error code into cell
  ; error code 6 is now in current cell
  ; convert to ASCII: 6 + 48 = '6'
  ++++++++++++++++++++++++++++++++++++++++++++++++ .
  [-] ++++++++++ .   ; newline
}
```

---

### System Interface

**Standard BF:**
- No system calls
- No file operations beyond stdin/stdout
- No FFI
- No framebuffer or graphics

**BF++:**
- **Raw syscalls** (`\`): `tape[ptr]` = syscall number, `tape[ptr+1..ptr+6]` = args (up to 6, 64-bit each). Result written to `tape[ptr]`. Errors mapped to BF++ error codes.
- **FFI** (`\ffi "lib" "func"`): call a C function from a shared library. Parameters read from tape, return value written to `tape[ptr]`.
- **Framebuffer** (`F`): flush pixel buffer (0xA000-0xFFFF region) to display. RGB888 format, ~90x90 pixels at default tape size. Requires `--framebuffer WxH` compiler flag.

**BF++ -- Syscall example (Linux file write):**
```
; Navigate to syscall region, switch to 64-bit cells
@128 %64

; syscall 1 = sys_write, fd=1 (stdout), buf, count
+                     ; tape[0x8000] = 1 (sys_write)
> +                   ; arg1 = 1 (stdout fd)
> "Hello\0"           ; data at arg2
> +++++               ; arg3 = 5 (byte count)
<<<<<
\                     ; execute syscall
?                     ; propagate error if write failed
```

**BF++ -- FFI example:**
```
\ffi "libm.so.6" "ceil"   ; call ceil() from libm
```

---

### Preprocessing

**Standard BF:**
- No preprocessing
- No include mechanism
- Code reuse requires manual copy-paste

**BF++:**
- `!include "filename"` directive, resolved before lexing (text-level expansion)
- Search path resolution order:
  1. Relative to the file containing the `!include`
  2. Each `--include` path (CLI flag), in order
  3. `./stdlib/` relative to CWD
  4. `stdlib/` relative to the bfpp executable
- Cycle detection via canonical path tracking (diamond includes work correctly)
- Maximum include depth: 64 levels
- String-literal aware: `!include` inside multi-line strings is not expanded

**BF++ -- Including stdlib modules:**
```
!include "io.bfpp"
!include "math.bfpp"

"Hello, World!\0"
<<<<<<<<<<<<<<<
!#.>                  ; print_string from io.bfpp
```

---

### Optimization

**Standard BF:**
- Typically interpreted with no optimization
- Some interpreters do basic loop optimization

**BF++:**
- **Parser-level**: consecutive identical ops coalesced (e.g., `++++` becomes `Increment(4)`)
- **-O1 (Basic)**:
  - Clear-loop: `[-]` and `[+]` replaced with `Clear` node (emits `tape[ptr] = 0`)
  - Error folding: consecutive `?` operators collapsed into one
- **-O2 (Full)**: all Basic passes plus:
  - Scan-loop: `[>]` replaced with `ScanRight`, `[<]` with `ScanLeft` (emits memchr-style scan)
  - Multiply-move: patterns like `[->>+++<<]` extracted as `MultiplyMove([(2, 3)])` -- emits direct arithmetic instead of O(N) loop
- Optimizer recurses into loop bodies, subroutine bodies, and result/catch blocks
- `--no-optimize` disables all passes

**BF -- Clear cell (loop executes N times):**
```
[-]                   ; decrements cell to 0, one iteration per unit of value
```

**BF++ -- Clear cell (optimizer reduces to single assignment):**
```
[-]                   ; optimizer detects this, emits tape[ptr] = 0
; No loop in generated C code
```

**BF++ -- Multiply-move (optimizer extracts pattern):**
```
[->>+++>+<<<]         ; optimizer detects: tape[ptr+2] += tape[ptr]*3,
                      ;                    tape[ptr+3] += tape[ptr]*1,
                      ;                    tape[ptr] = 0
; Generated C: straight-line arithmetic, no loop
```

---

### Standard Library

**Standard BF:**
- No standard library
- Every program starts from scratch

**BF++:**
8 modules, all written in BF++ itself:

| Module | Prefix | Key Subroutines |
|--------|--------|-----------------|
| `io.bfpp` | `.` `,` | `!#.>` print_string, `!#.+` print_int, `!#,<` read_line, `!#,+` read_int |
| `math.bfpp` | `m` | `!#m*` multiply, `!#m/` divide, `!#m%` modulo, `!#m^` power |
| `string.bfpp` | `s` | `!#sl` strlen, `!#sc` strcmp, `!#sy` strcpy, `!#sa` strcat |
| `mem.bfpp` | `m` | `!#mc` memcpy, `!#ms` memset, `!#ma` malloc, `!#mf` free |
| `err.bfpp` | `e` | `!#es` err_to_string, `!#ep` panic, `!#ea` assert |
| `file.bfpp` | `f` | `!#fo` file_open, `!#fr` file_read, `!#fw` file_write, `!#fc` file_close |
| `net.bfpp` | `t` | `!#tcp` tcp_connect, `!#tl` tcp_listen, `!#ta` tcp_accept, `!#ts` tcp_send, `!#tr` tcp_recv |
| `tui.bfpp` | `c`/`d` | `!#cm` cursor_move, `!#cl` clear, `!#co` set_color, `!#db` draw_box |

Naming convention: 2-character names after `#`. First character = module, second = operation.

---

### Compilation Model

**Standard BF:**
- Typically interpreted by a BF interpreter
- Some implementations compile to C or native code, but no standard compilation model

**BF++:**
- Transpiled to C11 by a Rust compiler (`bfpp`)
- Generated C includes `bfpp_runtime.h` (tape, stacks, error register, syscall abstraction)
- Each subroutine becomes a C function (names mangled: `>` -> `gt`, `.` -> `dot`, etc.)
- Compiled via system `cc` (gcc/clang)
- `--emit-c` outputs C source for inspection

| Compiler Flag | Effect |
|---------------|--------|
| `--tape-size N` | Set tape size in bytes (default 65536) |
| `--stack-size N` | Data stack entries (default 4096) |
| `--call-depth N` | Max call stack depth (default 256) |
| `--framebuffer WxH` | Enable framebuffer with dimensions |
| `--no-optimize` | Disable all optimizer passes |
| `-O1` | Basic optimizations |
| `-O2` | All optimizations |
| `-o FILE` | Output binary name |
| `--emit-c` | Output C source instead of binary |
| `--include PATH` | Add stdlib/include search path |

---

### Memory Layout

**Standard BF:**
- Flat, unstructured tape
- No designated regions

**BF++:**
Structured regions within the 64 KB default tape:

| Region | Address Range | Size | Purpose |
|--------|---------------|------|---------|
| General Purpose | `0x0000`-`0x7FFF` | 32 KB | User data, strings, computation |
| Syscall Parameters | `0x8000`-`0x80FF` | 256 B | Syscall number + up to 6 args (64-bit each) |
| I/O Buffer | `0x8100`-`0x8FFF` | 3,840 B | Buffered read/write staging (stdlib-managed) |
| Reserved | `0x9000`-`0x9FFF` | 4 KB | Future use (heap metadata, TLS, etc.) |
| Framebuffer | `0xA000`-`0xFFFF` | 24 KB | Pixel buffer (RGB888, when `--framebuffer` enabled) |

Separate memory regions (not on the tape):
- **Data stack**: 4,096 entries, 64-bit each (32 KB)
- **Call stack**: 256 frames (return address + saved error register per frame)
- **Cell width metadata**: parallel array, 1 byte per tape cell

---

## Complete Operator Reference

### Standard BF Operators (8)

| Op | Semantics |
|----|-----------|
| `>` | `ptr += 1` |
| `<` | `ptr -= 1` |
| `+` | `tape[ptr] += 1` |
| `-` | `tape[ptr] -= 1` |
| `.` | write `tape[ptr]` to stdout |
| `,` | read byte from stdin into `tape[ptr]` |
| `[` | if `tape[ptr] == 0`, jump past `]` |
| `]` | if `tape[ptr] != 0`, jump back to `[` |

### BF++ Extended Operators (25+ additional)

| Op | Name | Semantics |
|----|------|-----------|
| `@` | Absolute address | `ptr = tape[ptr]` |
| `*` | Dereference | Next op targets `tape[tape[ptr]]`; auto-restore pointer |
| `%` | Cell width cycle | Cycle: 8 -> 16 -> 32 -> 64 -> 8 bits |
| `"..."` | String literal | Write bytes to tape, advance pointer |
| `$` | Push | Push `tape[ptr]` onto data stack |
| `~` | Pop | Pop stack top into `tape[ptr]` |
| `!#name{...}` | Subroutine def | Define named subroutine |
| `!#name` | Subroutine call | Call named subroutine |
| `^` | Return | Return from subroutine |
| `\` | Syscall | Execute system call from tape layout |
| `.{N}` | Write to fd | Write `tape[ptr]` to fd N |
| `,{N}` | Read from fd | Read byte from fd N into `tape[ptr]` |
| `.{*}` | Write to indirect fd | fd from `tape[ptr+1]` |
| `,{*}` | Read from indirect fd | fd from `tape[ptr+1]` |
| `\|` | Bitwise OR | `tape[ptr] \|= tape[ptr+1]` |
| `&` | Bitwise AND | `tape[ptr] &= tape[ptr+1]` |
| `x` | Bitwise XOR | `tape[ptr] ^= tape[ptr+1]` |
| `s` | Shift left | `tape[ptr] <<= tape[ptr+1]` |
| `r` | Shift right | `tape[ptr] >>= tape[ptr+1]` |
| `n` | Bitwise NOT | `tape[ptr] = ~tape[ptr]` |
| `E` | Error read | `tape[ptr] = bfpp_err` |
| `e` | Error write | `bfpp_err = tape[ptr]` |
| `?` | Propagate | If error set, return from subroutine |
| `R{...}` | Result block | Try block; catches errors for matching `K{...}` |
| `K{...}` | Catch block | Runs if preceding `R{...}` errored |
| `T` | Tape address | Store current pointer address into cell |
| `F` | Framebuffer flush | Flush pixel buffer to display |
| `\ffi "l" "f"` | FFI call | Call C function from shared library |
| `;` | Comment | Ignore rest of line |
| `!include "f"` | Include | Preprocessor: splice file contents |

---

## Side-by-Side Examples

### Hello World

**Standard BF:**
```
++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>
---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.
```
71 characters of opaque arithmetic to produce 13 bytes of output.

**BF++:**
```
"Hello, World!\n\0"
<<<<<<<<<<<<<<<
!#.>
```
Three lines. String literal writes bytes directly; stdlib prints them.

### Cat (echo stdin to stdout)

**Standard BF:**
```
,[.,]
```

**BF++ (identical -- full compatibility):**
```
,[.,]
```

### Reading a Number from stdin

**Standard BF:**
```
; Read ASCII digit, convert to numeric value
,                     ; read byte (e.g., '5' = 53)
------------------------------------------------
; subtract 48 to convert ASCII to integer (53 - 48 = 5)
; result: cell contains 5
; Multi-digit? Requires a complex loop to accumulate digits,
; multiply by 10, add next digit -- dozens of operators.
```

**BF++:**
```
!include "io.bfpp"
!#,+                  ; read_int: parses decimal from stdin into cell
; done. Cell contains the parsed integer.
```

### Error Handling

**Standard BF:**
```
; Impossible. No error register, no propagation,
; no try/catch, no subroutines to return from.
; If a syscall fails (which BF can't make anyway),
; there is no mechanism to detect or recover.
```

**BF++:**
```
!#fail{
  ++++++ e            ; set error register to 6 (ERR_INVALID_ARG)
  ^
}

R{
  !#fail              ; try calling fail
}K{
  E                   ; load error code into cell
  ++++++++++++++++++++++++++++++++++++++++++++++++ .
  ; prints '6' (error code as ASCII digit)
}
```

### Writing to a File

**Standard BF:**
```
; Not possible. BF has no file I/O, no syscalls,
; no file descriptors. Only stdin and stdout exist.
```

**BF++:**
```
!include "file.bfpp"

; Open file for writing
< +                   ; flags = 1 (write/create)
> "output.txt\0"
!#fo ?                ; file_open, propagate error

; Write data
$                     ; save fd
"Hello from BF++\0"
> [-] ++++++++++++++++ ; count = 16
<<
~ >                   ; restore fd
!#fw ?                ; file_write, propagate error

; Close
~                     ; fd
!#fc                  ; file_close
```

### TCP Echo Server

**Standard BF:**
```
; Not possible. No networking, no sockets,
; no system calls of any kind.
```

**BF++:**
```
!include "net.bfpp"

; Listen on port 8080
; (port setup omitted for brevity)
!#tl ?                ; tcp_listen

[
  !#ta ?              ; accept client
  $                   ; save client fd

  ~ > [-] ++++++++++++++++++++++++++++++++ > ; recv buffer
  !#tr ?              ; tcp_recv

  ; echo back
  !#ts ?              ; tcp_send

  +                   ; keep looping
]
```
