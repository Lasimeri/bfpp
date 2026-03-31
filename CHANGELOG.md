# Changelog

## [0.2.0] - 2026-03-31

### Added
- **Numeric literals** (`#N`, `#0xHH`): set current cell to immediate value, respects cell width
- **Direct cell width** (`%1`, `%2`, `%4`, `%8`): set cell width without cycling, backward compatible with bare `%`
- **Block comments** (`/* ... */`): nestable, unterminated = lex error
- **Compiler intrinsics** (`__` prefix): inline C emission for system integration
  - Terminal: `__term_raw`, `__term_restore`, `__term_size`, `__term_alt_on/off`, `__term_mouse_on/off`
  - Time: `__sleep`, `__time_ms`
  - Environment: `__getenv`
  - Process: `__exit`, `__getpid`
  - I/O: `__poll_stdin`
  - TUI runtime: `__tui_init`, `__tui_cleanup`, `__tui_size`, `__tui_begin`, `__tui_end`, `__tui_put`, `__tui_puts`, `__tui_fill`, `__tui_box`, `__tui_key`
- **C runtime library** (`runtime/bfpp_rt.{h,c}`): double-buffered TUI renderer
  - UTF-8 cell storage for Unicode box drawing
  - Raw terminal mode with atexit crash safety
  - Alternate screen buffer
  - Key polling with escape sequence parsing (arrows, pgup/dn, home/end, delete)
  - Three box styles: ASCII, Unicode single-line, Unicode rounded
  - 256-color support
- **stdlib/graphics.bfpp**: SDL2 framebuffer drawing primitives
  - `!#px` set_pixel, `!#gx` get_pixel, `!#gc` clear_framebuffer
  - `!#fl` fill_rect, `!#rc` draw_rect, `!#hl` draw_hline, `!#vl` draw_vline
- **examples/intrinsics_demo.bfpp**: demonstrates __getenv, __getpid, __time_ms, __sleep, __exit
- **examples/tui_demo.bfpp**: rewritten with #N and tui.bfpp stdlib

### Changed
- All stdlib modules rewritten using `#N` and `%N` operators
- `stdlib/file.bfpp`: self-contained open/read/write/close with internal syscall frame setup
- `stdlib/net.bfpp`: full TCP stack — socket/connect/listen/accept/send/recv with sockaddr_in construction
- `stdlib/tui.bfpp`: 10 functions, arbitrary cursor positioning, 256-color, box drawing
- `stdlib/mem.bfpp`: working malloc (bump allocator), memcpy and memset via `*$`/`*~` indirect access
- `stdlib/err.bfpp`: proper panic via `#60` (SYS_exit), error-to-string via print_int
- `stdlib/io.bfpp`: print_int rewritten with stack-based digit extraction, read_int handles multi-digit input

### Fixed
- Preprocessor escaped quote bug (backslash counting)
- Standalone `R`/`K` without `{` now errors instead of silently dropping
- Multi-byte cell width overlap protection (continuation byte sentinel)
- Framebuffer offset dynamic (`BFPP_FB_OFFSET` at tape end, not hardcoded `0xA000`)
- SDL2 init failure no longer segfaults (null pointer guard on renderer)
- `return` in main emits `return 0;` instead of `return;` (C11 UB fix)
- `?` (Propagate) inside `R{}` block breaks to `K{}` instead of returning from subroutine
- Call stack depth enforced (guard in subroutine prologue)
- String literal pointer advance now masked with `TAPE_MASK`
- MultiplyMove factor type changed from u8 to usize (no overflow on >255)
- Duplicate multiply-move offsets merged
- Power-of-2 tape size validation
- Framebuffer bounds validation
- Error code constants used via C `#define`s (no magic numbers in generated C)
- All clippy warnings resolved
- `Cargo.toml` edition fixed from "2024" to "2021"

### Tests
- 72 unit tests + 9 integration tests = 81 total, all passing
- Zero compiler warnings, zero clippy warnings

## [0.1.0] - 2026-02-15

### Added
- Initial BF++ transpiler: preprocess, lex, parse, analyze, optimize, codegen pipeline
- 30+ operators: core BF, extended memory, stack, subroutines, syscalls, bitwise, error handling, FFI, framebuffer
- 8 stdlib modules: io, math, string, mem, err, file, net, tui
- Peephole optimizer: clear loop, scan loop, multiply-move, error folding
- Integration test suite with bash runner
- Benchmark suite with hyperfine support
- Spec documentation
