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
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            tape_size: 65536,
            stack_size: 4096,
            call_depth: 256,
            framebuffer: None,
            eof_value: 0,
        }
    }
}

/// Result of code generation: C source and metadata flags
pub struct CodegenResult {
    pub c_source: String,
    pub uses_ffi: bool,
}

pub fn generate(program: &Program, opts: &CodegenOptions) -> CodegenResult {
    // Pre-scan for FFI usage so we know whether to #include <dlfcn.h>
    let uses_ffi = program_uses_ffi(&program.nodes);
    let c_source = generate_c(program, opts, uses_ffi);
    CodegenResult { c_source, uses_ffi }
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
fn generate_c(program: &Program, opts: &CodegenOptions, uses_ffi: bool) -> String {
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

    // Detect compiler intrinsic usage to know which C headers/state to emit.
    let intrinsics = detect_intrinsics(&program.nodes);

    // Emit the C runtime header: includes, #defines, static globals,
    // helper functions (bfpp_get/set/push/pop/cycle_width, errno mapping,
    // syscall wrapper, constructor, and optional SDL framebuffer).
    out.push_str(&emit_header(opts, uses_ffi, &intrinsics));

    // Forward-declare all subroutines so they can call each other
    // regardless of definition order (mutual recursion).
    for name in &ctx.subroutines {
        out.push_str(&format!("void bfpp_sub_{}(void);\n", mangle_name(name)));
    }
    if !ctx.subroutines.is_empty() {
        out.push('\n');
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
        out.push_str("bfpp_fb_init();\n");
        indent(&mut out, ctx.indent);
        out.push_str("#endif\n");
    }

    emit_nodes_skip_subdefs(&mut out, &program.nodes, &mut ctx);

    if opts.framebuffer.is_some() {
        indent(&mut out, ctx.indent);
        out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
        indent(&mut out, ctx.indent);
        out.push_str("bfpp_fb_cleanup();\n");
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
    // Intrinsic-specific headers
    if intrinsics.terminal {
        h.push_str("#include <termios.h>\n");
        h.push_str("#include <sys/ioctl.h>\n");
    }
    if intrinsics.time {
        h.push_str("#include <time.h>\n");
    }
    if intrinsics.poll {
        h.push_str("#include <poll.h>\n");
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
    h.push_str("static uint8_t tape[TAPE_SIZE];\n");
    h.push_str("static int ptr = 0;\n");
    h.push_str("static int bfpp_err = 0;\n");
    h.push_str("static uint64_t stack[STACK_SIZE];\n");
    h.push_str("static int sp = 0;\n");
    h.push_str("static int bfpp_call_depth = 0;\n");
    h.push_str("static uint8_t cell_width[TAPE_SIZE]; /* 0=continuation, 1,2,4,8 */\n");
    // Terminal intrinsic state — saved termios for raw/restore
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

    // SDL2 framebuffer system. Guarded by #ifdef BFPP_FRAMEBUFFER so the
    // generated code compiles without SDL2 if framebuffer wasn't requested.
    //
    // bfpp_fb_init: creates SDL window + renderer + streaming texture.
    //   Falls back to software renderer if accelerated isn't available.
    //   All three SDL objects are NULL-checked — if any creation step fails,
    //   earlier objects are destroyed and the fb is left disabled (NULL).
    //
    // bfpp_fb_flush (invoked by `F`): uploads tape[BFPP_FB_OFFSET..] as
    //   RGB24 pixels to the texture, renders it, and pumps the SDL event
    //   queue. Handles SDL_QUIT by cleaning up and exit(0). The NULL checks
    //   on texture/renderer make flush a safe no-op if init failed.
    //
    // bfpp_fb_cleanup: teardown in reverse allocation order. Each pointer
    //   is NULL-checked so cleanup is safe even if init partially failed.
    if opts.framebuffer.is_some() {
        h.push_str(r#"
#ifdef BFPP_FRAMEBUFFER
static SDL_Window *bfpp_fb_window = NULL;
static SDL_Renderer *bfpp_fb_renderer = NULL;
static SDL_Texture *bfpp_fb_texture = NULL;

static void bfpp_fb_init(void) {
    if (SDL_Init(SDL_INIT_VIDEO) < 0) {
        fprintf(stderr, "SDL init failed: %s\n", SDL_GetError());
        return;
    }
    bfpp_fb_window = SDL_CreateWindow("BF++",
        SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
        BFPP_FB_WIDTH, BFPP_FB_HEIGHT, SDL_WINDOW_SHOWN);
    if (!bfpp_fb_window) {
        fprintf(stderr, "SDL window failed: %s\n", SDL_GetError());
        SDL_Quit();
        return;
    }
    bfpp_fb_renderer = SDL_CreateRenderer(bfpp_fb_window, -1,
        SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC);
    if (!bfpp_fb_renderer) {
        bfpp_fb_renderer = SDL_CreateRenderer(bfpp_fb_window, -1, 0);
    }
    if (!bfpp_fb_renderer) {
        SDL_DestroyWindow(bfpp_fb_window);
        bfpp_fb_window = NULL;
        SDL_Quit();
        return;
    }
    bfpp_fb_texture = SDL_CreateTexture(bfpp_fb_renderer,
        SDL_PIXELFORMAT_RGB24, SDL_TEXTUREACCESS_STREAMING,
        BFPP_FB_WIDTH, BFPP_FB_HEIGHT);
}

static void bfpp_fb_flush(void) {
    if (!bfpp_fb_texture || !bfpp_fb_renderer) return;
    SDL_UpdateTexture(bfpp_fb_texture, NULL, &tape[BFPP_FB_OFFSET], BFPP_FB_WIDTH * 3);
    SDL_RenderClear(bfpp_fb_renderer);
    SDL_RenderCopy(bfpp_fb_renderer, bfpp_fb_texture, NULL, NULL);
    SDL_RenderPresent(bfpp_fb_renderer);
    SDL_Event e;
    while (SDL_PollEvent(&e)) {
        if (e.type == SDL_QUIT) {
            SDL_DestroyTexture(bfpp_fb_texture);
            SDL_DestroyRenderer(bfpp_fb_renderer);
            SDL_DestroyWindow(bfpp_fb_window);
            SDL_Quit();
            exit(0);
        }
    }
}

static void bfpp_fb_cleanup(void) {
    if (bfpp_fb_texture) SDL_DestroyTexture(bfpp_fb_texture);
    if (bfpp_fb_renderer) SDL_DestroyRenderer(bfpp_fb_renderer);
    if (bfpp_fb_window) SDL_DestroyWindow(bfpp_fb_window);
    SDL_Quit();
}
#endif

"#);
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

        // Framebuffer flush
        AstNode::FramebufferFlush => {
            indent(out, ctx.indent);
            out.push_str("#ifdef BFPP_FRAMEBUFFER\n");
            indent(out, ctx.indent);
            out.push_str("bfpp_fb_flush();\n");
            indent(out, ctx.indent);
            out.push_str("#endif\n");
        }

        // Numeric literal: set current cell to an immediate value
        AstNode::SetValue(val) => {
            indent(out, ctx.indent);
            out.push_str(&format!("bfpp_set(ptr, {}ULL);\n", val));
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
        "__term_raw" => {
            indent(out, ctx.indent);
            out.push_str("{ struct termios raw = bfpp_saved_termios; raw.c_lflag &= ~(ECHO | ICANON | ISIG); raw.c_cc[VMIN] = 1; raw.c_cc[VTIME] = 0; if (tcsetattr(0, TCSAFLUSH, &raw) < 0) bfpp_err = BFPP_ERR_IO; else bfpp_term_raw = 1; }\n");
        }
        "__term_restore" => {
            indent(out, ctx.indent);
            out.push_str("if (bfpp_term_raw) { tcsetattr(0, TCSAFLUSH, &bfpp_saved_termios); bfpp_term_raw = 0; }\n");
        }
        "__term_size" => {
            // Output: tape[ptr]=cols, tape[ptr+1]=rows
            indent(out, ctx.indent);
            out.push_str("{ struct winsize ws; if (ioctl(0, TIOCGWINSZ, &ws) == 0) { bfpp_set(ptr, ws.ws_col); bfpp_set((ptr+1) & TAPE_MASK, ws.ws_row); } else { bfpp_err = BFPP_ERR_IO; } }\n");
        }
        "__term_alt_on" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1049h\", stdout); fflush(stdout);\n");
        }
        "__term_alt_off" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1049l\", stdout); fflush(stdout);\n");
        }
        "__term_mouse_on" => {
            indent(out, ctx.indent);
            out.push_str("fputs(\"\\033[?1000h\\033[?1006h\", stdout); fflush(stdout);\n");
        }
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

        // ── Unrecognized intrinsic ───────────────────────────
        _ => {
            indent(out, ctx.indent);
            out.push_str(&format!("/* WARNING: unknown intrinsic !#{} */\n", name));
        }
    }
}

// Scan AST for intrinsic usage. Returns which categories are used so
// emit_header can include the right C headers and state variables.
#[derive(Default)]
struct IntrinsicUsage {
    terminal: bool,  // __term_* intrinsics
    time: bool,      // __sleep, __time_ms
    env: bool,       // __getenv
    process: bool,   // __exit, __getpid
    poll: bool,      // __poll_stdin
}

fn detect_intrinsics(nodes: &[AstNode]) -> IntrinsicUsage {
    let mut usage = IntrinsicUsage::default();
    scan_intrinsics(nodes, &mut usage);
    usage
}

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

    #[test]
    fn test_hello_world_generates() {
        let tokens = lex("++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("int main(void)"));
        assert!(result.c_source.contains("while (bfpp_get(ptr))"));
        assert!(result.c_source.contains("putchar"));
    }

    #[test]
    fn test_subroutine_codegen() {
        let tokens = lex("!#pr{.^} !#pr").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("void bfpp_sub_pr(void)"));
        assert!(result.c_source.contains("bfpp_sub_pr();"));
    }

    #[test]
    fn test_error_handling_codegen() {
        let tokens = lex("R{+}K{-}").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("saved_err"));
        assert!(result.c_source.contains("if (bfpp_err)"));
    }

    #[test]
    fn test_name_mangling() {
        assert_eq!(mangle_name(".>"), "dotgt");
        assert_eq!(mangle_name("pr"), "pr");
        assert_eq!(mangle_name("m*"), "mstar");
        assert_eq!(mangle_name("tcp"), "tcp");
    }

    #[test]
    fn test_tape_addr_codegen() {
        let tokens = lex("T").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_push((uint64_t)(uintptr_t)&tape[ptr])"));
    }

    #[test]
    fn test_framebuffer_flush_codegen() {
        let tokens = lex("F").unwrap();
        let program = parse(&tokens).unwrap();
        let result = generate(&program, &CodegenOptions::default());
        assert!(result.c_source.contains("bfpp_fb_flush"));
    }

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
}
