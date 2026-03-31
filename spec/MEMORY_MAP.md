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
       │       Framebuffer (if enabled)   │
       │       (W * H * 3 bytes)          │
       │                                  │
       │  Pixel data (when --framebuffer  │
       │  is enabled). RGB888 format.     │
       │  BFPP_FB_OFFSET is dynamic:      │
       │  TAPE_SIZE - (W * H * 3).        │
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

### Reserved / Heap Metadata (0x9000–0x9FFF)

- 4 KB reserved for heap metadata and future use
- **Heap allocator metadata** (used by `mem.bfpp`):
  - `tape[0x9000]` (16-bit): next-free pointer for the bump allocator
  - Initialized to `0x1000` by `!#mi` (heap_init)
  - Allocatable range: `0x1000`–`0xFFFF` (or up to framebuffer offset if enabled)
- Remaining space (0x9002–0x9FFF): reserved for future use (thread-local storage, debug info, etc.)

### Framebuffer (dynamic offset–end of tape)

- Pixel data region for SDL2 framebuffer rendering
- Only active when `--framebuffer WxH` flag is passed to compiler
- RGB888 format: 3 bytes per pixel (R, G, B)
- **Dynamic offset**: `BFPP_FB_OFFSET = TAPE_SIZE - (WIDTH * HEIGHT * 3)`. The framebuffer is placed at the *end* of the tape, not at a hardcoded address. For the default 64 KB tape with 320x200 resolution: `65536 - (320 * 200 * 3) = 65536 - 192000` — which exceeds the default tape size, requiring `--tape-size` increase.
- For a 320x200 framebuffer: requires `--tape-size` of at least 192000 + general purpose space. `BFPP_FB_OFFSET` would be e.g. `262144 - 192000 = 70144 (0x11200)` with a 256 KB tape.
- The legacy address `0xA000` shown in the overview table is a convention for the default 64 KB tape with small framebuffers (e.g., 90x90). Always use `BFPP_FB_OFFSET` (a C `#define` in generated code) rather than hardcoding an address.
- BF++ source code cannot reference C `#define` values directly. Callers must pass the framebuffer offset as a parameter (e.g., `#40960` for 0xA000) or compute it from known tape size and resolution.
- Runtime flushes this region to an SDL texture each frame via the `F` operator

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
