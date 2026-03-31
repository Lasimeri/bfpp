/// BF++ Abstract Syntax Tree node types.

#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    // Core BF ops (with coalesced counts for optimizer)
    MoveRight(usize),       // > (count)
    MoveLeft(usize),        // < (count)
    Increment(usize),       // + (count)
    Decrement(usize),       // - (count)
    Output,                 // .
    Input,                  // ,
    Loop(Vec<AstNode>),     // [...]

    // Extended memory & data
    AbsoluteAddr,           // @
    Deref(Box<AstNode>),    // * (wraps the next op)
    CellWidthCycle,         // %
    StringLit(Vec<u8>),     // "..."

    // Stack
    Push,                   // $
    Pop,                    // ~

    // Subroutines
    SubDef(String, Vec<AstNode>), // !#name{...}
    SubCall(String),              // !#name
    Return,                       // ^

    // Syscall
    Syscall,                // backslash
    OutputFd(FdSpec),       // .{N}
    InputFd(FdSpec),        // ,{N}

    // Bitwise & arithmetic
    BitOr,                  // |
    BitAnd,                 // &
    BitXor,                 // x
    ShiftLeft,              // s
    ShiftRight,             // r
    BitNot,                 // n

    // Error handling
    ErrorRead,              // E
    ErrorWrite,             // e
    Propagate,              // ?
    ResultBlock(Vec<AstNode>, Vec<AstNode>), // R{...}K{...}

    // Tape address & framebuffer
    TapeAddr,               // T — push &tape[ptr] onto stack
    FramebufferFlush,       // F — flush framebuffer

    // FFI
    FfiCall(String, String), // \ffi "lib" "func"

    // Optimizer synthetic nodes
    Clear,                  // [-] optimized
    ScanRight,              // [>] optimized
    ScanLeft,               // [<] optimized
    MultiplyMove(Vec<(isize, usize)>), // [->>+++<<] pattern: (offset, factor) pairs
}

#[derive(Debug, Clone, PartialEq)]
pub enum FdSpec {
    Literal(u32),  // .{3}
    Indirect,      // .{*} — fd from tape[ptr+1]
}

/// A complete BF++ program.
#[derive(Debug, Clone)]
pub struct Program {
    pub nodes: Vec<AstNode>,
}
