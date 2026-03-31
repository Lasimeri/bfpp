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

    // Numeric literal — set current cell to an immediate value
    NumericLit(u64),   // #N or #0xHH — set cell to value N (respects cell width)

    // Multi-cell setup — set consecutive cells from P+0 in one shot
    MultiCell(Vec<u64>), // #{a, b, c} — set P+0=a, P+1=b, P+2=c; ptr stays at P+0

    // Direct cell width — set cell width without cycling
    SetCellWidth(u8),  // %1, %2, %4, %8 — set cell width directly

    // Conditional comparison — if current cell == N, execute block
    IfEqual,     // ?= — followed by #N { body } optionally : { else }
    IfNotEqual,  // ?! — followed by #N { body }
    IfLess,      // ?< — followed by #N { body }
    IfGreater,   // ?> — followed by #N { body }
    Colon,       // : — else separator in ?= #N { } : { }

    // Named variable declaration
    LetDecl(String, u64), // let name N — compile-time alias for tape position
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
            '%' => {
                chars.next(); col += 1;
                // Check for direct width: %1, %2, %4, %8
                match chars.peek() {
                    Some('1') => { chars.next(); col += 1; tokens.push(Token::SetCellWidth(1)); }
                    Some('2') => { chars.next(); col += 1; tokens.push(Token::SetCellWidth(2)); }
                    Some('4') => { chars.next(); col += 1; tokens.push(Token::SetCellWidth(4)); }
                    Some('8') => { chars.next(); col += 1; tokens.push(Token::SetCellWidth(8)); }
                    _ => { tokens.push(Token::CellWidthCycle); }
                }
            }

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
            '?' => {
                chars.next(); col += 1;
                // Check for conditional operators: ?= ?! ?< ?>
                match chars.peek() {
                    Some('=') => { chars.next(); col += 1; tokens.push(Token::IfEqual); }
                    Some('!') => { chars.next(); col += 1; tokens.push(Token::IfNotEqual); }
                    Some('<') => { chars.next(); col += 1; tokens.push(Token::IfLess); }
                    Some('>') => { chars.next(); col += 1; tokens.push(Token::IfGreater); }
                    _ => { tokens.push(Token::Propagate); }
                }
            }

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

            // # — numeric literal (#N), hex literal (#0xHH), or multi-cell setup (#{a,b,c})
            '#' => {
                chars.next(); col += 1;

                // Check for multi-cell setup: #{...}
                if chars.peek() == Some(&'{') {
                    chars.next(); col += 1; // consume {
                    let mut values = Vec::new();
                    loop {
                        // Skip whitespace
                        while chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
                            if *chars.peek().unwrap() == '\n' { line += 1; col = 1; }
                            chars.next(); col += 1;
                        }
                        if chars.peek() == Some(&'}') {
                            chars.next(); col += 1;
                            break;
                        }
                        // Parse a number (decimal or 0x hex)
                        let mut ns = String::new();
                        let hex = chars.peek() == Some(&'0') && {
                            let mut la = chars.clone(); la.next();
                            matches!(la.peek(), Some('x') | Some('X'))
                        };
                        if hex {
                            chars.next(); col += 1; // 0
                            chars.next(); col += 1; // x
                            while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                                ns.push(*chars.peek().unwrap());
                                chars.next(); col += 1;
                            }
                            values.push(u64::from_str_radix(&ns, 16).map_err(|_| LexError {
                                message: format!("Invalid hex in #{{...}}: 0x{}", ns), line, col,
                            })?);
                        } else {
                            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                                ns.push(*chars.peek().unwrap());
                                chars.next(); col += 1;
                            }
                            if ns.is_empty() {
                                return Err(LexError {
                                    message: "Expected number in #{...}".into(), line, col,
                                });
                            }
                            values.push(ns.parse::<u64>().map_err(|_| LexError {
                                message: format!("Invalid number in #{{...}}: {}", ns), line, col,
                            })?);
                        }
                        // Skip comma and whitespace
                        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
                            if chars.peek() == Some(&'\n') { line += 1; col = 1; }
                            chars.next(); col += 1;
                        }
                    }
                    tokens.push(Token::MultiCell(values));
                    continue;
                }

                let mut num_str = String::new();
                // Check for hex prefix 0x
                let is_hex = chars.peek() == Some(&'0') && {
                    let mut la = chars.clone();
                    la.next();
                    matches!(la.peek(), Some('x') | Some('X'))
                };
                if is_hex {
                    chars.next(); col += 1; // consume '0'
                    chars.next(); col += 1; // consume 'x'
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_hexdigit() {
                            num_str.push(c);
                            chars.next(); col += 1;
                        } else {
                            break;
                        }
                    }
                    if num_str.is_empty() {
                        return Err(LexError {
                            message: "Expected hex digits after '#0x'".into(),
                            line, col,
                        });
                    }
                    let val = u64::from_str_radix(&num_str, 16).map_err(|_| LexError {
                        message: format!("Invalid hex literal: #0x{}", num_str),
                        line, col,
                    })?;
                    tokens.push(Token::NumericLit(val));
                } else {
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_digit() {
                            num_str.push(c);
                            chars.next(); col += 1;
                        } else {
                            break;
                        }
                    }
                    if num_str.is_empty() {
                        return Err(LexError {
                            message: "Expected digits after '#'".into(),
                            line, col,
                        });
                    }
                    let val: u64 = num_str.parse().map_err(|_| LexError {
                        message: format!("Invalid numeric literal: #{}", num_str),
                        line, col,
                    })?;
                    tokens.push(Token::NumericLit(val));
                }
            }

            // Block comment: /* ... */ with nesting support
            '/' => {
                chars.next(); col += 1;
                if chars.peek() == Some(&'*') {
                    chars.next(); col += 1;
                    let mut depth = 1u32;
                    while depth > 0 {
                        match chars.next() {
                            Some('*') => {
                                col += 1;
                                if chars.peek() == Some(&'/') {
                                    chars.next(); col += 1;
                                    depth -= 1;
                                }
                            }
                            Some('/') => {
                                col += 1;
                                if chars.peek() == Some(&'*') {
                                    chars.next(); col += 1;
                                    depth += 1;
                                }
                            }
                            Some('\n') => { line += 1; col = 1; }
                            Some(_) => { col += 1; }
                            None => {
                                return Err(LexError {
                                    message: "Unterminated block comment /* ...".into(),
                                    line, col,
                                });
                            }
                        }
                    }
                }
                // Standalone '/' not followed by '*' is silently ignored
            }

            // Colon — else separator in conditional blocks: ?= #N { } : { }
            ':' => { chars.next(); col += 1; tokens.push(Token::Colon); }

            // 'l' — check for 'let' keyword (named variable declaration)
            'l' => {
                let mut la = chars.clone();
                la.next(); // l
                if la.peek() == Some(&'e') {
                    la.next(); // e
                    if la.peek() == Some(&'t') {
                        la.next(); // t
                        // Must be followed by whitespace (not part of a subroutine name)
                        if la.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next(); col += 1; // l
                            chars.next(); col += 1; // e
                            chars.next(); col += 1; // t
                            // Skip whitespace
                            while chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
                                chars.next(); col += 1;
                            }
                            // Parse name (alphanumeric + _)
                            let mut name = String::new();
                            while chars.peek().is_some_and(|c| c.is_alphanumeric() || *c == '_') {
                                name.push(*chars.peek().unwrap());
                                chars.next(); col += 1;
                            }
                            if name.is_empty() {
                                return Err(LexError {
                                    message: "Expected name after 'let'".into(), line, col,
                                });
                            }
                            // Skip whitespace
                            while chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
                                chars.next(); col += 1;
                            }
                            // Parse value (decimal or 0x hex)
                            let mut vs = String::new();
                            let hex = chars.peek() == Some(&'0') && {
                                let mut la2 = chars.clone(); la2.next();
                                matches!(la2.peek(), Some('x') | Some('X'))
                            };
                            let val = if hex {
                                chars.next(); col += 1; // 0
                                chars.next(); col += 1; // x
                                while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                                    vs.push(*chars.peek().unwrap());
                                    chars.next(); col += 1;
                                }
                                u64::from_str_radix(&vs, 16).map_err(|_| LexError {
                                    message: format!("Invalid hex in let: 0x{}", vs), line, col,
                                })?
                            } else {
                                while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                                    vs.push(*chars.peek().unwrap());
                                    chars.next(); col += 1;
                                }
                                if vs.is_empty() {
                                    return Err(LexError {
                                        message: "Expected value after 'let name'".into(), line, col,
                                    });
                                }
                                vs.parse::<u64>().map_err(|_| LexError {
                                    message: format!("Invalid number in let: {}", vs), line, col,
                                })?
                            };
                            tokens.push(Token::LetDecl(name, val));
                            continue;
                        }
                    }
                }
                // Not 'let' — fall through to ignored char
                chars.next(); col += 1;
            }

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

    #[test]
    fn test_numeric_literal_decimal() {
        let tokens = lex("#72").unwrap();
        assert_eq!(tokens, vec![Token::NumericLit(72)]);
    }

    #[test]
    fn test_numeric_literal_hex() {
        let tokens = lex("#0xFF").unwrap();
        assert_eq!(tokens, vec![Token::NumericLit(255)]);
    }

    #[test]
    fn test_numeric_literal_zero() {
        let tokens = lex("#0").unwrap();
        assert_eq!(tokens, vec![Token::NumericLit(0)]);
    }

    #[test]
    fn test_numeric_literal_large() {
        let tokens = lex("#36864").unwrap();
        assert_eq!(tokens, vec![Token::NumericLit(36864)]);
    }

    #[test]
    fn test_numeric_literal_error_empty() {
        assert!(lex("#abc").is_err());
    }

    #[test]
    fn test_direct_cell_width() {
        let tokens = lex("%8 %4 %2 %1").unwrap();
        assert_eq!(tokens, vec![
            Token::SetCellWidth(8),
            Token::SetCellWidth(4),
            Token::SetCellWidth(2),
            Token::SetCellWidth(1),
        ]);
    }

    #[test]
    fn test_bare_percent_still_cycles() {
        let tokens = lex("% %").unwrap();
        assert_eq!(tokens, vec![Token::CellWidthCycle, Token::CellWidthCycle]);
    }

    #[test]
    fn test_block_comment() {
        let tokens = lex("+ /* comment */ -").unwrap();
        assert_eq!(tokens, vec![Token::Increment, Token::Decrement]);
    }

    #[test]
    fn test_nested_block_comment() {
        let tokens = lex("+ /* outer /* inner */ still comment */ -").unwrap();
        assert_eq!(tokens, vec![Token::Increment, Token::Decrement]);
    }

    #[test]
    fn test_unterminated_block_comment() {
        assert!(lex("+ /* never closed").is_err());
    }

    #[test]
    fn test_standalone_r_error() {
        assert!(lex("R +").is_err());
    }

    #[test]
    fn test_standalone_k_error() {
        assert!(lex("K +").is_err());
    }

    #[test]
    fn test_multi_cell() {
        let tokens = lex("#{72, 101, 108}").unwrap();
        assert_eq!(tokens, vec![Token::MultiCell(vec![72, 101, 108])]);
    }

    #[test]
    fn test_multi_cell_hex() {
        let tokens = lex("#{0x48, 0x65}").unwrap();
        assert_eq!(tokens, vec![Token::MultiCell(vec![0x48, 0x65])]);
    }

    #[test]
    fn test_if_equal() {
        let tokens = lex("?= #17").unwrap();
        assert_eq!(tokens, vec![Token::IfEqual, Token::NumericLit(17)]);
    }

    #[test]
    fn test_if_not_equal() {
        let tokens = lex("?! #5").unwrap();
        assert_eq!(tokens, vec![Token::IfNotEqual, Token::NumericLit(5)]);
    }

    #[test]
    fn test_if_less_greater() {
        let tokens = lex("?< #32 ?> #126").unwrap();
        assert_eq!(tokens, vec![
            Token::IfLess, Token::NumericLit(32),
            Token::IfGreater, Token::NumericLit(126),
        ]);
    }

    #[test]
    fn test_colon_token() {
        let tokens = lex(": +").unwrap();
        assert_eq!(tokens, vec![Token::Colon, Token::Increment]);
    }

    #[test]
    fn test_let_declaration() {
        let tokens = lex("let cursor_x 50").unwrap();
        assert_eq!(tokens, vec![Token::LetDecl("cursor_x".into(), 50)]);
    }

    #[test]
    fn test_let_hex() {
        let tokens = lex("let addr 0x9000").unwrap();
        assert_eq!(tokens, vec![Token::LetDecl("addr".into(), 0x9000)]);
    }
}
