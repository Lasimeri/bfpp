// BF++ Code Generator — AST → C source.
//
// Strategy: walks the BF++ AST and emits a single-file C program that
// includes its own runtime (tape, pointer, stack, cell-width system, error
// handling, optional SDL framebuffer, optional FFI via dlopen/dlsym).
// The generated C is self-contained — no external BF++ runtime library.
// Subroutines are lifted to C functions with forward declarations;
// everything else lands in main().

use crate::ast::{AstNode, FdSpec, Program};

pub struct CodegenOptions {
    pub tape_size: usize,   // number of bytes in the BF tape (should be power-of-two for TAPE_MASK)
    pub stack_size: usize,  // max depth of the BF++ value stack ($~ operations)
    pub call_depth: usize,  // max recursive subroutine call depth before abort
    pub framebuffer: Option<(u32, u32)>, // if Some((w,h)), emit SDL2 framebuffer support
    pub eof_value: u8,      // value written to cell on EOF during input (`,`)
    pub render_threads: usize, // number of render threads for framebuffer pipeline (default 8)
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            tape_size: 65536,
            stack_size: 4096,
            call_depth: 256,
            framebuffer: None,
            eof_value: 0,
            render_threads: 8,
        }
    }
}

/// Result of code generation: C source and metadata flags.
/// The metadata fields inform the compiler driver (main.rs) which
/// libraries to link and which runtime files to compile alongside.
pub struct CodegenResult {
    pub c_source: String,
    /// True if the program contains any \ffi calls — triggers -ldl linking
    pub uses_ffi: bool,
    /// True if any __tui_* intrinsics are used — triggers bfpp_rt.c compilation
    pub uses_tui_runtime: bool,
    /// True if framebuffer pipeline is used — triggers bfpp_fb_pipeline.c compilation
    pub uses_fb_pipeline: bool,
    /// True if any threading intrinsics are used — triggers -pthread and bfpp_rt_parallel.c
    pub uses_threading: bool,
    /// True if any 3D intrinsics are used — triggers bfpp_rt_3d*.c compilation + GL/GLEW linking
    pub uses_3d: bool,
    /// True if multi-GPU/oracle intrinsics used — triggers bfpp_rt_3d_multigpu/oracle + EGL
    pub uses_multigpu: bool,
}

pub fn generate(program: &Program, opts: &CodegenOptions) -> CodegenResult {
    // Pre-scan for FFI usage so we know whether to #include <dlfcn.h>
    let uses_ffi = program_uses_ffi(&program.nodes);
    // Pre-scan for intrinsic usage so emit_header knows which C headers,
    // state variables, and constructors to include (e.g., termios for __term_*,
    // time.h for __sleep, bfpp_rt.h for __tui_*)
    let intrinsics = detect_intrinsics(&program.nodes);
    let uses_tui_runtime = intrinsics.tui;
    let uses_fb_pipeline = opts.framebuffer.is_some();
    let uses_threading = intrinsics.threading;
    let uses_3d = intrinsics.gl3d;
    let uses_multigpu = intrinsics.multigpu;
    let c_source = generate_c(program, opts, uses_ffi, &intrinsics);
    CodegenResult { c_source, uses_ffi, uses_tui_runtime, uses_fb_pipeline, uses_threading, uses_3d, uses_multigpu }
}

// Recursively checks whether any node in the AST uses \ffi calls.
// Needed at codegen time to conditionally emit the dlfcn.h include and
// set the uses_ffi metadata flag (which tells the compiler driver to link -ldl).
fn program_uses_ffi(nodes: &[AstNode]) -> bool {
    for node in nodes {
        match node {
            AstNode::FfiCall(_, _) => return true,
            AstNode::Loop(body) | AstNode::SubDef(_, body) => {
                if program_uses_ffi(body) { return true; }
            }
            AstNode::ResultBlock(r, k) => {
                if program_uses_ffi(r) || program_uses_ffi(k) { return true; }
            }
            AstNode::Deref(inner) => {
                if program_uses_ffi(&[*inner.clone()]) { return true; }
            }
            _ => {}
        }
    }
    false
}

// Main C generation pipeline: header → forward decls → subroutine bodies → main().
fn generate_c(program: &Program, opts: &CodegenOptions, uses_ffi: bool, intrinsics: &IntrinsicUsage) -> String {
    let mut out = String::new();
    let mut ctx = GenCtx {
        indent: 1,
        subroutines: Vec::new(),
        eof_value: opts.eof_value,
        in_subroutine: false,
        in_result_block: false,
    };

    // First pass: collect subroutine names for forward declarations.
    // Only scans top-level nodes — BF++ subroutine defs are always top-level.
    collect_subroutines(&program.nodes, &mut ctx.subroutines);

    // Emit the C runtime header: includes, #defines, static globals,
    // helper functions (bfpp_get/set/push/pop/cycle_width, errno mapping,
    // syscall wrapper, constructor, and optional SDL framebuffer).
    out.push_str(&emit_header(opts, uses_ffi, intrinsics));

    // Forward-declare all subroutines so they can call each other
    // regardless of definition order (mutual recursion).
    for name in &ctx.subroutines {
        out.push_str(&format!("void bfpp_sub_{}(void);\n", mangle_name(name)));
    }
    if !ctx.subroutines.is_empty() {
        out.push('\n');
    }

    // Subroutine table: static array of function pointers indexed by position.
    // Used by __spawn to look up subroutine entry points at runtime.
    if (intrinsics.threading || intrinsics.indirect_call) && !ctx.subroutines.is_empty() {
        out.push_str("static void (*bfpp_sub_table[])(void) = {\n");
        for name in &ctx.subroutines {
            out.push_str(&format!("    bfpp_sub_{},\n", mangle_name(name)));
        }
        out.push_str("};\n\n");
    }

    // Emit subroutine bodies. Each gets a call-depth guard (prologue/epilogue)
    // to enforce the CALL_DEPTH limit and prevent unbounded recursion from
    // blowing the C stack. The prologue increments+checks; the epilogue decrements.
    // Return (^) inside a subroutine also decrements before returning.
    for node in &program.nodes {
        if let AstNode::SubDef(name, body) = node {
            out.push_str(&format!("void bfpp_sub_{}(void) {{\n", mangle_name(name)));
            ctx.indent = 1;
            ctx.in_subroutine = true;
            // Prologue: increment call depth and abort on overflow
            indent(&mut out, ctx.indent);
            out.push_str("if (++bfpp_call_depth > CALL_DEPTH) { fprintf(stderr, \"bfpp: call stack overflow\\n\"); exit(1); }\n");
            emit_nodes(&mut out, body, &mut ctx);
            // Epilogue: decrement call depth on normal fall-through
            indent(&mut out, ctx.indent);
            out.push_str("bfpp_call_depth--;\n");
            out.push_str("}\n\n");
        }
    }
    ctx.in_subroutine = false;

    // Emit main
    out.push_str("int main(void) {\n");
    ctx.indent = 1;

    if opts.framebuffer.is_some() {
        indent(&mut out, ctx.indent);
        out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
        indent(&mut out, ctx.indent);
        out.push_str("bfpp_fb_pipeline_init(BFPP_FB_WIDTH, BFPP_FB_HEIGHT, tape, BFPP_FB_OFFSET);\n");
        indent(&mut out, ctx.indent);
        out.push_str("#endif\n");
    }

    emit_nodes_skip_subdefs(&mut out, &program.nodes, &mut ctx);

    if opts.framebuffer.is_some() {
        indent(&mut out, ctx.indent);
        out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
        indent(&mut out, ctx.indent);
        out.push_str("bfpp_fb_pipeline_cleanup();\n");
        indent(&mut out, ctx.indent);
        out.push_str("#endif\n");
    }

    indent(&mut out, ctx.indent);
    out.push_str("return 0;\n");
    out.push_str("}\n");

    out
}

// Codegen context — threaded through all emit_* functions.
//
// - indent: current C indentation level (number of 4-space tabs)
// - subroutines: collected subroutine names (for forward decls)
// - eof_value: what `,` writes on EOF (user-configurable)
// - in_subroutine: true when emitting inside a subroutine body.
//   Affects Return (^ emits `bfpp_call_depth--; return;` vs `return 0;`)
//   and Propagate (? must decrement call depth before returning).
// - in_result_block: true when inside the R{...} portion of an R/K block.
//   Propagate (?) emits `break;` instead of `return;` so control
//   transfers to the K (catch) block via the do/while(0) wrapper.
struct GenCtx {
    indent: usize,
    subroutines: Vec<String>,
    eof_value: u8,
    in_subroutine: bool,
    in_result_block: bool,
}

fn collect_subroutines(nodes: &[AstNode], names: &mut Vec<String>) {
    for node in nodes {
        if let AstNode::SubDef(name, _) = node {
            names.push(name.clone());
        }
    }
}

// Emits the entire C runtime that precedes main(). This is the "header" of
// the generated file — includes, defines, static state, and helper functions.
// Everything a BF++ program needs at runtime is generated here so the output
// is a single self-contained .c file.
fn emit_header(opts: &CodegenOptions, uses_ffi: bool, intrinsics: &IntrinsicUsage) -> String {
    let mut h = String::new();
    h.push_str("/* Generated by BF++ transpiler */\n");

    // Standard C + POSIX headers for I/O, memory, networking, and syscalls.
    // Always included because the runtime uses them unconditionally
    // (syscall wrapper, errno mapping, socket ops).
    h.push_str("#include <stdio.h>\n");
    h.push_str("#include <stdlib.h>\n");
    h.push_str("#include <string.h>\n");
    h.push_str("#include <stdint.h>\n");
    h.push_str("#include <errno.h>\n");
    h.push_str("#include <unistd.h>\n");
    h.push_str("#include <sys/types.h>\n");
    h.push_str("#include <sys/stat.h>\n");
    h.push_str("#include <fcntl.h>\n");
    h.push_str("#include <sys/socket.h>\n");
    h.push_str("#include <netinet/in.h>\n");
    h.push_str("#include <arpa/inet.h>\n");
    h.push_str("#include <sys/syscall.h>\n");
    // dlfcn.h only if the program uses \ffi — avoids unnecessary dep
    if uses_ffi {
        h.push_str("#include <dlfcn.h>\n");
    }
    // Intrinsic-specific headers — only included when the corresponding
    // intrinsic category is detected. Keeps the generated C minimal and
    // avoids requiring headers that the system might not have (e.g., poll.h
    // on exotic targets).
    if intrinsics.terminal {
        h.push_str("#include <termios.h>\n");   // tcgetattr/tcsetattr for raw mode
        h.push_str("#include <sys/ioctl.h>\n"); // ioctl(TIOCGWINSZ) for terminal size
    }
    if intrinsics.time {
        h.push_str("#include <time.h>\n");      // clock_gettime for __time_ms
    }
    if intrinsics.poll {
        h.push_str("#include <poll.h>\n");      // poll() for __poll_stdin
    }
    if intrinsics.tui {
        // TUI runtime provides double-buffered cell grid, box drawing, and
        // non-blocking key input. Compiled as a separate .c file by the driver.
        h.push_str("#include \"bfpp_rt.h\"\n");
    }
    if intrinsics.threading {
        // Parallel runtime: thread pool, mutexes, barriers, atomics.
        h.push_str("#include <pthread.h>\n");
        h.push_str("#include <stdatomic.h>\n");
        h.push_str("#include <sched.h>\n");
        h.push_str("#include \"bfpp_rt_parallel.h\"\n");
    }
    if intrinsics.gl3d {
        // 3D rendering: OpenGL proxy layer, fixed-point math, mesh generators, software fallback.
        h.push_str("#include \"bfpp_rt_3d.h\"\n");
    }
    if intrinsics.multigpu {
        // Multi-GPU + Scene Oracle: EGL multi-context, SFR/AFR, lock-free triple buffer.
        h.push_str("#include \"bfpp_rt_3d_multigpu.h\"\n");
        h.push_str("#include \"bfpp_rt_3d_oracle.h\"\n");
    }
    h.push('\n');

    // Framebuffer: maps the last W*H*3 bytes of the tape as an RGB24 pixel
    // buffer. BFPP_FB_OFFSET is the tape index where the framebuffer starts.
    // BF++ programs write pixel data there, then `F` flushes to an SDL window.
    // Placed at the tape's end so normal BF pointer movement near cell 0
    // doesn't accidentally clobber the framebuffer region.
    if let Some((w, h_)) = opts.framebuffer {
        h.push_str("#include <SDL2/SDL.h>\n");
        h.push_str("#define BFPP_FRAMEBUFFER\n");
        h.push_str(&format!("#define BFPP_FB_WIDTH {}\n", w));
        h.push_str(&format!("#define BFPP_FB_HEIGHT {}\n", h_));
        h.push_str("#define BFPP_FB_OFFSET (TAPE_SIZE - (BFPP_FB_WIDTH * BFPP_FB_HEIGHT * 3))\n");
    }

    // TAPE_MASK = TAPE_SIZE - 1: used for fast modular wrapping (ptr & TAPE_MASK)
    // instead of modulo. Requires TAPE_SIZE to be a power of two.
    h.push_str(&format!("#define TAPE_SIZE {}\n", opts.tape_size));
    h.push_str("#define TAPE_MASK (TAPE_SIZE - 1)\n");
    h.push_str(&format!("#define STACK_SIZE {}\n", opts.stack_size));
    h.push_str(&format!("#define CALL_DEPTH {}\n", opts.call_depth));
    h.push('\n');

    // Error code #defines — generated from the Rust constants in error_codes.rs.
    // This is the bridge: Rust owns the values, C code references BFPP_ERR_*
    // names. The errno_mapping_c_source() function uses these same numeric
    // values in its switch statement, keeping everything consistent.
    h.push_str(&format!("#define BFPP_OK {}\n", crate::error_codes::OK));
    h.push_str(&format!("#define BFPP_ERR_GENERIC {}\n", crate::error_codes::ERR_GENERIC));
    h.push_str(&format!("#define BFPP_ERR_NOT_FOUND {}\n", crate::error_codes::ERR_NOT_FOUND));
    h.push_str(&format!("#define BFPP_ERR_PERMISSION {}\n", crate::error_codes::ERR_PERMISSION));
    h.push_str(&format!("#define BFPP_ERR_OOM {}\n", crate::error_codes::ERR_OOM));
    h.push_str(&format!("#define BFPP_ERR_CONN_REFUSED {}\n", crate::error_codes::ERR_CONN_REFUSED));
    h.push_str(&format!("#define BFPP_ERR_INVALID_ARG {}\n", crate::error_codes::ERR_INVALID_ARG));
    h.push_str(&format!("#define BFPP_ERR_TIMEOUT {}\n", crate::error_codes::ERR_TIMEOUT));
    h.push_str(&format!("#define BFPP_ERR_EXISTS {}\n", crate::error_codes::ERR_EXISTS));
    h.push_str(&format!("#define BFPP_ERR_BUSY {}\n", crate::error_codes::ERR_BUSY));
    h.push_str(&format!("#define BFPP_ERR_PIPE {}\n", crate::error_codes::ERR_PIPE));
    h.push_str(&format!("#define BFPP_ERR_CONN_RESET {}\n", crate::error_codes::ERR_CONN_RESET));
    h.push_str(&format!("#define BFPP_ERR_ADDR_IN_USE {}\n", crate::error_codes::ERR_ADDR_IN_USE));
    h.push_str(&format!("#define BFPP_ERR_NOT_CONNECTED {}\n", crate::error_codes::ERR_NOT_CONNECTED));
    h.push_str(&format!("#define BFPP_ERR_INTERRUPTED {}\n", crate::error_codes::ERR_INTERRUPTED));
    h.push_str(&format!("#define BFPP_ERR_IO {}\n", crate::error_codes::ERR_IO));
    h.push_str(&format!("#define BFPP_ERR_NOLIB {}\n", crate::error_codes::ERR_NOLIB));
    h.push_str(&format!("#define BFPP_ERR_NOSYM {}\n", crate::error_codes::ERR_NOSYM));
    h.push('\n');

    // Core runtime state — all static globals, zero-initialized by bfpp_init().
    // tape: the BF memory array (byte-addressable, but cell_width[] overlays
    //   multi-byte cells on top via bfpp_get/bfpp_set).
    // ptr: current tape head position.
    // bfpp_err: the global error register (read by `E`, written by `e`).
    // stack/sp: the BF++ value stack ($ pushes, ~ pops). Holds uint64_t values.
    // bfpp_call_depth: subroutine recursion depth counter.
    // cell_width: parallel array — cell_width[i] == 0 means tape[i] is a
    //   continuation byte (part of a wider cell), 1/2/4/8 means tape[i] is
    //   the start of a cell with that many bytes. Acts as a sentinel so
    //   bfpp_get/bfpp_set know the cell's byte span.
    // tape[] is always shared (the inter-thread communication channel).
    // When threading is active:
    //   - Per-thread state (ptr, sp, etc.) uses _Thread_local with EXTERNAL linkage
    //     so bfpp_rt_parallel.c's thread entry wrapper can reset them via extern.
    //   - `static` is dropped — the runtime needs to see these symbols.
    // When threading is NOT active: everything is `static` (internal linkage, single TU).
    h.push_str("static uint8_t tape[TAPE_SIZE];\n");

    if intrinsics.threading {
        // External linkage _Thread_local: runtime's bfpp_thread_entry() references these
        h.push_str("_Thread_local int ptr = 0;\n");
        h.push_str("_Thread_local int bfpp_err = 0;\n");
        h.push_str("_Thread_local uint64_t stack[STACK_SIZE];\n");
        h.push_str("_Thread_local int sp = 0;\n");
        h.push_str("_Thread_local int bfpp_call_depth = 0;\n");
        h.push_str("_Thread_local uint8_t cell_width[TAPE_SIZE];\n");
    } else if intrinsics.gl3d {
        // 3D runtime references bfpp_err via extern — give it external linkage.
        // Other variables stay static (no threading, only bfpp_err crosses TU boundary).
        h.push_str("static int ptr = 0;\n");
        h.push_str("int bfpp_err = 0;\n");
        h.push_str("static uint64_t stack[STACK_SIZE];\n");
        h.push_str("static int sp = 0;\n");
        h.push_str("static int bfpp_call_depth = 0;\n");
        h.push_str("static uint8_t cell_width[TAPE_SIZE]; /* 0=continuation, 1,2,4,8 */\n");
    } else {
        h.push_str("static int ptr = 0;\n");
        h.push_str("static int bfpp_err = 0;\n");
        h.push_str("static uint64_t stack[STACK_SIZE];\n");
        h.push_str("static int sp = 0;\n");
        h.push_str("static int bfpp_call_depth = 0;\n");
        h.push_str("static uint8_t cell_width[TAPE_SIZE]; /* 0=continuation, 1,2,4,8 */\n");
    }

    // Dual-tape state: separate read/write tapes for data transformation.
    // Always static — thread entry doesn't reset these (each subroutine manages its own).
    h.push_str("static uint8_t rtape[TAPE_SIZE];\n");
    h.push_str("static uint8_t wtape[TAPE_SIZE];\n");
    h.push_str("static int rptr = 0;\n");
    h.push_str("static int wptr = 0;\n");
    h.push_str("static uint8_t rcell_width[TAPE_SIZE];\n");
    h.push_str("static uint8_t wcell_width[TAPE_SIZE];\n");
    // Terminal intrinsic state: saved_termios captures the original terminal
    // settings before __term_raw modifies them, so __term_restore can revert.
    // bfpp_term_raw tracks whether we're currently in raw mode to avoid
    // double-restoring (which would be harmless but wasteful).
    if intrinsics.terminal {
        h.push_str("static struct termios bfpp_saved_termios;\n");
        h.push_str("static int bfpp_term_raw = 0;\n");
    }
    h.push('\n');

    // Cell-width-aware accessors. All tape reads/writes go through bfpp_get/set
    // so multi-byte cells (2/4/8 bytes via %) work transparently.
    // cell_width[p] == 0 is the continuation-byte sentinel: accessing it is
    // an error (you landed in the middle of a wider cell). Uses memcpy for
    // safe unaligned access — the tape isn't guaranteed to be naturally aligned.
    //
    // bfpp_push/pop: value stack with bounds checking. Overflow sets ERR_OOM,
    // underflow sets ERR_INVALID_ARG. Both silently return on error (no abort)
    // so the program can check `E` and handle it.
    //
    // bfpp_cycle_width: cycles a cell's width 1→2→4→8→1. Before widening,
    // releases old continuation bytes (restores them to width=1). Then checks
    // that the new sub-cells are all width=1 (available). If any sub-cell is
    // already a continuation byte for another cell or is itself a wide cell,
    // reverts to width=1 and sets ERR_INVALID_ARG. This prevents overlapping
    // multi-byte cells.
    h.push_str(r#"static uint64_t bfpp_get(int p) {
    p &= TAPE_MASK;
    if (cell_width[p] == 0) { bfpp_err = BFPP_ERR_INVALID_ARG; return 0; } /* continuation byte */
    switch (cell_width[p]) {
        case 2: { uint16_t v; memcpy(&v, &tape[p], 2); return v; }
        case 4: { uint32_t v; memcpy(&v, &tape[p], 4); return v; }
        case 8: { uint64_t v; memcpy(&v, &tape[p], 8); return v; }
        default: return tape[p];
    }
}

static void bfpp_set(int p, uint64_t v) {
    p &= TAPE_MASK;
    if (cell_width[p] == 0) { bfpp_err = BFPP_ERR_INVALID_ARG; return; } /* continuation byte */
    switch (cell_width[p]) {
        case 2: { uint16_t t = (uint16_t)v; memcpy(&tape[p], &t, 2); break; }
        case 4: { uint32_t t = (uint32_t)v; memcpy(&tape[p], &t, 4); break; }
        case 8: { memcpy(&tape[p], &v, 8); break; }
        default: tape[p] = (uint8_t)v; break;
    }
}

static void bfpp_push(uint64_t val) {
    if (sp >= STACK_SIZE) { bfpp_err = BFPP_ERR_OOM; return; }
    stack[sp++] = val;
}

static uint64_t bfpp_pop(void) {
    if (sp <= 0) { bfpp_err = BFPP_ERR_INVALID_ARG; return 0; }
    return stack[--sp];
}

static void bfpp_cycle_width(int p) {
    int old_w = cell_width[p];
    int new_w, i;
    /* Release old sub-cells */
    for (i = 1; i < old_w; i++) cell_width[p + i] = 1;
    /* Determine new width */
    switch (old_w) {
        case 1: new_w = 2; break;
        case 2: new_w = 4; break;
        case 4: new_w = 8; break;
        default: new_w = 1; break;
    }
    /* Check sub-cells are available */
    for (i = 1; i < new_w; i++) {
        if (cell_width[p + i] != 1) {
            bfpp_err = BFPP_ERR_INVALID_ARG;
            cell_width[p] = 1; /* revert to 1-byte */
            return;
        }
    }
    cell_width[p] = new_w;
    /* Mark sub-cells as continuation (0) */
    for (i = 1; i < new_w; i++) cell_width[p + i] = 0;
}

"#);

    // errno→bfpp_err mapping function (generated from error_codes.rs)
    h.push_str(crate::error_codes::errno_mapping_c_source());
    h.push('\n');

    // Syscall wrapper: reads syscall number + 6 args from tape[ptr..ptr+48]
    // (each in an 8-byte cell), issues the syscall, writes result back to
    // tape[ptr]. On failure, translates errno to bfpp_err via the mapping
    // function and writes -1 to tape[ptr]. On success, sets bfpp_err = OK.
    h.push_str(r#"static void bfpp_syscall_exec(void) {
    uint64_t num = bfpp_get(ptr);
    uint64_t a1 = bfpp_get(ptr + 8);
    uint64_t a2 = bfpp_get(ptr + 16);
    uint64_t a3 = bfpp_get(ptr + 24);
    uint64_t a4 = bfpp_get(ptr + 32);
    uint64_t a5 = bfpp_get(ptr + 40);
    uint64_t a6 = bfpp_get(ptr + 48);
    long result = syscall(num, a1, a2, a3, a4, a5, a6);
    if (result < 0) {
        bfpp_err = bfpp_errno_to_code(errno);
        bfpp_set(ptr, (uint64_t)(-1));
    } else {
        bfpp_err = BFPP_OK;
        bfpp_set(ptr, (uint64_t)result);
    }
}

/* Constructor: runs before main(). Zeros the tape, sets every cell_width
   entry to 1 (each cell starts as a single byte), and zeros the stack. */
static void __attribute__((constructor)) bfpp_init(void) {
    memset(tape, 0, TAPE_SIZE);
    memset(cell_width, 1, TAPE_SIZE);  /* 1 = independent 1-byte cell */
    memset(stack, 0, sizeof(stack));
}

"#);

    // Terminal intrinsics: save initial termios in a separate constructor
    // that runs after bfpp_init. This captures the terminal state before
    // any raw-mode changes so __term_restore can revert cleanly.
    if intrinsics.terminal {
        h.push_str("static void __attribute__((constructor)) bfpp_term_init(void) {\n");
        h.push_str("    tcgetattr(0, &bfpp_saved_termios);\n");
        h.push_str("}\n\n");
    }

    // SDL2 framebuffer system. Uses the tiled render pipeline (bfpp_fb_pipeline.h/c)
    // for parallel strip processing with triple buffering and cache management.
    // The pipeline spawns a dedicated presenter thread (owns SDL) + N render threads.
    // The F operator becomes non-blocking (sets atomic flag), the presenter thread
    // flushes at vsync cadence. __fb_sync blocks until the next present completes.
    if opts.framebuffer.is_some() {
        h.push_str("#ifdef BFPP_FRAMEBUFFER\n");
        h.push_str("#include \"bfpp_fb_pipeline.h\"\n");
        h.push_str(&format!("#define BFPP_FB_RENDER_THREADS {}\n", opts.render_threads));
        h.push_str("#endif\n\n");
    }

    h
}

fn emit_nodes(out: &mut String, nodes: &[AstNode], ctx: &mut GenCtx) {
    for node in nodes {
        emit_node(out, node, ctx);
    }
}

// Used in main() to skip SubDef nodes — those were already emitted as
// top-level C functions before main(). Everything else is emitted inline.
fn emit_nodes_skip_subdefs(out: &mut String, nodes: &[AstNode], ctx: &mut GenCtx) {
    for node in nodes {
        if matches!(node, AstNode::SubDef(_, _)) {
            continue;
        }
        emit_node(out, node, ctx);
    }
}

// Per-node C emission. Each AstNode maps to one or more lines of C code.
// The ctx tracks indentation and context flags that alter codegen behavior
// (in_subroutine, in_result_block).
fn emit_node(out: &mut String, node: &AstNode, ctx: &mut GenCtx) {
    match node {
        AstNode::MoveRight(n) => {
            indent(out, ctx.indent);
            out.push_str(&format!("ptr = (ptr + {}) & TAPE_MASK;\n", n));
        }
        AstNode::MoveLeft(n) => {
            indent(out, ctx.indent);
            out.push_str(&format!("ptr = (ptr - {} + TAPE_SIZE) & TAPE_MASK;\n", n));
        }
        AstNode::Increment(n) => {
            indent(out, ctx.indent);
            if *n == 1 {
                out.push_str("bfpp_set(ptr, bfpp_get(ptr) + 1);\n");
            } else {
                out.push_str(&format!("bfpp_set(ptr, bfpp_get(ptr) + {});\n", n));
            }
        }
        AstNode::Decrement(n) => {
            indent(out, ctx.indent);
            if *n == 1 {
                out.push_str("bfpp_set(ptr, bfpp_get(ptr) - 1);\n");
            } else {
                out.push_str(&format!("bfpp_set(ptr, bfpp_get(ptr) - {});\n", n));
            }
        }
        AstNode::Output => {
            indent(out, ctx.indent);
            out.push_str("putchar((int)bfpp_get(ptr));\n");
        }
        AstNode::Input => {
            indent(out, ctx.indent);
            out.push_str(&format!(
                "{{ int c = getchar(); bfpp_set(ptr, c == EOF ? {} : (uint64_t)c); }}\n",
                ctx.eof_value
            ));
        }
        AstNode::Loop(body) => {
            indent(out, ctx.indent);
            out.push_str("while (bfpp_get(ptr)) {\n");
            ctx.indent += 1;
            emit_nodes(out, body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // Extended memory
        AstNode::AbsoluteAddr => {
            indent(out, ctx.indent);
            out.push_str("ptr = (int)bfpp_get(ptr) & TAPE_MASK;\n");
        }
        // Dereference: saves ptr, jumps to the address stored in the current
        // cell, executes the inner operation there, then restores ptr. This is
        // what makes pointer-indirect operations possible in a single-pointer
        // architecture.
        AstNode::Deref(inner) => {
            indent(out, ctx.indent);
            out.push_str("{ int saved_ptr = ptr; ptr = (int)bfpp_get(ptr) & TAPE_MASK;\n");
            ctx.indent += 1;
            emit_node(out, inner, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("ptr = saved_ptr; }\n");
        }
        AstNode::CellWidthCycle => {
            indent(out, ctx.indent);
            out.push_str("bfpp_cycle_width(ptr);\n");
        }
        AstNode::StringLit(bytes) => {
            indent(out, ctx.indent);
            out.push_str("{\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str("static const uint8_t str_data[] = {");
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(&format!("{}", b));
            }
            out.push_str("};\n");
            indent(out, ctx.indent);
            out.push_str(&format!("memcpy(&tape[ptr], str_data, {});\n", bytes.len()));
            indent(out, ctx.indent);
            out.push_str(&format!("ptr = (ptr + {}) & TAPE_MASK;\n", bytes.len()));
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // Stack
        AstNode::Push => {
            indent(out, ctx.indent);
            out.push_str("bfpp_push(bfpp_get(ptr));\n");
        }
        AstNode::Pop => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_pop());\n");
        }

        // Subroutines
        AstNode::SubDef(_, _) => {
            // Handled at top level — skip in-line
        }
        // SubCall: names starting with "__" are compiler intrinsics (emitted
        // as inline C), everything else is a regular subroutine call.
        AstNode::SubCall(name) => {
            if name.starts_with("__") {
                emit_intrinsic(out, name, ctx);
            } else {
                indent(out, ctx.indent);
                out.push_str(&format!("bfpp_sub_{}();\n", mangle_name(name)));
            }
        }
        // Return (^) emits different code depending on context:
        // - In a subroutine: must decrement call depth before returning (the
        //   normal epilogue won't run since we're returning early).
        // - In main: emits `return 0;` to exit the program.
        AstNode::Return => {
            indent(out, ctx.indent);
            if ctx.in_subroutine {
                out.push_str("bfpp_call_depth--; return;\n");
            } else {
                out.push_str("return 0;\n");
            }
        }

        // Syscall
        AstNode::Syscall => {
            indent(out, ctx.indent);
            out.push_str("bfpp_syscall_exec();\n");
        }
        AstNode::OutputFd(fd) => {
            indent(out, ctx.indent);
            match fd {
                FdSpec::Literal(n) => {
                    out.push_str(&format!(
                        "{{ uint8_t b = (uint8_t)bfpp_get(ptr); write({}, &b, 1); }}\n", n
                    ));
                }
                FdSpec::Indirect => {
                    out.push_str(
                        "{ uint8_t b = (uint8_t)bfpp_get(ptr); write((int)bfpp_get(ptr+1), &b, 1); }\n"
                    );
                }
            }
        }
        AstNode::InputFd(fd) => {
            indent(out, ctx.indent);
            match fd {
                FdSpec::Literal(n) => {
                    out.push_str(&format!(
                        "{{ uint8_t b; if (read({}, &b, 1) == 1) bfpp_set(ptr, b); else bfpp_set(ptr, {}); }}\n",
                        n, ctx.eof_value
                    ));
                }
                FdSpec::Indirect => {
                    out.push_str(&format!(
                        "{{ uint8_t b; if (read((int)bfpp_get(ptr+1), &b, 1) == 1) bfpp_set(ptr, b); else bfpp_set(ptr, {}); }}\n",
                        ctx.eof_value
                    ));
                }
            }
        }

        // Bitwise
        AstNode::BitOr => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_get(ptr) | bfpp_get(ptr+1));\n");
        }
        AstNode::BitAnd => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_get(ptr) & bfpp_get(ptr+1));\n");
        }
        AstNode::BitXor => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_get(ptr) ^ bfpp_get(ptr+1));\n");
        }
        AstNode::ShiftLeft => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_get(ptr) << bfpp_get(ptr+1));\n");
        }
        AstNode::ShiftRight => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_get(ptr) >> bfpp_get(ptr+1));\n");
        }
        AstNode::BitNot => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, ~bfpp_get(ptr));\n");
        }

        // Error handling
        AstNode::ErrorRead => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)bfpp_err);\n");
        }
        AstNode::ErrorWrite => {
            indent(out, ctx.indent);
            out.push_str("bfpp_err = (int)bfpp_get(ptr);\n");
        }
        // Propagate (?) — early-exit on error. Three behaviors:
        // 1. Inside R{...} block: `break;` — exits the do/while(0) wrapper,
        //    falling through to the K{...} (catch) block.
        // 2. Inside a subroutine (but not in an R block): decrements call
        //    depth and returns from the subroutine.
        // 3. In main: returns from main (exits the program).
        AstNode::Propagate => {
            indent(out, ctx.indent);
            if ctx.in_result_block {
                out.push_str("if (bfpp_err) break;\n");
            } else if ctx.in_subroutine {
                out.push_str("if (bfpp_err) { bfpp_call_depth--; return; }\n");
            } else {
                out.push_str("if (bfpp_err) return;\n");
            }
        }
        // R{...}K{...} — result/catch block (try/catch analogue).
        //
        // Implementation uses the do { ... } while(0) trick: the R body is
        // wrapped in do/while(0), so `break;` (from Propagate/?) jumps to
        // the statement after the loop — which is the K (catch) block.
        // This avoids goto while keeping structured control flow.
        //
        // Error register lifecycle:
        // 1. Save the outer bfpp_err (so R/K blocks can nest).
        // 2. Clear bfpp_err to BFPP_OK — the R body starts with a clean slate.
        // 3. Execute R body. If ? fires, `break` exits to K.
        // 4. If bfpp_err is set, execute K body (the catch path).
        // 5. If the R block succeeded (bfpp_err still OK), restore the
        //    saved outer error — don't mask a pre-existing error from the
        //    caller just because this R block succeeded.
        AstNode::ResultBlock(result_body, catch_body) => {
            indent(out, ctx.indent);
            out.push_str("{\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str("int saved_err = bfpp_err;\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_err = BFPP_OK;\n");
            // do/while(0) wrapper — enables `break`-based error propagation
            indent(out, ctx.indent);
            out.push_str("do {\n");
            ctx.indent += 1;
            let prev_in_result = ctx.in_result_block;
            ctx.in_result_block = true;
            emit_nodes(out, result_body, ctx);
            ctx.in_result_block = prev_in_result;
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("} while(0);\n");
            indent(out, ctx.indent);
            // K (catch) block — only executes if bfpp_err was set during R body
            out.push_str("if (bfpp_err) {\n");
            ctx.indent += 1;
            emit_nodes(out, catch_body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
            // Restore outer error if R block succeeded (don't clobber caller's error state)
            indent(out, ctx.indent);
            out.push_str("if (!bfpp_err) bfpp_err = saved_err;\n");
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // Tape address (T): pushes the actual C pointer to tape[ptr] onto the
        // BF++ stack. Enables passing tape addresses to syscalls or FFI functions
        // that expect memory pointers (e.g., read/write buffer addresses).
        AstNode::TapeAddr => {
            indent(out, ctx.indent);
            out.push_str("bfpp_push((uint64_t)(uintptr_t)&tape[ptr]);\n");
        }

        // Framebuffer flush — non-blocking (sets atomic flag, presenter thread picks it up)
        AstNode::FramebufferFlush => {
            indent(out, ctx.indent);
            out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_fb_request_flush();\n");
            indent(out, ctx.indent);
            out.push_str("#endif\n");
        }

        // ── Dual-tape operators ─────────────────────────────
        AstNode::ReadTape => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, rtape[rptr]);\n");
        }
        AstNode::WriteTape => {
            indent(out, ctx.indent);
            out.push_str("wtape[wptr] = (uint8_t)bfpp_get(ptr);\n");
        }
        AstNode::ReadPtrRight => {
            indent(out, ctx.indent);
            out.push_str("rptr = (rptr + 1) & TAPE_MASK;\n");
        }
        AstNode::ReadPtrLeft => {
            indent(out, ctx.indent);
            out.push_str("rptr = (rptr - 1 + TAPE_SIZE) & TAPE_MASK;\n");
        }
        AstNode::Transfer => {
            indent(out, ctx.indent);
            out.push_str("wtape[wptr] = rtape[rptr];\n");
        }
        AstNode::SwapTapes => {
            // Swap read/write tape pointers + pointer indices. Uses a static
            // scratch buffer to avoid stack-allocating TAPE_SIZE bytes.
            indent(out, ctx.indent);
            out.push_str("{ static _Thread_local uint8_t _swap_tmp[TAPE_SIZE]; memcpy(_swap_tmp, rtape, TAPE_SIZE); memcpy(rtape, wtape, TAPE_SIZE); memcpy(wtape, _swap_tmp, TAPE_SIZE); int _tp = rptr; rptr = wptr; wptr = _tp; }\n");
        }
        AstNode::SyncPtrs => {
            indent(out, ctx.indent);
            out.push_str("rptr = wptr;\n");
        }

        // Numeric literal: set current cell to an immediate value
        AstNode::SetValue(val) => {
            indent(out, ctx.indent);
            out.push_str(&format!("bfpp_set(ptr, {}ULL);\n", val));
        }

        // Multi-cell setup: set P+0, P+1, ... P+N from a list of values.
        // Pointer stays at P+0 after the operation.
        AstNode::SetMulti(values) => {
            indent(out, ctx.indent);
            out.push_str("{\n");
            ctx.indent += 1;
            for (i, val) in values.iter().enumerate() {
                indent(out, ctx.indent);
                out.push_str(&format!("bfpp_set(ptr + {}, {}ULL);\n", i, val));
            }
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // Conditional comparisons: snapshot current cell value, compare, branch.
        // Non-destructive — the cell value is preserved after the comparison.
        AstNode::IfEqual(val, body, else_body) => {
            indent(out, ctx.indent);
            out.push_str(&format!("if (bfpp_get(ptr) == {}ULL) {{\n", val));
            ctx.indent += 1;
            emit_nodes(out, body, ctx);
            ctx.indent -= 1;
            if let Some(else_nodes) = else_body {
                indent(out, ctx.indent);
                out.push_str("} else {\n");
                ctx.indent += 1;
                emit_nodes(out, else_nodes, ctx);
                ctx.indent -= 1;
            }
            indent(out, ctx.indent);
            out.push_str("}\n");
        }
        AstNode::IfNotEqual(val, body) => {
            indent(out, ctx.indent);
            out.push_str(&format!("if (bfpp_get(ptr) != {}ULL) {{\n", val));
            ctx.indent += 1;
            emit_nodes(out, body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }
        AstNode::IfLess(val, body) => {
            indent(out, ctx.indent);
            out.push_str(&format!("if (bfpp_get(ptr) < {}ULL) {{\n", val));
            ctx.indent += 1;
            emit_nodes(out, body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }
        AstNode::IfGreater(val, body) => {
            indent(out, ctx.indent);
            out.push_str(&format!("if (bfpp_get(ptr) > {}ULL) {{\n", val));
            ctx.indent += 1;
            emit_nodes(out, body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // ?{true}:{false} — destructive truthiness test.
        // Reads cell value, zeroes it, branches on the saved value.
        AstNode::IfElse(true_body, false_body) => {
            indent(out, ctx.indent);
            out.push_str("{\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str("uint64_t _cond = bfpp_get(ptr);\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, 0);\n");
            indent(out, ctx.indent);
            out.push_str("if (_cond) {\n");
            ctx.indent += 1;
            emit_nodes(out, true_body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("} else {\n");
            ctx.indent += 1;
            emit_nodes(out, false_body, ctx);
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // Direct cell width: set cell width to a specific value (1, 2, 4, or 8)
        // without cycling. Handles sub-cell release and continuation marking
        // the same way bfpp_cycle_width does.
        AstNode::SetCellWidth(w) => {
            indent(out, ctx.indent);
            out.push_str("{ int old_w = cell_width[ptr]; int i;\n");
            indent(out, ctx.indent);
            out.push_str("for (i = 1; i < old_w; i++) cell_width[ptr + i] = 1;\n");
            indent(out, ctx.indent);
            out.push_str(&format!("cell_width[ptr] = {};\n", w));
            if *w > 1 {
                indent(out, ctx.indent);
                out.push_str(&format!("for (i = 1; i < {}; i++) {{\n", w));
                indent(out, ctx.indent + 1);
                out.push_str("if (cell_width[ptr + i] != 1) { bfpp_err = BFPP_ERR_INVALID_ARG; cell_width[ptr] = 1; break; }\n");
                indent(out, ctx.indent + 1);
                out.push_str("cell_width[ptr + i] = 0;\n");
                indent(out, ctx.indent);
                out.push_str("}\n");
            }
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // FFI call (\ffi "lib" "func"): dynamic library call via dlopen/dlsym.
        // Opens the library, resolves the symbol, reads 6 args from
        // tape[ptr+8..ptr+48] (same layout as syscall), calls the function,
        // writes the result to tape[ptr]. Sets ERR_NOLIB or ERR_NOSYM on
        // failure. Always closes the library handle after the call.
        AstNode::FfiCall(lib, func) => {
            indent(out, ctx.indent);
            out.push_str("{\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str(&format!("void *ffi_handle = dlopen(\"{}\", RTLD_LAZY);\n", lib));
            indent(out, ctx.indent);
            out.push_str("if (!ffi_handle) { bfpp_err = BFPP_ERR_NOLIB; }\n");
            indent(out, ctx.indent);
            out.push_str("else {\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str(&format!(
                "long (*ffi_fn)(long,long,long,long,long,long) = (long(*)(long,long,long,long,long,long))dlsym(ffi_handle, \"{}\");\n",
                func
            ));
            indent(out, ctx.indent);
            out.push_str("if (!ffi_fn) { bfpp_err = BFPP_ERR_NOSYM; dlclose(ffi_handle); }\n");
            indent(out, ctx.indent);
            out.push_str("else {\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str("long a1 = (long)bfpp_get(ptr + 8);\n");
            indent(out, ctx.indent);
            out.push_str("long a2 = (long)bfpp_get(ptr + 16);\n");
            indent(out, ctx.indent);
            out.push_str("long a3 = (long)bfpp_get(ptr + 24);\n");
            indent(out, ctx.indent);
            out.push_str("long a4 = (long)bfpp_get(ptr + 32);\n");
            indent(out, ctx.indent);
            out.push_str("long a5 = (long)bfpp_get(ptr + 40);\n");
            indent(out, ctx.indent);
            out.push_str("long a6 = (long)bfpp_get(ptr + 48);\n");
            indent(out, ctx.indent);
            out.push_str("long ffi_result = ffi_fn(a1, a2, a3, a4, a5, a6);\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)ffi_result);\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_err = BFPP_OK;\n");
            indent(out, ctx.indent);
            out.push_str("dlclose(ffi_handle);\n");
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }

        // Optimizer-generated nodes: these don't appear in source BF++ but are
        // produced by the optimizer when it recognizes common patterns.
        // Clear: [-] → set cell to 0 directly.
        // ScanRight/ScanLeft: [>]/[<] → linear scan for a zero cell.
        // MultiplyMove: [->>+++<<] patterns → multiply current cell value by
        //   constant factors and add to cells at known offsets, then clear.
        AstNode::Clear => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, 0);\n");
        }
        AstNode::ScanRight => {
            indent(out, ctx.indent);
            out.push_str("while (bfpp_get(ptr)) ptr = (ptr + 1) & TAPE_MASK;\n");
        }
        AstNode::ScanLeft => {
            indent(out, ctx.indent);
            out.push_str("while (bfpp_get(ptr)) ptr = (ptr - 1 + TAPE_SIZE) & TAPE_MASK;\n");
        }
        AstNode::MultiplyMove(pairs) => {
            indent(out, ctx.indent);
            out.push_str("{\n");
            ctx.indent += 1;
            indent(out, ctx.indent);
            out.push_str("uint64_t val = bfpp_get(ptr);\n");
            for (offset, factor) in pairs {
                indent(out, ctx.indent);
                if *offset >= 0 {
                    out.push_str(&format!(
                        "bfpp_set(ptr + {}, bfpp_get(ptr + {}) + val * {});\n",
                        offset, offset, factor
                    ));
                } else {
                    out.push_str(&format!(
                        "bfpp_set(ptr - {}, bfpp_get(ptr - {}) + val * {});\n",
                        -offset, -offset, factor
                    ));
                }
            }
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, 0);\n");
            ctx.indent -= 1;
            indent(out, ctx.indent);
            out.push_str("}\n");
        }
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("    ");
    }
}

/// Mangle a subroutine name for C identifier compatibility.
///
/// BF++ subroutine names can contain operator characters (>, <, +, *, etc.)
/// that are illegal in C identifiers. Each special char is replaced with a
/// readable ASCII word (e.g., '>' → "gt", '*' → "star"). Unknown characters
/// get a unicode-hex escape ("u{XX}"). Combined with the "bfpp_sub_" prefix
/// added at the call site, this guarantees unique, valid C function names
/// that don't collide with libc or user symbols.
fn mangle_name(name: &str) -> String {
    let mut mangled = String::new();
    for c in name.chars() {
        match c {
            '>' => mangled.push_str("gt"),
            '<' => mangled.push_str("lt"),
            '+' => mangled.push_str("plus"),
            '-' => mangled.push_str("minus"),
            '.' => mangled.push_str("dot"),
            ',' => mangled.push_str("comma"),
            '[' => mangled.push_str("lbr"),
            ']' => mangled.push_str("rbr"),
            '@' => mangled.push_str("at"),
            '*' => mangled.push_str("star"),
            '%' => mangled.push_str("pct"),
            '$' => mangled.push_str("dollar"),
            '~' => mangled.push_str("tilde"),
            '\\' => mangled.push_str("bslash"),
            '|' => mangled.push_str("pipe"),
            '&' => mangled.push_str("amp"),
            '^' => mangled.push_str("caret"),
            '_' => mangled.push('_'),
            c if c.is_alphanumeric() => mangled.push(c),
            _ => mangled.push_str(&format!("u{:02x}", c as u32)),
        }
    }
    mangled
}

// ── Compiler Intrinsics ──────────────────────────────────────────────
//
// Intrinsics are subroutine calls with names starting with "__" that the
// compiler replaces with inline C code instead of a BF++ subroutine call.
// This bridges the gap between what BF++ operators can express and what
// C can do — terminal control, time, environment, process management.
//
// Each intrinsic reads its arguments from tape[ptr..] and writes results
// back to tape[ptr..]. The mapping is documented per-intrinsic below.

// Emit inline C for a compiler intrinsic. Returns without writing if
// the intrinsic name is not recognized (shouldn't happen if analyzer
// allows all __ names through).
fn emit_intrinsic(out: &mut String, name: &str, ctx: &mut GenCtx) {
    match name {
        // ── Terminal control ─────────────────────────────────
        // __term_raw: switch stdin to raw mode (no echo, no line buffering,
        // no signal generation). Clones saved_termios so restore can revert.
        // VMIN=1/VTIME=0 means read() blocks until at least 1 byte arrives.
        "__term_raw" => {
            indent(out, ctx.indent);
            out.push_str("{ struct termios raw = bfpp_saved_termios; raw.c_lflag &= ~(ECHO | ICANON | ISIG); raw.c_cc[VMIN] = 1; raw.c_cc[VTIME] = 0; if (tcsetattr(0, TCSAFLUSH, &raw) < 0) bfpp_err = BFPP_ERR_IO; else bfpp_term_raw = 1; }\n");
        }
        // __term_restore: revert to the original terminal mode captured at
        // program start. No-op if not currently in raw mode.
        "__term_restore" => {
            indent(out, ctx.indent);
            out.push_str("if (bfpp_term_raw) { tcsetattr(0, TCSAFLUSH, &bfpp_saved_termios); bfpp_term_raw = 0; }\n");
        }
        // __term_size: query terminal dimensions via TIOCGWINSZ ioctl.
        // Output: tape[ptr]=cols, tape[ptr+1]=rows
        "__term_size" => {
            indent(out, ctx.indent);
            out.push_str("{ struct winsize ws; if (ioctl(0, TIOCGWINSZ, &ws) == 0) { bfpp_set(ptr, ws.ws_col); bfpp_set(ptr+1, ws.ws_row); } else { bfpp_err = BFPP_ERR_IO; } }\n");
        }
        // __term_alt_on: switch to the alternate screen buffer (xterm ?1049h).
        // Used by TUI apps so they don't clobber the user's scrollback.
        "__term_alt_on" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1049h\", stdout); fflush(stdout);\n");
        }
        // __term_alt_off: return to the main screen buffer.
        "__term_alt_off" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1049l\", stdout); fflush(stdout);\n");
        }
        // __term_mouse_on: enable X11 basic mouse tracking (?1000h) and SGR
        // extended mouse format (?1006h) for coordinates > 223.
        "__term_mouse_on" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1000h\\033[?1006h\", stdout); fflush(stdout);\n");
        }
        // __term_mouse_off: disable both mouse tracking modes.
        "__term_mouse_off" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1000l\\033[?1006l\", stdout); fflush(stdout);\n");
        }

        // ── Time ─────────────────────────────────────────────
        "__sleep" => {
            // Input: tape[ptr]=milliseconds
            indent(out, ctx.indent);
            out.push_str("usleep((useconds_t)(bfpp_get(ptr) * 1000));\n");
        }
        "__time_ms" => {
            // Output: tape[ptr]=timestamp in milliseconds (monotonic)
            indent(out, ctx.indent);
            out.push_str("{ struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); bfpp_set(ptr, (uint64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000); }\n");
        }

        // ── Environment ──────────────────────────────────────
        "__getenv" => {
            // Input: null-terminated var name at tape[ptr]
            // Output: value written at tape[ptr] (overwrites name), or bfpp_err if not found
            indent(out, ctx.indent);
            out.push_str("{ char *v = getenv((char*)&tape[ptr]); if (v) { size_t l = strlen(v); if (l > TAPE_SIZE - ptr - 1) l = TAPE_SIZE - ptr - 1; memcpy(&tape[ptr], v, l); tape[ptr + l] = 0; } else { tape[ptr] = 0; bfpp_err = BFPP_ERR_NOT_FOUND; } }\n");
        }

        // ── Process ──────────────────────────────────────────
        "__exit" => {
            // Input: tape[ptr]=exit_code
            indent(out, ctx.indent);
            out.push_str("exit((int)bfpp_get(ptr));\n");
        }
        "__getpid" => {
            // Output: tape[ptr]=pid
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)getpid());\n");
        }

        // ── Non-blocking I/O ─────────────────────────────────
        "__poll_stdin" => {
            // Input: tape[ptr]=timeout_ms
            // Output: tape[ptr]=1 if data ready, 0 if timeout
            indent(out, ctx.indent);
            out.push_str("{ struct pollfd pfd = {0, POLLIN, 0}; int r = poll(&pfd, 1, (int)bfpp_get(ptr)); bfpp_set(ptr, r > 0 ? 1 : 0); }\n");
        }

        // ── Memory operations ─────────────────────────────────────
        "__memcpy" => {
            // Input: tape[ptr]=dst, tape[ptr+1]=src, tape[ptr+2]=count
            // Uses memmove (handles overlapping regions)
            indent(out, ctx.indent);
            out.push_str("{ int dst = (int)bfpp_get(ptr) & TAPE_MASK; int src = (int)bfpp_get(ptr+1) & TAPE_MASK; int cnt = (int)bfpp_get(ptr+2); if (cnt > 0 && dst + cnt <= TAPE_SIZE && src + cnt <= TAPE_SIZE) memmove(&tape[dst], &tape[src], cnt); }\n");
        }
        "__memset" => {
            // Input: tape[ptr]=addr, tape[ptr+1]=value, tape[ptr+2]=count
            indent(out, ctx.indent);
            out.push_str("{ int addr = (int)bfpp_get(ptr) & TAPE_MASK; uint8_t val = (uint8_t)bfpp_get(ptr+1); int cnt = (int)bfpp_get(ptr+2); if (cnt > 0 && addr + cnt <= TAPE_SIZE) memset(&tape[addr], val, cnt); }\n");
        }
        "__memchr" => {
            // Input: tape[ptr]=start_addr, tape[ptr+1]=byte, tape[ptr+2]=max_count
            // Output: tape[ptr]=found_addr (or 0 if not found)
            indent(out, ctx.indent);
            out.push_str("{ int start = (int)bfpp_get(ptr) & TAPE_MASK; uint8_t needle = (uint8_t)bfpp_get(ptr+1); int maxn = (int)bfpp_get(ptr+2); if (maxn > TAPE_SIZE - start) maxn = TAPE_SIZE - start; uint8_t *found = memchr(&tape[start], needle, maxn); bfpp_set(ptr, found ? (uint64_t)(found - tape) : 0); }\n");
        }

        // ── Integer arithmetic (raw, not Q16.16) ────────────────
        // These operate on raw integers in tape cells, NOT fixed-point.
        // Essential for offset calculations, index math, and self-hosting.
        "__mul" => {
            // Input: tape[ptr]=a, tape[ptr+1]=b. Output: tape[ptr]=a*b
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_get(ptr) * bfpp_get(ptr+1));\n");
        }
        "__div" => {
            // Input: tape[ptr]=a, tape[ptr+1]=b. Output: tape[ptr]=a/b, tape[ptr+1]=a%b
            indent(out, ctx.indent);
            out.push_str("{ uint64_t _a = bfpp_get(ptr), _b = bfpp_get(ptr+1); if (_b) { bfpp_set(ptr, _a / _b); bfpp_set(ptr+1, _a % _b); } else { bfpp_err = BFPP_ERR_INVALID_ARG; } }\n");
        }
        "__mod" => {
            // Input: tape[ptr]=a, tape[ptr+1]=b. Output: tape[ptr]=a%b
            indent(out, ctx.indent);
            out.push_str("{ uint64_t _b = bfpp_get(ptr+1); if (_b) { bfpp_set(ptr, bfpp_get(ptr) % _b); } else { bfpp_err = BFPP_ERR_INVALID_ARG; } }\n");
        }

        // ── String operations ────────────────────────────────────
        "__strcmp" => {
            // Input: tape[ptr]=addr_a, tape[ptr+1]=addr_b (both null-terminated on tape)
            // Output: tape[ptr] = 0 if equal, <0 if a<b, >0 if a>b
            indent(out, ctx.indent);
            out.push_str("{ int _a = (int)bfpp_get(ptr) & TAPE_MASK; int _b = (int)bfpp_get(ptr+1) & TAPE_MASK; int _r = 0; while (tape[_a] == tape[_b] && tape[_a] != 0) { _a++; _b++; } _r = (int)tape[_a] - (int)tape[_b]; bfpp_set(ptr, (uint64_t)(int64_t)_r); }\n");
        }
        "__strlen" => {
            // Input: tape[ptr]=addr (null-terminated on tape)
            // Output: tape[ptr]=length
            indent(out, ctx.indent);
            out.push_str("{ int _a = (int)bfpp_get(ptr) & TAPE_MASK; int _l = 0; while (tape[_a + _l] != 0) _l++; bfpp_set(ptr, (uint64_t)_l); }\n");
        }
        "__strcpy" => {
            // Input: tape[ptr]=dst_addr, tape[ptr+1]=src_addr
            // Output: copies null-terminated string from src to dst
            indent(out, ctx.indent);
            out.push_str("{ int _d = (int)bfpp_get(ptr) & TAPE_MASK; int _s = (int)bfpp_get(ptr+1) & TAPE_MASK; while (tape[_s]) { tape[_d++] = tape[_s++]; } tape[_d] = 0; }\n");
        }

        // ── Array operations (O(1) element ops with shift) ───────
        "__array_insert" => {
            // Input: tape[ptr]=array_addr, tape[ptr+1]=index, tape[ptr+2]=element_size,
            //        tape[ptr+3]=count (current element count), tape[ptr+4]=value_addr (source)
            // Shifts elements at [index..count] right by element_size, copies value from value_addr.
            indent(out, ctx.indent);
            out.push_str("{ int _base = (int)bfpp_get(ptr) & TAPE_MASK; int _idx = (int)bfpp_get(ptr+1); int _esz = (int)bfpp_get(ptr+2); int _cnt = (int)bfpp_get(ptr+3); int _vsrc = (int)bfpp_get(ptr+4) & TAPE_MASK; int _off = _base + _idx * _esz; int _tail = (_cnt - _idx) * _esz; if (_tail > 0) memmove(&tape[_off + _esz], &tape[_off], _tail); memcpy(&tape[_off], &tape[_vsrc], _esz); }\n");
        }
        "__array_remove" => {
            // Input: tape[ptr]=array_addr, tape[ptr+1]=index, tape[ptr+2]=element_size,
            //        tape[ptr+3]=count
            // Shifts elements at [index+1..count] left by element_size, removing element at index.
            indent(out, ctx.indent);
            out.push_str("{ int _base = (int)bfpp_get(ptr) & TAPE_MASK; int _idx = (int)bfpp_get(ptr+1); int _esz = (int)bfpp_get(ptr+2); int _cnt = (int)bfpp_get(ptr+3); int _off = _base + _idx * _esz; int _tail = (_cnt - _idx - 1) * _esz; if (_tail > 0) memmove(&tape[_off], &tape[_off + _esz], _tail); }\n");
        }

        // ── Indirect subroutine call ─────────────────────────────
        "__call" => {
            // Input: tape[ptr]=subroutine_index (0-based index into bfpp_sub_table)
            // Calls bfpp_sub_table[index](). Enables computed dispatch / jump tables.
            indent(out, ctx.indent);
            out.push_str("{ int _idx = (int)bfpp_get(ptr); if (_idx >= 0 && _idx < (int)(sizeof(bfpp_sub_table)/sizeof(bfpp_sub_table[0]))) bfpp_sub_table[_idx](); else bfpp_err = BFPP_ERR_INVALID_ARG; }\n");
        }

        // ── Hash map on tape ─────────────────────────────────────
        // Simple open-addressing hash map stored on tape.
        // Layout at map_addr: [capacity:4][count:4][entries...]
        // Each entry: [hash:4][key_len:1][key_data:max_key][value:cell_width]
        "__hashmap_init" => {
            // Input: tape[ptr]=map_addr, tape[ptr+1]=capacity (num buckets)
            // Zeros the map region.
            indent(out, ctx.indent);
            out.push_str("{ int _addr = (int)bfpp_get(ptr) & TAPE_MASK; int _cap = (int)bfpp_get(ptr+1); bfpp_set(_addr, (uint64_t)_cap); bfpp_set(_addr + 4, 0); memset(&tape[_addr + 8], 0, _cap * 40); }\n");
        }
        "__hashmap_get" => {
            // Input: tape[ptr]=map_addr, tape[ptr+1]=key_addr (null-terminated)
            // Output: tape[ptr]=value (0 if not found), tape[ptr+1]=1 if found, 0 if not
            indent(out, ctx.indent);
            out.push_str("{ int _maddr = (int)bfpp_get(ptr) & TAPE_MASK; int _kaddr = (int)bfpp_get(ptr+1) & TAPE_MASK; int _cap = (int)bfpp_get(_maddr); uint32_t _h = 5381; for (int _i = _kaddr; tape[_i]; _i++) _h = _h * 33 + tape[_i]; int _slot = (_h % _cap) * 40 + _maddr + 8; int _found = 0; for (int _p = 0; _p < _cap; _p++) { int _s = ((_h + _p) % _cap) * 40 + _maddr + 8; if (tape[_s] == 0 && tape[_s+1] == 0 && tape[_s+2] == 0 && tape[_s+3] == 0) break; int _ka = _s + 5; int _ki = _kaddr; int _eq = 1; while (tape[_ka] && tape[_ki]) { if (tape[_ka] != tape[_ki]) { _eq = 0; break; } _ka++; _ki++; } if (_eq && tape[_ka] == tape[_ki]) { bfpp_set(ptr, bfpp_get(_s + 36)); bfpp_set(ptr+1, 1); _found = 1; break; } } if (!_found) { bfpp_set(ptr, 0); bfpp_set(ptr+1, 0); } }\n");
        }
        "__hashmap_set" => {
            // Input: tape[ptr]=map_addr, tape[ptr+1]=key_addr, tape[ptr+2]=value
            // Inserts or updates key→value. Uses djb2 hash + linear probing.
            indent(out, ctx.indent);
            out.push_str("{ int _maddr = (int)bfpp_get(ptr) & TAPE_MASK; int _kaddr = (int)bfpp_get(ptr+1) & TAPE_MASK; uint64_t _val = bfpp_get(ptr+2); int _cap = (int)bfpp_get(_maddr); uint32_t _h = 5381; for (int _i = _kaddr; tape[_i]; _i++) _h = _h * 33 + tape[_i]; for (int _p = 0; _p < _cap; _p++) { int _s = ((_h + _p) % _cap) * 40 + _maddr + 8; uint32_t _sh = (uint32_t)tape[_s] | ((uint32_t)tape[_s+1]<<8) | ((uint32_t)tape[_s+2]<<16) | ((uint32_t)tape[_s+3]<<24); if (_sh == 0) { tape[_s] = _h & 0xFF; tape[_s+1] = (_h>>8) & 0xFF; tape[_s+2] = (_h>>16) & 0xFF; tape[_s+3] = (_h>>24) & 0xFF; int _ki = _kaddr; int _ko = _s + 5; while (tape[_ki]) tape[_ko++] = tape[_ki++]; tape[_ko] = 0; bfpp_set(_s + 36, _val); bfpp_set(_maddr + 4, bfpp_get(_maddr + 4) + 1); break; } int _ka = _s + 5; int _ki = _kaddr; int _eq = 1; while (tape[_ka] && tape[_ki]) { if (tape[_ka] != tape[_ki]) { _eq = 0; break; } _ka++; _ki++; } if (_eq && tape[_ka] == tape[_ki]) { bfpp_set(_s + 36, _val); break; } } }\n");
        }

        // ── TUI Runtime ──────────────────────────────────────────
        // These intrinsics delegate to the bfpp_rt.c double-buffered terminal
        // UI runtime. It provides a cell grid with per-cell fg/bg colors,
        // diff-based rendering (only changed cells are redrawn), box drawing,
        // and non-blocking keyboard input. All functions are defined in
        // runtime/bfpp_rt.c and declared in runtime/bfpp_rt.h.

        // __tui_init: initialize the TUI runtime (allocates cell buffers,
        // enters raw mode, switches to alt screen).
        "__tui_init" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_init();\n");
        }
        // __tui_cleanup: tear down the TUI runtime (frees buffers, restores
        // terminal mode, returns to main screen).
        "__tui_cleanup" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_cleanup();\n");
        }
        // __tui_size: query the TUI grid dimensions.
        // Output: tape[ptr]=cols, tape[ptr+1]=rows
        "__tui_size" => {
            indent(out, ctx.indent);
            out.push_str("{ int c,r; bfpp_tui_get_size(&c,&r); bfpp_set(ptr,c); bfpp_set(ptr+1,r); }\n");
        }
        // __tui_begin: start a new frame. Marks the front buffer as "drawing"
        // so changes accumulate without flushing to the terminal.
        "__tui_begin" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_begin_frame();\n");
        }
        // __tui_end: finish the frame. Diffs the front buffer against the back
        // buffer and emits only the changed cells as ANSI escape sequences.
        "__tui_end" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_end_frame();\n");
        }
        // __tui_put: write a single character with color to the cell grid.
        // Input: tape[ptr]=row, [ptr+1]=col, [ptr+2]=char, [ptr+3]=fg, [ptr+4]=bg
        // fg/bg are cast through int8_t so values 0-7 are normal ANSI colors
        // and -1 means "default/no change".
        "__tui_put" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_put((int)bfpp_get(ptr), (int)bfpp_get(ptr+1), (uint8_t)bfpp_get(ptr+2), (int)(int8_t)bfpp_get(ptr+3), (int)(int8_t)bfpp_get(ptr+4));\n");
        }
        // __tui_puts: write a null-terminated string at a position with color.
        // Input: tape[ptr]=row, [ptr+1]=col, null-terminated string at ptr+2,
        // fg at first byte after the null terminator, bg at the byte after that.
        // This layout lets the caller pack position+text+color contiguously.
        "__tui_puts" => {
            indent(out, ctx.indent);
            out.push_str("{ int r=(int)bfpp_get(ptr), c=(int)bfpp_get(ptr+1); char *s=(char*)&tape[(ptr+2)&TAPE_MASK]; int sl=strlen(s); int fg=(int)(int8_t)tape[(ptr+2+sl+1)&TAPE_MASK]; int bg=(int)(int8_t)tape[(ptr+2+sl+2)&TAPE_MASK]; bfpp_tui_puts(r,c,s,fg,bg); }\n");
        }
        // __tui_fill: fill a rectangular region with a character and color.
        // Input: tape[ptr]=row,[ptr+1]=col,[ptr+2]=width,[ptr+3]=height,
        //        [ptr+4]=char,[ptr+5]=fg,[ptr+6]=bg
        "__tui_fill" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_fill((int)bfpp_get(ptr), (int)bfpp_get(ptr+1), (int)bfpp_get(ptr+2), (int)bfpp_get(ptr+3), (uint8_t)bfpp_get(ptr+4), (int)(int8_t)bfpp_get(ptr+5), (int)(int8_t)bfpp_get(ptr+6));\n");
        }
        // __tui_box: draw a box with border characters (single/double line).
        // Input: tape[ptr]=row,[ptr+1]=col,[ptr+2]=width,[ptr+3]=height,
        //        [ptr+4]=style (0=single, 1=double)
        "__tui_box" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_tui_box((int)bfpp_get(ptr), (int)bfpp_get(ptr+1), (int)bfpp_get(ptr+2), (int)bfpp_get(ptr+3), (int)bfpp_get(ptr+4));\n");
        }
        // __tui_key: poll for a keypress with timeout.
        // Input: tape[ptr]=timeout in milliseconds (0 = non-blocking)
        // Output: tape[ptr]=keycode, or -1 (0xFFFFFFFF...) if no key within timeout.
        // The double cast (int64_t then uint64_t) preserves the -1 sentinel
        // through the unsigned tape cell.
        "__tui_key" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)(int64_t)bfpp_tui_poll_key((int)bfpp_get(ptr)));\n");
        }

        // ── Framebuffer pipeline intrinsics ──────────────────
        "__fb_sync" => {
            indent(out, ctx.indent);
            out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_fb_sync();\n");
            indent(out, ctx.indent);
            out.push_str("#endif\n");
        }
        "__fb_pixel_nt" => {
            // Input: tape[ptr]=x, [ptr+1]=y, [ptr+2]=r, [ptr+3]=g, [ptr+4]=b
            indent(out, ctx.indent);
            out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_fb_write_pixel_nt(tape, BFPP_FB_OFFSET, (int)bfpp_get(ptr), (int)bfpp_get(ptr+1), BFPP_FB_WIDTH, (uint8_t)bfpp_get(ptr+2), (uint8_t)bfpp_get(ptr+3), (uint8_t)bfpp_get(ptr+4));\n");
            indent(out, ctx.indent);
            out.push_str("#endif\n");
        }

        // ── 3D Rendering intrinsics ────────────────────────
        // Lifecycle
        "__gl_init" => {
            indent(out, ctx.indent);
            out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_3d_init(BFPP_FB_WIDTH, BFPP_FB_HEIGHT, tape, BFPP_FB_OFFSET);\n");
            indent(out, ctx.indent);
            out.push_str("#endif\n");
        }
        "__gl_cleanup" => {
            indent(out, ctx.indent);
            out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_3d_cleanup();\n");
            indent(out, ctx.indent);
            out.push_str("#endif\n");
        }
        // Buffer management
        "__gl_create_buffer" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_create_buffer(tape, ptr);\n");
        }
        "__gl_buffer_data" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_buffer_data(tape, ptr);\n");
        }
        "__gl_delete_buffer" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_delete_buffer(tape, ptr);\n");
        }
        // VAO management
        "__gl_create_vao" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_create_vao(tape, ptr);\n");
        }
        "__gl_bind_vao" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_bind_vao(tape, ptr);\n");
        }
        "__gl_vertex_attrib" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_vertex_attrib(tape, ptr);\n");
        }
        "__gl_delete_vao" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_delete_vao(tape, ptr);\n");
        }
        // Shader management
        "__gl_create_shader" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_create_shader(tape, ptr);\n");
        }
        "__gl_shader_source" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_shader_source(tape, ptr);\n");
        }
        "__gl_compile_shader" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_compile_shader(tape, ptr);\n");
        }
        "__gl_create_program" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_create_program(tape, ptr);\n");
        }
        "__gl_attach_shader" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_attach_shader(tape, ptr);\n");
        }
        "__gl_link_program" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_link_program(tape, ptr);\n");
        }
        "__gl_use_program" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_use_program(tape, ptr);\n");
        }
        // Uniforms
        "__gl_uniform_loc" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_uniform_loc(tape, ptr);\n");
        }
        "__gl_uniform_1f" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_uniform_1f(tape, ptr);\n");
        }
        "__gl_uniform_3f" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_uniform_3f(tape, ptr);\n");
        }
        "__gl_uniform_4f" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_uniform_4f(tape, ptr);\n");
        }
        "__gl_uniform_mat4" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_uniform_mat4(tape, ptr);\n");
        }
        // Drawing
        "__gl_clear" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_clear(tape, ptr);\n");
        }
        "__gl_draw_arrays" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_draw_arrays(tape, ptr);\n");
        }
        "__gl_draw_elements" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_draw_elements(tape, ptr);\n");
        }
        "__gl_viewport" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_viewport(tape, ptr);\n");
        }
        "__gl_depth_test" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_depth_test(tape, ptr);\n");
        }
        "__gl_present" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_present(tape, ptr);\n");
        }
        // Shadow mapping
        "__gl_shadow_enable" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_shadow_enable(tape, ptr);\n");
        }
        "__gl_shadow_disable" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_shadow_disable(tape, ptr);\n");
        }
        "__gl_shadow_quality" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_shadow_quality(tape, ptr);\n");
        }
        // Textures
        "__gl_create_texture" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_create_texture(tape, ptr);\n");
        }
        "__gl_texture_data" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_texture_data(tape, ptr);\n");
        }
        "__gl_bind_texture" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_bind_texture(tape, ptr);\n");
        }
        "__gl_delete_texture" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_delete_texture(tape, ptr);\n");
        }
        // Image loading (BMP via SDL2)
        "__img_load" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_img_load(tape, ptr);\n");
        }
        // Fixed-point math (Tier 2)
        "__fp_mul" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_fp_mul(tape, ptr);\n");
        }
        "__fp_div" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_fp_div(tape, ptr);\n");
        }
        "__fp_sin" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_fp_sin(tape, ptr);\n");
        }
        "__fp_cos" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_fp_cos(tape, ptr);\n");
        }
        "__fp_sqrt" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_fp_sqrt(tape, ptr);\n");
        }
        // Matrix operations (Tier 2)
        "__mat4_identity" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mat4_identity(tape, ptr);\n");
        }
        "__mat4_multiply" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mat4_multiply(tape, ptr);\n");
        }
        "__mat4_rotate" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mat4_rotate(tape, ptr);\n");
        }
        "__mat4_translate" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mat4_translate(tape, ptr);\n");
        }
        "__mat4_perspective" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mat4_perspective(tape, ptr);\n");
        }
        // Mesh generators (Tier 3)
        "__mesh_cube" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mesh_cube(tape, ptr);\n");
        }
        "__mesh_sphere" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mesh_sphere(tape, ptr);\n");
        }
        "__mesh_torus" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mesh_torus(tape, ptr);\n");
        }
        "__mesh_plane" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mesh_plane(tape, ptr);\n");
        }
        "__mesh_cylinder" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mesh_cylinder(tape, ptr);\n");
        }

        // ── Multi-GPU + Scene Oracle intrinsics ────────────
        "__gl_multi_gpu" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_multi_gpu(tape, ptr);\n");
        }
        "__gl_gpu_count" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_gpu_count(tape, ptr);\n");
        }
        "__gl_frame_time" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_frame_time(tape, ptr);\n");
        }
        "__scene_publish" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_scene_publish_intrinsic(tape, ptr);\n");
        }
        "__scene_mode" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_scene_mode_intrinsic(tape, ptr);\n");
        }
        "__scene_extrap_ms" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_scene_extrap_ms_intrinsic(tape, ptr);\n");
        }

        // ── Input event intrinsics ────────────────────────────
        "__input_poll" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_input_poll(tape, ptr);\n");
        }
        "__input_mouse_pos" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_input_mouse_pos(tape, ptr);\n");
        }
        "__input_key_held" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_gl_input_key_held(tape, ptr);\n");
        }

        // ── Threading intrinsics ────────────────────────────
        "__spawn" => {
            // Input: tape[ptr]=subroutine_index, tape[ptr+8]=start_ptr
            // Output: tape[ptr]=thread_id (pthread_t)
            indent(out, ctx.indent);
            out.push_str("{ bfpp_thread_arg_t *_a = malloc(sizeof(bfpp_thread_arg_t)); ");
            out.push_str("_a->func = bfpp_sub_table[(int)bfpp_get(ptr)]; ");
            out.push_str("_a->start_ptr = (int)bfpp_get(ptr+8); ");
            out.push_str("_a->index = atomic_fetch_add(&bfpp_next_thread_index, 1); ");
            out.push_str("_a->tape_size = TAPE_SIZE; ");
            out.push_str("pthread_t _tid; pthread_create(&_tid, NULL, bfpp_thread_entry, _a); ");
            out.push_str("bfpp_set(ptr, (uint64_t)(uintptr_t)_tid); }\n");
        }
        "__join" => {
            // Input: tape[ptr]=thread_id
            indent(out, ctx.indent);
            out.push_str("pthread_join((pthread_t)(uintptr_t)bfpp_get(ptr), NULL);\n");
        }
        "__yield" => {
            indent(out, ctx.indent);
            out.push_str("sched_yield();\n");
        }
        "__thread_id" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)bfpp_thread_index);\n");
        }
        "__num_cores" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)sysconf(_SC_NPROCESSORS_ONLN));\n");
        }
        "__mutex_init" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mutex_init((int)bfpp_get(ptr));\n");
        }
        "__mutex_lock" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mutex_lock((int)bfpp_get(ptr));\n");
        }
        "__mutex_unlock" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_mutex_unlock((int)bfpp_get(ptr));\n");
        }
        "__atomic_load" => {
            // Input: tape[ptr]=addr. Output: tape[ptr]=value
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_atomic_load(tape, (int)bfpp_get(ptr), cell_width[(int)bfpp_get(ptr) & TAPE_MASK]));\n");
        }
        "__atomic_store" => {
            // Input: tape[ptr]=value, tape[ptr+1]=addr
            indent(out, ctx.indent);
            out.push_str("bfpp_atomic_store(tape, (int)bfpp_get(ptr+1), bfpp_get(ptr), cell_width[(int)bfpp_get(ptr+1) & TAPE_MASK]);\n");
        }
        "__atomic_add" => {
            // Input: tape[ptr]=value, tape[ptr+1]=addr. Output: tape[ptr]=old_value
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, bfpp_atomic_add(tape, (int)bfpp_get(ptr+1), bfpp_get(ptr), cell_width[(int)bfpp_get(ptr+1) & TAPE_MASK]));\n");
        }
        "__atomic_cas" => {
            // Input: tape[ptr]=expected, tape[ptr+1]=desired, tape[ptr+2]=addr
            // Output: tape[ptr]=success (1/0)
            indent(out, ctx.indent);
            out.push_str("bfpp_set(ptr, (uint64_t)bfpp_atomic_cas(tape, (int)bfpp_get(ptr+2), bfpp_get(ptr), bfpp_get(ptr+1), cell_width[(int)bfpp_get(ptr+2) & TAPE_MASK]));\n");
        }
        "__barrier_init" => {
            // Input: tape[ptr]=barrier_id, tape[ptr+1]=count
            indent(out, ctx.indent);
            out.push_str("bfpp_barrier_init((int)bfpp_get(ptr), (int)bfpp_get(ptr+1));\n");
        }
        "__barrier_wait" => {
            indent(out, ctx.indent);
            out.push_str("bfpp_barrier_wait((int)bfpp_get(ptr));\n");
        }

        // ── Unrecognized intrinsic ───────────────────────────
        _ => {
            indent(out, ctx.indent);
            out.push_str(&format!("/* WARNING: unknown intrinsic !#{} */\n", name));
        }
    }
}

// Tracks which categories of compiler intrinsics a program uses.
// Each flag corresponds to a set of C headers, state variables, and/or
// constructor functions that emit_header must include. This avoids
// unconditionally including everything (e.g., termios.h, poll.h) which
// would add unnecessary dependencies for programs that don't use those
// features.
#[derive(Default)]
struct IntrinsicUsage {
    terminal: bool,  // __term_* — requires <termios.h>, <sys/ioctl.h>, saved_termios state
    time: bool,      // __sleep, __time_ms — requires <time.h>
    env: bool,       // __getenv — no extra headers (stdlib.h covers getenv)
    process: bool,   // __exit, __getpid — no extra headers (stdlib.h/unistd.h cover these)
    poll: bool,      // __poll_stdin — requires <poll.h>
    tui: bool,       // __tui_* — requires bfpp_rt.h/bfpp_rt.c external runtime
    threading: bool, // __spawn, __join, __mutex_*, __atomic_*, __barrier_* — requires -pthread + bfpp_rt_parallel
    fb_sync: bool,   // __fb_sync, __fb_pixel_nt — framebuffer pipeline intrinsics
    gl3d: bool,      // __gl_*, __fp_*, __mat4_*, __mesh_* — requires bfpp_rt_3d + OpenGL/GLEW + math
    multigpu: bool,  // __gl_multi_gpu, __gl_gpu_count, __gl_frame_time, __scene_* — requires bfpp_rt_3d_multigpu + bfpp_rt_3d_oracle + EGL
    indirect_call: bool, // __call — needs bfpp_sub_table (same as threading)
}

// Pre-scan the entire AST for intrinsic calls and return a summary of
// which intrinsic categories are used. Called once before code generation
// begins so that emit_header can emit the correct includes.
fn detect_intrinsics(nodes: &[AstNode]) -> IntrinsicUsage {
    let mut usage = IntrinsicUsage::default();
    scan_intrinsics(nodes, &mut usage);
    usage
}

// Recursive AST walker that sets usage flags when it encounters SubCall
// nodes with __ prefixed names. Recurses into loops, subroutine bodies,
// result/catch blocks, and deref wrappers to catch intrinsics at any depth.
fn scan_intrinsics(nodes: &[AstNode], usage: &mut IntrinsicUsage) {
    for node in nodes {
        match node {
            AstNode::SubCall(name) if name.starts_with("__") => {
                match name.as_str() {
                    "__term_raw" | "__term_restore" | "__term_size" |
                    "__term_alt_on" | "__term_alt_off" |
                    "__term_mouse_on" | "__term_mouse_off" => usage.terminal = true,
                    "__sleep" | "__time_ms" => usage.time = true,
                    "__getenv" => usage.env = true,
                    "__exit" | "__getpid" => usage.process = true,
                    "__poll_stdin" => usage.poll = true,
                    "__tui_init" | "__tui_cleanup" | "__tui_size" |
                    "__tui_begin" | "__tui_end" | "__tui_put" |
                    "__tui_puts" | "__tui_fill" | "__tui_box" |
                    "__tui_key" => usage.tui = true,
                    "__spawn" | "__join" | "__yield" | "__thread_id" | "__num_cores" |
                    "__mutex_init" | "__mutex_lock" | "__mutex_unlock" |
                    "__atomic_load" | "__atomic_store" | "__atomic_add" | "__atomic_cas" |
                    "__barrier_init" | "__barrier_wait" => usage.threading = true,
                    "__fb_sync" | "__fb_pixel_nt" => usage.fb_sync = true,
                    "__gl_init" | "__gl_cleanup" | "__gl_create_buffer" |
                    "__gl_buffer_data" | "__gl_delete_buffer" |
                    "__gl_create_vao" | "__gl_bind_vao" | "__gl_vertex_attrib" |
                    "__gl_delete_vao" | "__gl_create_shader" | "__gl_shader_source" |
                    "__gl_compile_shader" | "__gl_create_program" | "__gl_attach_shader" |
                    "__gl_link_program" | "__gl_use_program" |
                    "__gl_uniform_loc" | "__gl_uniform_1f" | "__gl_uniform_3f" |
                    "__gl_uniform_4f" | "__gl_uniform_mat4" |
                    "__gl_clear" | "__gl_draw_arrays" | "__gl_draw_elements" |
                    "__gl_viewport" | "__gl_depth_test" | "__gl_present" |
                    "__gl_shadow_enable" | "__gl_shadow_disable" | "__gl_shadow_quality" |
                    "__gl_create_texture" | "__gl_texture_data" | "__gl_bind_texture" |
                    "__gl_delete_texture" | "__img_load" |
                    "__fp_mul" | "__fp_div" | "__fp_sin" | "__fp_cos" | "__fp_sqrt" |
                    "__mat4_identity" | "__mat4_multiply" | "__mat4_rotate" |
                    "__mat4_translate" | "__mat4_perspective" |
                    "__mesh_cube" | "__mesh_sphere" | "__mesh_torus" |
                    "__mesh_plane" | "__mesh_cylinder" |
                    "__input_poll" | "__input_mouse_pos" | "__input_key_held" => usage.gl3d = true,
                    "__gl_multi_gpu" | "__gl_gpu_count" | "__gl_frame_time" |
                    "__scene_publish" | "__scene_mode" | "__scene_extrap_ms" => {
                        usage.gl3d = true;
                        usage.multigpu = true;
                    }
                    // Core intrinsics — no external deps, always available
                    "__mul" | "__div" | "__mod" |
                    "__strcmp" | "__strlen" | "__strcpy" |
                    "__array_insert" | "__array_remove" |
                    "__hashmap_init" | "__hashmap_get" | "__hashmap_set" => {}
                    // Indirect call — needs sub_table
                    "__call" => { usage.indirect_call = true; }
                    _ => {}
                }
            }
            AstNode::Loop(body) | AstNode::SubDef(_, body) => scan_intrinsics(body, usage),
            AstNode::ResultBlock(r, k) => {
                scan_intrinsics(r, usage);
                scan_intrinsics(k, usage);
            }
            AstNode::Deref(inner) => scan_intrinsics(&[*inner.clone()], usage),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    /// Smoke test: the classic BF hello-world program produces valid C
    /// containing main(), a while loop, and putchar output.
    #[test]
    fn test_hello_world_generates() {
        let tokens = lex("++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("int main(void)"));
        assert!(result.c_source.contains("while (bfpp_get(ptr))"));
        assert!(result.c_source.contains("putchar"));
    }

    /// Subroutines emit a forward declaration and a C function definition,
    /// and call sites emit bfpp_sub_*() invocations.
    #[test]
    fn test_subroutine_codegen() {
        let tokens = lex("!#pr{.^} !#pr").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("void bfpp_sub_pr(void)"));
        assert!(result.c_source.contains("bfpp_sub_pr();"));
    }

    /// R{...}K{...} blocks emit saved_err / do-while(0) / if(bfpp_err) structure.
    #[test]
    fn test_error_handling_codegen() {
        let tokens = lex("R{+}K{-}").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("saved_err"));
        assert!(result.c_source.contains("if (bfpp_err)"));
    }

    /// Name mangling replaces BF++ operator chars with readable ASCII words
    /// so subroutine names become valid C identifiers.
    #[test]
    fn test_name_mangling() {
        assert_eq!(mangle_name(".>"), "dotgt");
        assert_eq!(mangle_name("pr"), "pr");
        assert_eq!(mangle_name("m*"), "mstar");
        assert_eq!(mangle_name("tcp"), "tcp");
    }

    /// T (TapeAddr) emits a bfpp_push of the raw C pointer to tape[ptr].
    #[test]
    fn test_tape_addr_codegen() {
        let tokens = lex("T").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_push((uint64_t)(uintptr_t)&tape[ptr])"));
    }

    /// F (FramebufferFlush) emits a guarded bfpp_fb_flush() call.
    #[test]
    fn test_framebuffer_flush_codegen() {
        let tokens = lex("F").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_fb_request_flush"));
    }

    /// FFI calls emit dlopen/dlsym boilerplate and set the uses_ffi metadata flag.
    #[test]
    fn test_ffi_codegen() {
        let tokens = lex(r#"\ffi "libm.so.6" "ceil""#).unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("dlopen"));
        assert!(result.c_source.contains("dlsym"));
        assert!(result.c_source.contains("libm.so.6"));
        assert!(result.c_source.contains("ceil"));
        assert!(result.uses_ffi);
    }

    /// #N (SetValue) emits bfpp_set with a ULL-suffixed literal.
    #[test]
    fn test_set_value_codegen() {
        let tokens = lex("#42 .").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_set(ptr, 42ULL)"));
        assert!(result.c_source.contains("putchar"));
    }

    /// Hex literals (#0xFF) are resolved at lex/parse time; codegen sees
    /// the decimal value and emits it as a ULL constant.
    #[test]
    fn test_set_value_hex_codegen() {
        let tokens = lex("#0xFF").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_set(ptr, 255ULL)"));
    }

    /// %N (SetCellWidth) emits inline C that releases old sub-cells and
    /// marks new continuation bytes, matching the bfpp_cycle_width logic.
    #[test]
    fn test_set_cell_width_codegen() {
        let tokens = lex("%8 #100").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("cell_width[ptr] = 8"));
        assert!(result.c_source.contains("bfpp_set(ptr, 100ULL)"));
    }

    /// __sleep intrinsic emits usleep() and triggers the time.h include.
    #[test]
    fn test_intrinsic_sleep_codegen() {
        let tokens = lex("#100 !#__sleep").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("usleep"));
        assert!(result.c_source.contains("#include <time.h>"));
    }

    /// __exit intrinsic emits exit() with the cell value as the exit code.
    #[test]
    fn test_intrinsic_exit_codegen() {
        let tokens = lex("#0 !#__exit").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("exit((int)bfpp_get(ptr))"));
    }

    /// TUI intrinsics emit bfpp_tui_*() calls, include bfpp_rt.h, and set
    /// the uses_tui_runtime flag so the compiler driver links the runtime.
    #[test]
    fn test_intrinsic_tui_codegen() {
        let tokens = lex("!#__tui_init !#__tui_cleanup").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_tui_init()"));
        assert!(result.c_source.contains("bfpp_tui_cleanup()"));
        assert!(result.c_source.contains("#include \"bfpp_rt.h\""));
        assert!(result.uses_tui_runtime);
    }

    /// Terminal intrinsics emit tcsetattr/ioctl calls, include termios.h,
    /// and emit the saved_termios state variable and constructor.
    #[test]
    fn test_intrinsic_term_codegen() {
        let tokens = lex("!#__term_raw !#__term_size !#__term_restore").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("tcsetattr"));
        assert!(result.c_source.contains("ioctl"));
        assert!(result.c_source.contains("#include <termios.h>"));
        assert!(result.c_source.contains("struct termios bfpp_saved_termios"));
    }

    #[test]
    fn test_multi_cell_codegen() {
        let tokens = lex("#{65, 66, 67}").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_set(ptr + 0"));
        assert!(result.c_source.contains("65ULL"));
        assert!(result.c_source.contains("bfpp_set(ptr + 2"));
        assert!(result.c_source.contains("67ULL"));
    }

    #[test]
    fn test_if_equal_codegen() {
        let tokens = lex("?= #17 [ #89 . ]").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("if (bfpp_get(ptr) == 17ULL)"));
    }

    #[test]
    fn test_if_else_codegen() {
        let tokens = lex("?= #65 [ #89 . ] : [ #78 . ]").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("if (bfpp_get(ptr) == 65ULL)"));
        assert!(result.c_source.contains("} else {"));
    }

    #[test]
    fn test_if_less_codegen() {
        let tokens = lex("?< #32 [ #89 . ]").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("if (bfpp_get(ptr) < 32ULL)"));
    }

    #[test]
    fn test_memcpy_intrinsic_codegen() {
        let tokens = lex("!#__memcpy").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("memmove"));
    }

    #[test]
    fn test_memset_intrinsic_codegen() {
        let tokens = lex("!#__memset").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("memset(&tape"));
    }
}
