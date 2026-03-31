// BF++ Abstract Syntax Tree
//
// Defines the node types emitted by the parser. Each AstNode variant maps to
// either a single BF++ token or a structured construct (loop, subroutine,
// result/catch block). The parser coalesces consecutive identical movement and
// arithmetic tokens into a single node with a count (e.g., `++++` → Increment(4)),
// so the AST is already partially optimized before the optimizer pass runs.
//
// The optimizer may later replace certain Loop patterns with synthetic nodes
// (Clear, ScanRight, ScanLeft, MultiplyMove) that have no corresponding source
// token — these exist only as optimization artifacts for codegen.

/// BF++ Abstract Syntax Tree node types.

#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    // ── Core BF ops ──────────────────────────────────────────────────
    // Counts are coalesced by the parser: N consecutive identical tokens
    // become a single node with count N. This reduces AST size and lets
    // codegen emit `ptr += 4` instead of four separate `ptr += 1` calls.
    MoveRight(usize),       // > (count)
    MoveLeft(usize),        // < (count)
    Increment(usize),       // + (count)
    Decrement(usize),       // - (count)
    Output,                 // .
    Input,                  // ,
    Loop(Vec<AstNode>),     // [...] — body nodes between brackets

    // ── Extended memory & data ───────────────────────────────────────
    AbsoluteAddr,           // @ — set pointer to value of current cell (absolute jump)
    Deref(Box<AstNode>),    // * — treat current cell as a pointer: save ptr, jump to
                            //   tape[current_cell], execute the wrapped op there, then
                            //   restore ptr. The parser enforces that * always wraps
                            //   exactly one subsequent op (recursive parse_single call).
    CellWidthCycle,         // % — cycle cell bit-width (8 → 16 → 32 → 64 → 8)
    StringLit(Vec<u8>),     // "..." — raw bytes of the string literal, written to
                            //   consecutive cells starting at current pointer

    // ── Stack ────────────────────────────────────────────────────────
    Push,                   // $ — push current cell value onto the auxiliary stack
    Pop,                    // ~ — pop top of stack into current cell

    // ── Subroutines ─────────────────────────────────────────────────
    // SubDef defines a named subroutine with a body; SubCall invokes one.
    // The lexer distinguishes them: `!#name{` starts a def (followed by a
    // brace-delimited body), while a bare `!#name` (no brace) is a call.
    // The analyzer enforces: no calls to undefined subs, no duplicate defs.
    SubDef(String, Vec<AstNode>), // !#name{...} — definition with body
    SubCall(String),              // !#name      — call site (name must match a def)
    Return,                       // ^ — early return from subroutine (or main)

    // ── Syscall & file-descriptor I/O ───────────────────────────────
    Syscall,                // \ — raw syscall (args read from tape layout)
    OutputFd(FdSpec),       // .{N} or .{*} — write current cell to a specific fd
    InputFd(FdSpec),        // ,{N} or ,{*} — read into current cell from a specific fd

    // ── Bitwise & arithmetic ────────────────────────────────────────
    // All bitwise ops operate on the current cell in-place.
    BitOr,                  // |
    BitAnd,                 // &
    BitXor,                 // x
    ShiftLeft,              // s
    ShiftRight,             // r
    BitNot,                 // n

    // ── Error handling ──────────────────────────────────────────────
    ErrorRead,              // E — read errno into current cell
    ErrorWrite,             // e — write current cell to errno
    Propagate,              // ? — if errno is set, propagate (return from sub)
    ResultBlock(Vec<AstNode>, Vec<AstNode>), // R{...}K{...} — try/catch analog.
                            // First vec is the "result" (try) body, second is the
                            // "catch" (K) body. Parser enforces that R{} is always
                            // immediately followed by K{} — an orphan R or K is an error.

    // ── Tape address & framebuffer ──────────────────────────────────
    TapeAddr,               // T — push &tape[ptr] onto stack (raw pointer)
    FramebufferFlush,       // F — flush framebuffer to display

    // ── FFI ─────────────────────────────────────────────────────────
    // Foreign function interface: call a C function from a shared library.
    // The analyzer validates that neither lib nor func name is empty.
    FfiCall(String, String), // \ffi "lib" "func"

    // ── Optimizer synthetic nodes ───────────────────────────────────
    // These never appear in parser output. The optimizer rewrites certain
    // Loop patterns into these more efficient representations for codegen.
    Clear,                  // [-] → set cell to 0
    ScanRight,              // [>] → scan right for first zero cell
    ScanLeft,               // [<] → scan left for first zero cell
    MultiplyMove(Vec<(isize, usize)>), // [->>+++<<] pattern: list of (offset, factor)
                            // pairs. Distributes the current cell's value to cells at
                            // relative offsets, multiplied by the given factors, then
                            // clears the current cell.
}

// File descriptor specifier for directed I/O (.{N} and ,{N}).
// Literal: the fd number is baked into the source (e.g., .{2} writes to stderr).
// Indirect: the fd number is read from tape[ptr+1] at runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum FdSpec {
    Literal(u32),  // .{3}  — compile-time fd
    Indirect,      // .{*}  — runtime fd from tape[ptr+1]
}

/// A complete BF++ program.
#[derive(Debug, Clone)]
pub struct Program {
    pub nodes: Vec<AstNode>,
}
