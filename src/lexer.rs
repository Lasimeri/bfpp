/// BF++ Lexer — tokenizes source into a stream of tokens.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Core BF
    MoveRight,
    MoveLeft,
    Increment,
    Decrement,
    Output,
    Input,
    LoopStart,
    LoopEnd,

    // Extended memory
    AbsoluteAddr,   // @
    Deref,          // *
    CellWidthCycle, // %

    // String literal (already parsed bytes)
    StringLit(Vec<u8>),

    // Stack
    Push,  // $
    Pop,   // ~

    // Subroutines
    SubDef(String),   // !#name{ — name extracted, { consumed
    SubCall(String),  // !#name (not followed by {)
    BraceClose,       // }
    Return,           // ^

    // Syscall
    Syscall,          // backslash (standalone)
    OutputFd(FdSpec), // .{N}
    InputFd(FdSpec),  // ,{N}

    // Bitwise
    BitOr,       // |
    BitAnd,      // &
    BitXor,      // x
    ShiftLeft,   // s
    ShiftRight,  // r
    BitNot,      // n

    // Error handling
    ErrorRead,   // E
    ErrorWrite,  // e
    Propagate,   // ?
    ResultStart, // R{
    CatchStart,  // K{

    // Tape address & framebuffer
    TapeAddr,          // T
    FramebufferFlush,  // F

    // FFI
    FfiCall(String, String), // \ffi "lib" "func"
}

#[derive(Debug, Clone, PartialEq)]
pub enum FdSpec {
    Literal(u32),
    Indirect,
}

#[derive(Debug, Clone)]
pub struct LexError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    let mut line = 1usize;
    let mut col = 1usize;

    while let Some(&ch) = chars.peek() {
        match ch {
            // Comment — skip to end of line
            ';' => {
                while let Some(&c) = chars.peek() {
                    if c == '\n' {
                        break;
                    }
                    chars.next();
                    col += 1;
                }
            }

            // Newline
            '\n' => {
                chars.next();
                line += 1;
                col = 1;
                continue;
            }

            // Core BF ops
            '>' => { chars.next(); col += 1; tokens.push(Token::MoveRight); }
            '<' => { chars.next(); col += 1; tokens.push(Token::MoveLeft); }
            '+' => { chars.next(); col += 1; tokens.push(Token::Increment); }
            '-' => { chars.next(); col += 1; tokens.push(Token::Decrement); }
            '[' => { chars.next(); col += 1; tokens.push(Token::LoopStart); }
            ']' => { chars.next(); col += 1; tokens.push(Token::LoopEnd); }

            // . — could be Output or .{N} fd-extended
            '.' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'{') {
                    let fd = parse_fd_spec(&mut chars, &mut col, line)?;
                    tokens.push(Token::OutputFd(fd));
                } else {
                    tokens.push(Token::Output);
                }
            }

            // , — could be Input or ,{N}
            ',' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'{') {
                    let fd = parse_fd_spec(&mut chars, &mut col, line)?;
                    tokens.push(Token::InputFd(fd));
                } else {
                    tokens.push(Token::Input);
                }
            }

            // Extended memory
            '@' => { chars.next(); col += 1; tokens.push(Token::AbsoluteAddr); }
            '*' => { chars.next(); col += 1; tokens.push(Token::Deref); }
            '%' => { chars.next(); col += 1; tokens.push(Token::CellWidthCycle); }

            // Stack
            '$' => { chars.next(); col += 1; tokens.push(Token::Push); }
            '~' => { chars.next(); col += 1; tokens.push(Token::Pop); }

            // Return
            '^' => { chars.next(); col += 1; tokens.push(Token::Return); }

            // String literal
            '"' => {
                let s = parse_string_literal(&mut chars, &mut col, &mut line)?;
                tokens.push(Token::StringLit(s));
            }

            // Subroutine def/call: !#name{ or !#name
            '!' => {
                chars.next(); col += 1;
                if chars.peek() != Some(&'#') {
                    return Err(LexError {
                        message: "Expected '#' after '!' for subroutine".into(),
                        line, col,
                    });
                }
                chars.next(); col += 1; // consume #
                let name = parse_sub_name(&mut chars, &mut col);
                if name.is_empty() {
                    return Err(LexError {
                        message: "Empty subroutine name after '!#'".into(),
                        line, col,
                    });
                }
                if chars.peek() == Some(&'{') {
                    chars.next(); col += 1;
                    tokens.push(Token::SubDef(name));
                } else {
                    tokens.push(Token::SubCall(name));
                }
            }

            // Brace close (ends subroutine def, R block, or K block)
            '}' => { chars.next(); col += 1; tokens.push(Token::BraceClose); }

            // Syscall: \ or \ffi "lib" "func"
            '\\' => {
                chars.next(); col += 1;
                // Check for \ffi
                if chars.peek() == Some(&'f') {
                    let mut lookahead = chars.clone();
                    lookahead.next(); // f
                    if lookahead.peek() == Some(&'f') {
                        lookahead.next(); // second f
                        if lookahead.peek() == Some(&'i') {
                            // Consume ffi
                            chars.next(); col += 1; // f
                            chars.next(); col += 1; // f
                            chars.next(); col += 1; // i
                            // Skip whitespace
                            while chars.peek().map_or(false, |c| c.is_whitespace() && *c != '\n') {
                                chars.next(); col += 1;
                            }
                            // Parse first string literal (lib name)
                            if chars.peek() != Some(&'"') {
                                return Err(LexError {
                                    message: "Expected '\"' after \\ffi for library name".into(),
                                    line, col,
                                });
                            }
                            let lib = parse_string_literal(&mut chars, &mut col, &mut line)?;
                            let lib_name = String::from_utf8(lib).map_err(|_| LexError {
                                message: "Invalid UTF-8 in FFI library name".into(),
                                line, col,
                            })?;
                            // Skip whitespace
                            while chars.peek().map_or(false, |c| c.is_whitespace() && *c != '\n') {
                                chars.next(); col += 1;
                            }
                            // Parse second string literal (func name)
                            if chars.peek() != Some(&'"') {
                                return Err(LexError {
                                    message: "Expected '\"' after library name for function name".into(),
                                    line, col,
                                });
                            }
                            let func = parse_string_literal(&mut chars, &mut col, &mut line)?;
                            let func_name = String::from_utf8(func).map_err(|_| LexError {
                                message: "Invalid UTF-8 in FFI function name".into(),
                                line, col,
                            })?;
                            tokens.push(Token::FfiCall(lib_name, func_name));
                            continue;
                        }
                    }
                }
                tokens.push(Token::Syscall);
            }

            // Bitwise
            '|' => { chars.next(); col += 1; tokens.push(Token::BitOr); }
            '&' => { chars.next(); col += 1; tokens.push(Token::BitAnd); }

            // Error handling
            '?' => { chars.next(); col += 1; tokens.push(Token::Propagate); }

            // R{ and K{ — result/catch blocks
            'R' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'{') {
                    chars.next(); col += 1;
                    tokens.push(Token::ResultStart);
                } else {
                    return Err(LexError {
                        message: "Expected '{' after 'R' for result block".into(),
                        line, col,
                    });
                }
            }
            'K' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'{') {
                    chars.next(); col += 1;
                    tokens.push(Token::CatchStart);
                } else {
                    return Err(LexError {
                        message: "Expected '{' after 'K' for catch block".into(),
                        line, col,
                    });
                }
            }

            // Single-char alpha operators
            'x' => { chars.next(); col += 1; tokens.push(Token::BitXor); }
            's' => { chars.next(); col += 1; tokens.push(Token::ShiftLeft); }
            'r' => { chars.next(); col += 1; tokens.push(Token::ShiftRight); }
            'n' => { chars.next(); col += 1; tokens.push(Token::BitNot); }
            'E' => { chars.next(); col += 1; tokens.push(Token::ErrorRead); }
            'e' => { chars.next(); col += 1; tokens.push(Token::ErrorWrite); }
            'T' => { chars.next(); col += 1; tokens.push(Token::TapeAddr); }
            'F' => { chars.next(); col += 1; tokens.push(Token::FramebufferFlush); }

            // All other characters are ignored (whitespace, unrecognized chars)
            _ => {
                chars.next();
                col += 1;
            }
        }
    }

    Ok(tokens)
}

/// Parse {N} or {*} fd specifier. Assumes '{' is the next char.
fn parse_fd_spec(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    col: &mut usize,
    line: usize,
) -> Result<FdSpec, LexError> {
    // consume '{'
    chars.next(); *col += 1;

    if chars.peek() == Some(&'*') {
        chars.next(); *col += 1;
        if chars.peek() == Some(&'}') {
            chars.next(); *col += 1;
            return Ok(FdSpec::Indirect);
        }
        return Err(LexError {
            message: "Expected '}' after '{*'".into(),
            line, col: *col,
        });
    }

    let mut num_str = String::new();
    while let Some(&c) = chars.peek() {
        if c == '}' {
            chars.next(); *col += 1;
            break;
        }
        if c.is_ascii_digit() {
            num_str.push(c);
            chars.next(); *col += 1;
        } else {
            return Err(LexError {
                message: format!("Unexpected char '{}' in fd specifier", c),
                line, col: *col,
            });
        }
    }

    let fd: u32 = num_str.parse().map_err(|_| LexError {
        message: "Invalid fd number".into(),
        line, col: *col,
    })?;

    Ok(FdSpec::Literal(fd))
}

/// Parse a string literal, consuming the opening and closing quotes.
/// Handles escape sequences.
fn parse_string_literal(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    col: &mut usize,
    line: &mut usize,
) -> Result<Vec<u8>, LexError> {
    // consume opening "
    chars.next(); *col += 1;
    let start_line = *line;

    let mut bytes = Vec::new();
    loop {
        match chars.next() {
            None => {
                return Err(LexError {
                    message: "Unterminated string literal".into(),
                    line: start_line, col: *col,
                });
            }
            Some('"') => {
                *col += 1;
                break;
            }
            Some('\\') => {
                *col += 1;
                match chars.next() {
                    Some('0') => { *col += 1; bytes.push(0); }
                    Some('n') => { *col += 1; bytes.push(b'\n'); }
                    Some('r') => { *col += 1; bytes.push(b'\r'); }
                    Some('t') => { *col += 1; bytes.push(b'\t'); }
                    Some('\\') => { *col += 1; bytes.push(b'\\'); }
                    Some('"') => { *col += 1; bytes.push(b'"'); }
                    Some('x') => {
                        *col += 1;
                        let h1 = chars.next().ok_or(LexError {
                            message: "Unexpected end in \\x escape".into(),
                            line: *line, col: *col,
                        })?;
                        let h2 = chars.next().ok_or(LexError {
                            message: "Unexpected end in \\x escape".into(),
                            line: *line, col: *col,
                        })?;
                        *col += 2;
                        let hex = format!("{}{}", h1, h2);
                        let val = u8::from_str_radix(&hex, 16).map_err(|_| LexError {
                            message: format!("Invalid hex escape: \\x{}", hex),
                            line: *line, col: *col,
                        })?;
                        bytes.push(val);
                    }
                    Some(c) => {
                        return Err(LexError {
                            message: format!("Unknown escape sequence: \\{}", c),
                            line: *line, col: *col,
                        });
                    }
                    None => {
                        return Err(LexError {
                            message: "Unterminated escape in string".into(),
                            line: *line, col: *col,
                        });
                    }
                }
            }
            Some('\n') => {
                bytes.push(b'\n');
                *line += 1;
                *col = 1;
            }
            Some(c) => {
                *col += 1;
                // Encode as UTF-8 bytes
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                bytes.extend_from_slice(encoded.as_bytes());
            }
        }
    }

    Ok(bytes)
}

/// Parse subroutine name characters (after !#).
/// Valid chars: symbols used in BF++ ops + alphanumeric.
fn parse_sub_name(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    col: &mut usize,
) -> String {
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || is_sub_name_symbol(c) {
            name.push(c);
            chars.next();
            *col += 1;
        } else {
            break;
        }
    }
    name
}

fn is_sub_name_symbol(c: char) -> bool {
    matches!(c, '>' | '<' | '+' | '-' | '.' | ',' | '@' | '*' | '%'
        | '$' | '~' | '\\' | '|' | '&' | '^' | '_' | '/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_bf_ops() {
        let tokens = lex("><+-.,[]").unwrap();
        assert_eq!(tokens, vec![
            Token::MoveRight, Token::MoveLeft,
            Token::Increment, Token::Decrement,
            Token::Output, Token::Input,
            Token::LoopStart, Token::LoopEnd,
        ]);
    }

    #[test]
    fn test_comment() {
        let tokens = lex("+++ ; this is a comment\n---").unwrap();
        assert_eq!(tokens, vec![
            Token::Increment, Token::Increment, Token::Increment,
            Token::Decrement, Token::Decrement, Token::Decrement,
        ]);
    }

    #[test]
    fn test_string_literal() {
        let tokens = lex(r#""Hello\n\0""#).unwrap();
        assert_eq!(tokens, vec![
            Token::StringLit(vec![72, 101, 108, 108, 111, 10, 0]),
        ]);
    }

    #[test]
    fn test_subroutine_def() {
        let tokens = lex("!#pr{[.>]^}").unwrap();
        assert_eq!(tokens, vec![
            Token::SubDef("pr".into()),
            Token::LoopStart, Token::Output, Token::MoveRight, Token::LoopEnd,
            Token::Return,
            Token::BraceClose,
        ]);
    }

    #[test]
    fn test_subroutine_call() {
        let tokens = lex("!#pr").unwrap();
        assert_eq!(tokens, vec![Token::SubCall("pr".into())]);
    }

    #[test]
    fn test_fd_extended_io() {
        let tokens = lex(".{2} ,{3}").unwrap();
        assert_eq!(tokens, vec![
            Token::OutputFd(FdSpec::Literal(2)),
            Token::InputFd(FdSpec::Literal(3)),
        ]);
    }

    #[test]
    fn test_fd_indirect() {
        let tokens = lex(".{*}").unwrap();
        assert_eq!(tokens, vec![Token::OutputFd(FdSpec::Indirect)]);
    }

    #[test]
    fn test_error_handling_ops() {
        let tokens = lex("E e ? R{ } K{ }").unwrap();
        assert_eq!(tokens, vec![
            Token::ErrorRead, Token::ErrorWrite, Token::Propagate,
            Token::ResultStart, Token::BraceClose,
            Token::CatchStart, Token::BraceClose,
        ]);
    }

    #[test]
    fn test_bitwise_ops() {
        let tokens = lex("| & x s r n").unwrap();
        assert_eq!(tokens, vec![
            Token::BitOr, Token::BitAnd, Token::BitXor,
            Token::ShiftLeft, Token::ShiftRight, Token::BitNot,
        ]);
    }

    #[test]
    fn test_extended_memory() {
        let tokens = lex("@ * %").unwrap();
        assert_eq!(tokens, vec![
            Token::AbsoluteAddr, Token::Deref, Token::CellWidthCycle,
        ]);
    }

    #[test]
    fn test_stack_ops() {
        let tokens = lex("$ ~").unwrap();
        assert_eq!(tokens, vec![Token::Push, Token::Pop]);
    }

    #[test]
    fn test_syscall() {
        let tokens = lex("\\").unwrap();
        assert_eq!(tokens, vec![Token::Syscall]);
    }

    #[test]
    fn test_hex_escape() {
        let tokens = lex(r#""\x41\x42""#).unwrap();
        assert_eq!(tokens, vec![Token::StringLit(vec![0x41, 0x42])]);
    }

    #[test]
    fn test_tape_addr() {
        let tokens = lex("T").unwrap();
        assert_eq!(tokens, vec![Token::TapeAddr]);
    }

    #[test]
    fn test_framebuffer_flush() {
        let tokens = lex("F").unwrap();
        assert_eq!(tokens, vec![Token::FramebufferFlush]);
    }

    #[test]
    fn test_ffi_call() {
        let tokens = lex(r#"\ffi "libm.so.6" "ceil""#).unwrap();
        assert_eq!(tokens, vec![
            Token::FfiCall("libm.so.6".into(), "ceil".into()),
        ]);
    }
}
