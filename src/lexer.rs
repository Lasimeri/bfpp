/// BF++ Lexer — tokenizes source into a stream of tokens.
///
/// The lexer operates as a single-pass character dispatcher: each iteration peeks at
/// the current character and dispatches to the appropriate token constructor. Multi-char
/// tokens (strings, subroutines, fd specs, FFI) consume additional characters inline
/// via helper functions. Unrecognized characters are silently ignored, which is
/// intentional — BF traditionally treats non-instruction chars as comments.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Core BF — the original 8 Brainfuck instructions
    MoveRight,
    MoveLeft,
    Increment,
    Decrement,
    Output,
    Input,
    LoopStart,
    LoopEnd,

    // Extended memory — pointer addressing and cell width control
    AbsoluteAddr,   // @ — jump pointer to address stored in current cell
    Deref,          // * — dereference: use current cell's value as a pointer
    CellWidthCycle, // % — cycle cell width (8→16→32 bit)

    // String literal — pre-parsed into raw bytes (escape sequences already resolved)
    StringLit(Vec<u8>),

    // Stack — push/pop current cell value to an auxiliary stack
    Push,  // $
    Pop,   // ~

    // Subroutines — definition and invocation
    SubDef(String),   // !#name{ — opens a named subroutine body
    SubCall(String),  // !#name (no trailing {) — invokes a previously defined subroutine
    BraceClose,       // } — closes a subroutine def, R{}, or K{} block
    Return,           // ^ — early return from subroutine

    // Syscall and fd-directed I/O
    Syscall,          // \ (standalone) — raw syscall
    OutputFd(FdSpec), // .{N} or .{*} — write to a specific or indirect fd
    InputFd(FdSpec),  // ,{N} or ,{*} — read from a specific or indirect fd

    // Bitwise — single-char operators on current cell
    BitOr,       // |
    BitAnd,      // &
    BitXor,      // x
    ShiftLeft,   // s
    ShiftRight,  // r
    BitNot,      // n

    // Error handling — result/catch pattern
    ErrorRead,   // E — read error register into current cell
    ErrorWrite,  // e — write current cell into error register
    Propagate,   // ? — propagate error (abort current block if error is set)
    ResultStart, // R{ — begin a result block (catches errors from body)
    CatchStart,  // K{ — begin a catch block (runs if preceding R{} errored)

    // Tape address & framebuffer
    TapeAddr,          // T — store current pointer address into cell
    FramebufferFlush,  // F — flush framebuffer to display

    // FFI — foreign function interface
    FfiCall(String, String), // \ffi "lib" "func" — call an external shared library function
}

// File descriptor specifier for fd-directed I/O (.{N} and ,{N} syntax).
// Literal holds a compile-time fd number; Indirect means "read fd from current cell at runtime."
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

/// Main lexer entry point. Consumes a source string and produces a flat token stream.
///
/// Design: single-pass, peek-based character dispatch. Each match arm handles one
/// token class. Multi-character tokens (strings, subroutines, fd specs, FFI) delegate
/// to dedicated parsers that advance the iterator. Unrecognized characters fall through
/// to the wildcard arm and are silently ignored — this preserves BF's convention that
/// non-instruction characters serve as inline comments.
pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    let mut line = 1usize;
    let mut col = 1usize;

    while let Some(&ch) = chars.peek() {
        match ch {
            // Comment — semicolon consumes everything to end of line (but not the newline itself)
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

            // . and , are overloaded: plain is standard I/O, followed by {N} is fd-directed.
            // Peek after consuming the char to decide which variant to emit.
            '.' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'{') {
                    let fd = parse_fd_spec(&mut chars, &mut col, line)?;
                    tokens.push(Token::OutputFd(fd));
                } else {
                    tokens.push(Token::Output);
                }
            }

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

            // Subroutine syntax: !#name{ (definition) or !#name (call).
            // The trailing { distinguishes defs from calls. The name can contain
            // BF operator chars (see is_sub_name_symbol), allowing names like "!#>>" —
            // this is intentional so subroutine names can be mnemonic for their operation.
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
                // Trailing { means definition; absence means call
                if chars.peek() == Some(&'{') {
                    chars.next(); col += 1;
                    tokens.push(Token::SubDef(name));
                } else {
                    tokens.push(Token::SubCall(name));
                }
            }

            // Brace close (ends subroutine def, R block, or K block)
            '}' => { chars.next(); col += 1; tokens.push(Token::BraceClose); }

            // Backslash: either a standalone syscall (\) or an FFI call (\ffi "lib" "func").
            // Uses lookahead cloning to check for "ffi" without consuming chars prematurely —
            // if the lookahead doesn't match, the backslash is emitted as a plain Syscall token.
            '\\' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'f') {
                    // Clone the iterator to peek 3 chars ahead without consuming
                    let mut lookahead = chars.clone();
                    lookahead.next(); // f
                    if lookahead.peek() == Some(&'f') {
                        lookahead.next(); // second f
                        if lookahead.peek() == Some(&'i') {
                            // Confirmed \ffi — now consume the chars for real
                            chars.next(); col += 1; // f
                            chars.next(); col += 1; // f
                            chars.next(); col += 1; // i
                            // Skip whitespace
                            while chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
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
                            while chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
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

            // R{ and K{ — error handling blocks. Unlike subroutines, R and K are not
            // standalone tokens — they MUST be followed by {. A bare R or K is a lex error,
            // not a no-op, to catch typos early.
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

            // Single-char alpha operators — these use lowercase/uppercase letters as mnemonics.
            // Case matters: E (read error) vs e (write error), etc.
            'x' => { chars.next(); col += 1; tokens.push(Token::BitXor); }
            's' => { chars.next(); col += 1; tokens.push(Token::ShiftLeft); }
            'r' => { chars.next(); col += 1; tokens.push(Token::ShiftRight); }
            'n' => { chars.next(); col += 1; tokens.push(Token::BitNot); }
            'E' => { chars.next(); col += 1; tokens.push(Token::ErrorRead); }
            'e' => { chars.next(); col += 1; tokens.push(Token::ErrorWrite); }
            'T' => { chars.next(); col += 1; tokens.push(Token::TapeAddr); }
            'F' => { chars.next(); col += 1; tokens.push(Token::FramebufferFlush); }

            // Wildcard: silently ignore whitespace, digits, and any unrecognized characters.
            // This is by design — BF convention treats everything non-instructional as a comment.
            _ => {
                chars.next();
                col += 1;
            }
        }
    }

    Ok(tokens)
}

// Parse {N} or {*} fd specifier after a . or , token.
// Two forms:
//   {N}  — literal fd number (e.g., {2} for stderr)
//   {*}  — indirect: fd number is read from the current cell at runtime
// Caller has already consumed the . or , and confirmed '{' is next.
fn parse_fd_spec(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    col: &mut usize,
    line: usize,
) -> Result<FdSpec, LexError> {
    // consume '{'
    chars.next(); *col += 1;

    // Check for indirect specifier {*}
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

    // Literal fd: accumulate digits until closing }
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

    // Parse accumulated digits as u32 — empty string (bare {}) fails here
    let fd: u32 = num_str.parse().map_err(|_| LexError {
        message: "Invalid fd number".into(),
        line, col: *col,
    })?;

    Ok(FdSpec::Literal(fd))
}

// Parse a string literal, consuming opening and closing quotes, and resolving
// escape sequences into raw bytes. Returns Vec<u8> (not String) because BF++
// string literals can contain arbitrary bytes via \x escapes and \0 null bytes.
//
// Supported escapes: \0 \n \r \t \\ \" \xHH
// Multi-line strings are allowed — embedded newlines update the line counter.
// Unknown escape sequences are a hard error (not silently passed through).
fn parse_string_literal(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    col: &mut usize,
    line: &mut usize,
) -> Result<Vec<u8>, LexError> {
    chars.next(); *col += 1; // consume opening "
    let start_line = *line;  // remember where the string started for error reporting

    let mut bytes = Vec::new();
    loop {
        match chars.next() {
            // EOF inside a string literal
            None => {
                return Err(LexError {
                    message: "Unterminated string literal".into(),
                    line: start_line, col: *col,
                });
            }
            // Unescaped closing quote — end of string
            Some('"') => {
                *col += 1;
                break;
            }
            // Escape sequence — dispatch on the character after the backslash
            Some('\\') => {
                *col += 1;
                match chars.next() {
                    Some('0') => { *col += 1; bytes.push(0); }
                    Some('n') => { *col += 1; bytes.push(b'\n'); }
                    Some('r') => { *col += 1; bytes.push(b'\r'); }
                    Some('t') => { *col += 1; bytes.push(b'\t'); }
                    Some('\\') => { *col += 1; bytes.push(b'\\'); }
                    Some('"') => { *col += 1; bytes.push(b'"'); }
                    // Hex escape: \xHH — exactly two hex digits required
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
            // Bare newline inside string — allowed (multi-line string), update position
            Some('\n') => {
                bytes.push(b'\n');
                *line += 1;
                *col = 1;
            }
            // Normal character — encode as UTF-8 bytes (handles multibyte chars correctly)
            Some(c) => {
                *col += 1;
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                bytes.extend_from_slice(encoded.as_bytes());
            }
        }
    }

    Ok(bytes)
}

// Consume subroutine name characters after the !# prefix.
// Name terminates at the first character that isn't alphanumeric or a recognized
// BF++ operator symbol. This allows mnemonic names like "!#add", "!#>>", or "!#mul/div".
// The terminator (typically { or whitespace) is NOT consumed.
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

// Characters allowed in subroutine names beyond alphanumerics.
// Includes BF operators and common separator chars (_, /) so names can
// embed operational hints. Notably excludes {, }, [, ], !, #, ", and ;
// which have structural meaning in the grammar.
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
