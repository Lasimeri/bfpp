# BF++ Standard Library Reference

**Version**: 0.1.0

---

## Overview

The BF++ standard library is written in BF++ itself. Each module defines subroutines using `!#name{...}` syntax. Programs include stdlib modules via the `--include` compiler flag or by placing them in the search path.

All stdlib subroutines follow the convention:
- **Arguments**: placed in tape cells at current `ptr` before call
- **Return value**: left in `tape[ptr]` after return
- **Errors**: set via error register; callers should use `?` or `R{...}K{...}`

---

## Module: `io.bfpp`

Basic I/O operations on stdin/stdout.

| Subroutine | Symbol | Args | Returns | Description |
|------------|--------|------|---------|-------------|
| print_string | `!#.>` | `ptr` → null-terminated string | — | Print string at current pointer to stdout. Advances ptr to null terminator. |
| print_int | `!#.+` | `tape[ptr]` = integer value | — | Print cell value as decimal ASCII to stdout. |
| read_line | `!#,<` | `ptr` → buffer start | `tape[ptr]` = bytes read | Read from stdin until newline or EOF. Writes to tape at ptr. Null-terminates. |
| read_int | `!#,+` | — | `tape[ptr]` = parsed integer | Read decimal integer from stdin into current cell. |

---

## Module: `file.bfpp`

File operations built on syscall bridge.

| Subroutine | Symbol | Args | Returns | Errors |
|------------|--------|------|---------|--------|
| file_open | `!#fo` | `ptr` → null-terminated path, `tape[ptr-1]` = flags (0=read, 1=write, 2=append) | `tape[ptr]` = fd | 2 (not found), 3 (permission) |
| file_read | `!#fr` | `tape[ptr]` = fd, `tape[ptr+1]` = count, `ptr+2` → buffer | `tape[ptr]` = bytes read | 6 (invalid fd), 15 (I/O) |
| file_write | `!#fw` | `tape[ptr]` = fd, `tape[ptr+1]` = count, `ptr+2` → data | `tape[ptr]` = bytes written | 6 (invalid fd), 15 (I/O) |
| file_close | `!#fc` | `tape[ptr]` = fd | — | 6 (invalid fd) |

**Flags for file_open**:

| Value | Mode |
|-------|------|
| 0 | Read only (`O_RDONLY`) |
| 1 | Write only, create/truncate (`O_WRONLY \| O_CREAT \| O_TRUNC`) |
| 2 | Append (`O_WRONLY \| O_CREAT \| O_APPEND`) |
| 3 | Read/write (`O_RDWR`) |

---

## Module: `net.bfpp`

TCP networking operations.

| Subroutine | Symbol | Args | Returns | Errors |
|------------|--------|------|---------|--------|
| tcp_connect | `!#tcp` | `ptr` → null-terminated host, `tape[ptr-2..ptr-1]` = port (16-bit) | `tape[ptr]` = socket fd | 5 (conn refused), 7 (timeout) |
| tcp_listen | `!#tl` | `tape[ptr..ptr+1]` = port (16-bit), `tape[ptr+2]` = backlog | `tape[ptr]` = server fd | 12 (addr in use) |
| tcp_accept | `!#ta` | `tape[ptr]` = server fd | `tape[ptr]` = client fd | 6 (invalid fd) |
| tcp_send | `!#ts` | `tape[ptr]` = fd, `tape[ptr+1]` = count, `ptr+2` → data | `tape[ptr]` = bytes sent | 10 (pipe), 11 (conn reset) |
| tcp_recv | `!#tr` | `tape[ptr]` = fd, `tape[ptr+1]` = max count, `ptr+2` → buffer | `tape[ptr]` = bytes received | 11 (conn reset) |

---

## Module: `string.bfpp`

Null-terminated string operations.

| Subroutine | Symbol | Args | Returns | Description |
|------------|--------|------|---------|-------------|
| strlen | `!#sl` | `ptr` → string | `tape[ptr]` = length | Count bytes until null terminator |
| strcmp | `!#sc` | `ptr` → string A, `tape[ptr-2..ptr-1]` = address of string B | `tape[ptr]` = result (0=equal, 1=A>B, 255=A<B) | Lexicographic comparison |
| strcpy | `!#sy` | `ptr` → dest, `tape[ptr-2..ptr-1]` = src address | — | Copy string from src to dest (including null) |
| strcat | `!#sa` | `ptr` → dest (existing string), `tape[ptr-2..ptr-1]` = src address | — | Append src to dest |

---

## Module: `math.bfpp`

Multi-byte arithmetic (works with current cell width).

| Subroutine | Symbol | Args | Returns | Description |
|------------|--------|------|---------|-------------|
| multiply | `!#m*` | `tape[ptr]` = A, `tape[ptr+1]` = B | `tape[ptr]` = A * B | Unsigned multiplication |
| divide | `!#m/` | `tape[ptr]` = A, `tape[ptr+1]` = B | `tape[ptr]` = A / B | Unsigned division. Error 6 if B=0. |
| modulo | `!#m%` | `tape[ptr]` = A, `tape[ptr+1]` = B | `tape[ptr]` = A % B | Unsigned modulo. Error 6 if B=0. |
| power | `!#m^` | `tape[ptr]` = base, `tape[ptr+1]` = exp | `tape[ptr]` = base^exp | Unsigned exponentiation |

---

## Module: `mem.bfpp`

Memory management within the tape.

| Subroutine | Symbol | Args | Returns | Description |
|------------|--------|------|---------|-------------|
| memcpy | `!#mc` | `tape[ptr]` = dest addr, `tape[ptr+1]` = src addr, `tape[ptr+2]` = count | — | Copy count bytes from src to dest |
| memset | `!#ms` | `tape[ptr]` = dest addr, `tape[ptr+1]` = value, `tape[ptr+2]` = count | — | Fill count bytes at dest with value |
| malloc | `!#ma` | `tape[ptr]` = size | `tape[ptr]` = allocated address | Simple bump allocator in reserved region. Error 4 if OOM. |
| free | `!#mf` | `tape[ptr]` = address | — | Mark region as free (best-effort) |

**Note**: `malloc`/`free` use a simple allocator within the general-purpose region. No compaction or defragmentation. Suitable for small, bounded allocations.

---

## Module: `tui.bfpp`

Terminal UI via ANSI escape sequences.

| Subroutine | Symbol | Args | Returns | Description |
|------------|--------|------|---------|-------------|
| cursor_move | `!#cm` | `tape[ptr]` = row, `tape[ptr+1]` = col | — | Move cursor to (row, col) using `\e[row;colH` |
| clear | `!#cl` | — | — | Clear screen using `\e[2J\e[H` |
| set_color | `!#co` | `tape[ptr]` = color code (ANSI 0-7 fg, 8-15 bg) | — | Set terminal color |
| draw_box | `!#db` | `tape[ptr]` = row, `[ptr+1]` = col, `[ptr+2]` = width, `[ptr+3]` = height | — | Draw box using Unicode box-drawing chars |

**Color codes**:

| Value | Color (FG) | Value | Color (BG) |
|-------|------------|-------|------------|
| 0 | Black | 8 | Black BG |
| 1 | Red | 9 | Red BG |
| 2 | Green | 10 | Green BG |
| 3 | Yellow | 11 | Yellow BG |
| 4 | Blue | 12 | Blue BG |
| 5 | Magenta | 13 | Magenta BG |
| 6 | Cyan | 14 | Cyan BG |
| 7 | White | 15 | White BG |

---

## Module: `err.bfpp`

Error handling utilities.

| Subroutine | Symbol | Args | Returns | Description |
|------------|--------|------|---------|-------------|
| err_to_string | `!#es` | Error code in error register | `ptr` → error description string (written to tape) | Convert error code to human-readable string |
| panic | `!#ep` | `ptr` → message string (optional) | Does not return | Print error message to stderr, exit with code 1 |
| assert | `!#ea` | `tape[ptr]` = condition | — | If condition == 0, panic with "assertion failed" |
