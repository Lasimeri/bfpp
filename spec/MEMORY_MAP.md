# BF++ Memory Map

**Version**: 0.1.0

---

## Overview

BF++ uses a flat tape with conventionally designated regions. The tape is a contiguous byte array; region boundaries are conventions enforced by the standard library, not hardware.

---

## Default Memory Layout (64 KB tape)

```
0x0000 ┌──────────────────────────────────┐
       │                                  │
       │       General Purpose            │
       │       (32,768 bytes)             │
       │                                  │
       │  User data, strings, variables,  │
       │  computation workspace           │
       │                                  │
0x7FFF ├──────────────────────────────────┤
0x8000 │                                  │
       │       Syscall Parameters         │
       │       (256 bytes)                │
       │                                  │
       │  Syscall number + up to 6 args   │
       │  (64-bit each = 56 bytes used)   │
       │                                  │
0x80FF ├──────────────────────────────────┤
0x8100 │                                  │
       │       I/O Buffer                 │
       │       (3,840 bytes)              │
       │                                  │
       │  Buffered read/write staging     │
       │  Used by stdlib file/net ops     │
       │                                  │
0x8FFF ├──────────────────────────────────┤
0x9000 │                                  │
       │       Reserved                   │
       │       (4,096 bytes)              │
       │                                  │
       │  Future use (heap metadata,      │
       │  thread-local storage, etc.)     │
       │                                  │
0x9FFF ├──────────────────────────────────┤
0xA000 │                                  │
       │       Framebuffer                │
       │       (24,576 bytes)             │
       │                                  │
       │  Pixel data (when --framebuffer  │
       │  is enabled). RGB888 format.     │
       │  Unused if framebuffer disabled. │
       │                                  │
0xFFFF └──────────────────────────────────┘
```

---

## Region Details

### General Purpose (0x0000–0x7FFF)

- 32 KB of unrestricted user memory
- Pointer starts at 0x0000
- No enforced structure — programs organize this freely
- Standard library string/math routines operate within this region by convention

### Syscall Parameters (0x8000–0x80FF)

- 256 bytes reserved for syscall setup
- Layout when using 64-bit cells:

| Offset | Size | Content |
|--------|------|---------|
| 0x8000 | 8 bytes | Syscall number |
| 0x8008 | 8 bytes | Argument 1 |
| 0x8010 | 8 bytes | Argument 2 |
| 0x8018 | 8 bytes | Argument 3 |
| 0x8020 | 8 bytes | Argument 4 |
| 0x8028 | 8 bytes | Argument 5 |
| 0x8030 | 8 bytes | Argument 6 |
| 0x8038 | 8 bytes | Return value (written by syscall) |
| 0x8040–0x80FF | 192 bytes | Scratch space for complex syscall args |

- Programs should `@` to 0x8000, set `%` to 64-bit, then populate args before `\`

### I/O Buffer (0x8100–0x8FFF)

- 3,840 bytes for buffered I/O
- Standard library `file.bfpp` and `net.bfpp` use this region for read/write buffers
- Layout is stdlib-managed:
  - 0x8100–0x87FF: Read buffer (1,792 bytes)
  - 0x8800–0x8FFF: Write buffer (2,048 bytes)

### Reserved (0x9000–0x9FFF)

- 4 KB reserved for future use
- Potential uses: heap metadata for `malloc`/`free`, thread-local storage, debug info

### Framebuffer (0xA000–0xFFFF)

- 24,576 bytes for pixel data
- Only active when `--framebuffer WxH` flag is passed to compiler
- RGB888 format: 3 bytes per pixel (R, G, B)
- Maximum resolution at default tape size: ~90x90 pixels (8,192 pixels)
- Larger resolutions require `--tape-size` increase
- Runtime flushes this region to an SDL surface each frame

---

## Data Stack (Separate Region)

- Not part of the tape
- Default: 4,096 entries, 64 bits each (32 KB)
- Accessed only via `$` (push) and `~` (pop)
- Stack pointer starts at 0, grows upward

---

## Call Stack (Separate Region)

- Not part of the tape
- Default: 256 frames
- Each frame stores:
  - Return instruction index (64-bit)
  - Saved error register value (64-bit)
- Managed internally by subroutine call/return

---

## Cell Width Metadata (Separate Region)

- Parallel array to tape, 1 byte per cell
- Values: 1 (8-bit), 2 (16-bit), 4 (32-bit), 8 (64-bit)
- Default: all cells start as 8-bit (value 1)
- Modified by `%` operator

---

## Custom Tape Sizes

When `--tape-size N` is used:

- General purpose region scales proportionally
- Syscall params region stays at 256 bytes
- I/O buffer stays at 3,840 bytes
- If `N < 0x8100`, syscall and I/O regions are not available (bare BF mode)
- If `--framebuffer` is enabled and `N < 0xA000 + (W*H*3)`, compile-time error
