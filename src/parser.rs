// BF++ Parser — transforms a flat token stream into a structured AST.
//
// Architecture: recursive-descent parser driven by `parse_block` / `parse_single`.
//
// `parse_block` consumes tokens until it hits the expected `BlockEnd` terminator
// (EOF, `]`, or `}`). Each iteration calls `parse_single` to consume one logical
// node. This naturally handles nesting: when `parse_single` sees a `[`, it calls
// `parse_block` with `BlockEnd::LoopEnd`, which recurses until the matching `]`.
// Same pattern for `{`-delimited subroutine bodies and R/K blocks.
//
// Coalescing: consecutive identical movement/arithmetic tokens (>, <, +, -)
// are collapsed into a single AST node with a count. This happens in
// `parse_single` via `count_consecutive` — after consuming the first token,
// it greedily eats all following tokens of the same kind.
//
// Bracket matching: handled structurally by the recursive descent. An unmatched
// `[` surfaces as `parse_block` reaching EOF while expecting `BlockEnd::LoopEnd`.
// A stray `]` outside any loop is caught by the "unexpected terminator" check
// at the top of `parse_block`.

use crate::ast::{AstNode, FdSpec, Program};
use crate::lexer::Token;

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub token_index: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Parse error at token {}: {}", self.token_index, self.message)
    }
}

// Entry point: parse the full token stream into a Program.
// Wraps parse_block with BlockEnd::Eof so the entire input is consumed.
pub fn parse(tokens: &[Token]) -> Result<Program, ParseError> {
    let mut pos = 0;
    let nodes = parse_block(tokens, &mut pos, BlockEnd::Eof)?;
    Ok(Program { nodes })
}

// Determines what token ends the current block context.
// The recursive descent uses this to know when to stop and return:
//   - Eof: top-level program, stop at end of input
//   - LoopEnd: inside [...], stop at `]`
//   - BraceClose: inside {...}, stop at `}` (subroutine bodies, R/K blocks)
#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockEnd {
    Eof,
    LoopEnd,     // ]
    BraceClose,  // }
}

// Parse a sequence of nodes until the expected block terminator.
//
// This is the core recursive-descent driver. On each iteration it:
// 1. Checks if the current token is the expected terminator → return collected nodes
// 2. Checks for unexpected terminators (e.g., `]` when expecting `}`) → error
// 3. Otherwise delegates to parse_single to consume one node
//
// If we reach end-of-input without finding the expected terminator (and it's
// not Eof), that's an unterminated bracket/brace error.
fn parse_block(tokens: &[Token], pos: &mut usize, end: BlockEnd) -> Result<Vec<AstNode>, ParseError> {
    let mut nodes = Vec::new();

    while *pos < tokens.len() {
        let token = &tokens[*pos];

        // Check for block terminator — consume it and return
        match (end, token) {
            (BlockEnd::LoopEnd, Token::LoopEnd) => {
                *pos += 1;
                return Ok(nodes);
            }
            (BlockEnd::BraceClose, Token::BraceClose) => {
                *pos += 1;
                return Ok(nodes);
            }
            _ => {}
        }

        // A `]` when we're not inside a loop is always an error.
        // (A `}` outside a brace context is handled by parse_single as a fallthrough.)
        if matches!(token, Token::LoopEnd) && end != BlockEnd::LoopEnd {
            return Err(ParseError {
                message: "Unexpected ']' without matching '['".into(),
                token_index: *pos,
            });
        }

        let node = parse_single(tokens, pos)?;
        nodes.push(node);
    }

    // Reached end of input — only valid if we expected Eof
    match end {
        BlockEnd::Eof => Ok(nodes),
        BlockEnd::LoopEnd => Err(ParseError {
            message: "Unterminated '[' — missing ']'".into(),
            token_index: *pos,
        }),
        BlockEnd::BraceClose => Err(ParseError {
            message: "Unterminated '{' — missing '}'".into(),
            token_index: *pos,
        }),
    }
}

// Parse a single AST node from the current token position.
//
// Consumes tokens[*pos] and advances pos. For most tokens this is a 1:1
// mapping. Special cases:
//
// - Movement/arithmetic tokens: after consuming the first, `count_consecutive`
//   greedily eats all identical following tokens. `>>>>` becomes MoveRight(4).
//
// - Deref (`*`): recursively calls parse_single to wrap the NEXT op. So `*+`
//   parses as Deref(Increment(1)). If `*` is at end of input, that's an error —
//   deref must always have a target.
//
// - LoopStart (`[`): calls parse_block with LoopEnd to consume the loop body.
//
// - SubDef (`!#name{`): the lexer already consumed the `{`, so we call
//   parse_block with BraceClose to get the body. SubCall (`!#name`) is a
//   simple leaf node — no body to parse.
//
// - ResultStart (`R{`): parse the result body, then REQUIRE a CatchStart (`K{`)
//   immediately after. This enforces the R{...}K{...} pairing at parse time.
//
// - CatchStart (`K{`) appearing WITHOUT a preceding R{} is an error — caught
//   as a fallthrough case at the bottom.
fn parse_single(tokens: &[Token], pos: &mut usize) -> Result<AstNode, ParseError> {
    let token = &tokens[*pos];
    *pos += 1;

    match token {
        // ── Core BF — coalesce consecutive identical ops ─────────────
        Token::MoveRight => {
            let count = 1 + count_consecutive(tokens, pos, &Token::MoveRight);
            Ok(AstNode::MoveRight(count))
        }
        Token::MoveLeft => {
            let count = 1 + count_consecutive(tokens, pos, &Token::MoveLeft);
            Ok(AstNode::MoveLeft(count))
        }
        Token::Increment => {
            let count = 1 + count_consecutive(tokens, pos, &Token::Increment);
            Ok(AstNode::Increment(count))
        }
        Token::Decrement => {
            let count = 1 + count_consecutive(tokens, pos, &Token::Decrement);
            Ok(AstNode::Decrement(count))
        }
        Token::Output => Ok(AstNode::Output),
        Token::Input => Ok(AstNode::Input),

        // ── Loop: recurse into parse_block until `]` ─────────────────
        Token::LoopStart => {
            let body = parse_block(tokens, pos, BlockEnd::LoopEnd)?;
            Ok(AstNode::Loop(body))
        }

        // ── Extended memory ──────────────────────────────────────────
        Token::AbsoluteAddr => Ok(AstNode::AbsoluteAddr),
        Token::Deref => {
            // Deref wraps the next op: `*+` → Deref(Increment(1))
            // Must have a following token to wrap; bare `*` at EOF is an error.
            if *pos < tokens.len() {
                let inner = parse_single(tokens, pos)?;
                Ok(AstNode::Deref(Box::new(inner)))
            } else {
                Err(ParseError {
                    message: "Dereference '*' at end of input — missing target operator".into(),
                    token_index: *pos - 1,
                })
            }
        }
        Token::CellWidthCycle => Ok(AstNode::CellWidthCycle),

        // ── String literal ───────────────────────────────────────────
        Token::StringLit(bytes) => Ok(AstNode::StringLit(bytes.clone())),

        // ── Stack ────────────────────────────────────────────────────
        Token::Push => Ok(AstNode::Push),
        Token::Pop => Ok(AstNode::Pop),

        // ── Subroutines ─────────────────────────────────────────────
        // SubDef: the lexer already consumed the opening `{`, so we parse
        // the body until `}` via BraceClose.
        Token::SubDef(name) => {
            let body = parse_block(tokens, pos, BlockEnd::BraceClose)?;
            Ok(AstNode::SubDef(name.clone(), body))
        }
        // SubCall: bare name reference, no body to parse.
        Token::SubCall(name) => Ok(AstNode::SubCall(name.clone())),
        Token::Return => Ok(AstNode::Return),

        // ── Syscall & fd-directed I/O ────────────────────────────────
        Token::Syscall => Ok(AstNode::Syscall),
        Token::OutputFd(fd) => Ok(AstNode::OutputFd(convert_fd(fd))),
        Token::InputFd(fd) => Ok(AstNode::InputFd(convert_fd(fd))),

        // ── Bitwise ─────────────────────────────────────────────────
        Token::BitOr => Ok(AstNode::BitOr),
        Token::BitAnd => Ok(AstNode::BitAnd),
        Token::BitXor => Ok(AstNode::BitXor),
        Token::ShiftLeft => Ok(AstNode::ShiftLeft),
        Token::ShiftRight => Ok(AstNode::ShiftRight),
        Token::BitNot => Ok(AstNode::BitNot),

        // ── Error handling ──────────────────────────────────────────
        Token::ErrorRead => Ok(AstNode::ErrorRead),
        Token::ErrorWrite => Ok(AstNode::ErrorWrite),
        Token::Propagate => Ok(AstNode::Propagate),

        // R{...}K{...} — result/catch block pair.
        // Parse the R body first, then enforce that K{ follows immediately.
        // This is the only place where two consecutive block constructs are
        // required to be paired — the parser rejects an orphan R{} or K{}.
        Token::ResultStart => {
            let result_body = parse_block(tokens, pos, BlockEnd::BraceClose)?;
            // K{ must follow immediately — no intervening tokens allowed
            if *pos < tokens.len() && tokens[*pos] == Token::CatchStart {
                *pos += 1;
                let catch_body = parse_block(tokens, pos, BlockEnd::BraceClose)?;
                Ok(AstNode::ResultBlock(result_body, catch_body))
            } else {
                Err(ParseError {
                    message: "R{...} block must be followed by K{...} catch block".into(),
                    token_index: *pos,
                })
            }
        }

        // ── Tape address & framebuffer ──────────────────────────────
        Token::TapeAddr => Ok(AstNode::TapeAddr),
        Token::FramebufferFlush => Ok(AstNode::FramebufferFlush),

        // ── Immediate value & direct width ─────────────────────────
        Token::NumericLit(val) => Ok(AstNode::SetValue(*val)),
        Token::SetCellWidth(w) => Ok(AstNode::SetCellWidth(*w)),

        // ── FFI ─────────────────────────────────────────────────────
        Token::FfiCall(lib, func) => Ok(AstNode::FfiCall(lib.clone(), func.clone())),

        // ── Error cases: terminators appearing in wrong context ─────
        // A bare K{ without a preceding R{} — the R branch above is the
        // only valid way to enter a catch block.
        Token::CatchStart => {
            Err(ParseError {
                message: "K{...} catch block without preceding R{...} result block".into(),
                token_index: *pos - 1,
            })
        }

        Token::BraceClose => {
            Err(ParseError {
                message: "Unexpected '}'".into(),
                token_index: *pos - 1,
            })
        }

        Token::LoopEnd => {
            Err(ParseError {
                message: "Unexpected ']'".into(),
                token_index: *pos - 1,
            })
        }
    }
}

/// Count consecutive tokens matching `target`, advancing `pos`.
/// Used by the coalescing logic: after consuming the first `+`, this eats
/// all following `+` tokens and returns the extra count (so total = 1 + result).
fn count_consecutive(tokens: &[Token], pos: &mut usize, target: &Token) -> usize {
    let mut count = 0;
    while *pos < tokens.len() && &tokens[*pos] == target {
        count += 1;
        *pos += 1;
    }
    count
}

// Convert lexer FdSpec to AST FdSpec.
// These are structurally identical but live in separate modules to keep the
// lexer and AST decoupled — the AST shouldn't depend on lexer types.
fn convert_fd(fd: &crate::lexer::FdSpec) -> FdSpec {
    match fd {
        crate::lexer::FdSpec::Literal(n) => FdSpec::Literal(*n),
        crate::lexer::FdSpec::Indirect => FdSpec::Indirect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    #[test]
    fn test_simple_bf() {
        let tokens = lex("+++[>++<-]>.").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 4); // Inc(3), Loop, MoveRight(1), Output
    }

    #[test]
    fn test_nested_loops() {
        let tokens = lex("[[[]]]").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 1);
        if let AstNode::Loop(outer) = &program.nodes[0] {
            if let AstNode::Loop(middle) = &outer[0] {
                assert!(matches!(&middle[0], AstNode::Loop(_)));
            } else {
                panic!("Expected nested loop");
            }
        } else {
            panic!("Expected loop");
        }
    }

    #[test]
    fn test_unmatched_bracket() {
        let tokens = lex("[+").unwrap();
        assert!(parse(&tokens).is_err());
    }

    #[test]
    fn test_subroutine_def_and_call() {
        let tokens = lex("!#pr{.^} !#pr").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 2);
        assert!(matches!(&program.nodes[0], AstNode::SubDef(name, _) if name == "pr"));
        assert!(matches!(&program.nodes[1], AstNode::SubCall(name) if name == "pr"));
    }

    #[test]
    fn test_result_catch() {
        let tokens = lex("R{+}K{-}").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 1);
        if let AstNode::ResultBlock(r, k) = &program.nodes[0] {
            assert_eq!(r.len(), 1);
            assert_eq!(k.len(), 1);
        } else {
            panic!("Expected ResultBlock");
        }
    }

    #[test]
    fn test_coalescing() {
        let tokens = lex("++++>>>>").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 2);
        assert_eq!(program.nodes[0], AstNode::Increment(4));
        assert_eq!(program.nodes[1], AstNode::MoveRight(4));
    }

    #[test]
    fn test_deref() {
        let tokens = lex("*+").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 1);
        if let AstNode::Deref(inner) = &program.nodes[0] {
            assert_eq!(**inner, AstNode::Increment(1));
        } else {
            panic!("Expected Deref");
        }
    }

    #[test]
    fn test_tape_addr_and_framebuffer() {
        let tokens = lex("T F").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 2);
        assert_eq!(program.nodes[0], AstNode::TapeAddr);
        assert_eq!(program.nodes[1], AstNode::FramebufferFlush);
    }

    #[test]
    fn test_ffi_call() {
        let tokens = lex(r#"\ffi "libm.so.6" "ceil""#).unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 1);
        assert!(matches!(&program.nodes[0], AstNode::FfiCall(lib, func) if lib == "libm.so.6" && func == "ceil"));
    }

    #[test]
    fn test_numeric_literal() {
        let tokens = lex("#42 .").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 2);
        assert_eq!(program.nodes[0], AstNode::SetValue(42));
        assert_eq!(program.nodes[1], AstNode::Output);
    }

    #[test]
    fn test_direct_cell_width() {
        let tokens = lex("%8 #100").unwrap();
        let program = parse(&tokens).unwrap();
        assert_eq!(program.nodes.len(), 2);
        assert_eq!(program.nodes[0], AstNode::SetCellWidth(8));
        assert_eq!(program.nodes[1], AstNode::SetValue(100));
    }
}
