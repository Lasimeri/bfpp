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

    // Dual-tape (multicore) — inter-tape communication
    ReadTape,          // { (standalone) — read from read-tape into current cell
    ReadPtrRight,      // ( — advance read-tape pointer right
    ReadPtrLeft,       // ) — advance read-tape pointer left
    Transfer,          // P — bulk transfer: copy cell to write-tape, advance both ptrs
    SwapTapes,         // Q — swap read-tape and write-tape roles
    SyncPtrs,          // V — synchronize read/write tape pointers to main ptr position

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

    // If/else on cell truthiness — destructive (zeroes cell after test)
    IfElseStart, // ?{ — begins a ?{true_body}:{false_body} block
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
                // Check for conditional operators: ?= ?! ?< ?> ?{
                match chars.peek() {
                    Some('=') => { chars.next(); col += 1; tokens.push(Token::IfEqual); }
                    Some('!') => { chars.next(); col += 1; tokens.push(Token::IfNotEqual); }
                    Some('<') => { chars.next(); col += 1; tokens.push(Token::IfLess); }
                    Some('>') => { chars.next(); col += 1; tokens.push(Token::IfGreater); }
                    // ?{ — if/else on cell truthiness (destructive: zeroes cell after test).
                    // Consumes the { so the parser sees IfElseStart and parses body until }.
                    Some('{') => { chars.next(); col += 1; tokens.push(Token::IfElseStart); }
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

            // Dual-tape operators — inter-tape communication for multicore support.
            // `{` is safe here: SubDef consumes `{` after `!#name`, R/K consume `{` after
            // `R`/`K`, `.{`/`,{` consume `{` in the fd-spec path, `#{` consumes `{` for
            // multi-cell. A standalone `{` (not preceded by any of those) is ReadTape.
            // `}` is NOT handled here — it stays as BraceClose, and the parser emits
            // WriteTape when BraceClose appears outside a block context.
            '{' => { chars.next(); col += 1; tokens.push(Token::ReadTape); }
            '(' => { chars.next(); col += 1; tokens.push(Token::ReadPtrRight); }
            ')' => { chars.next(); col += 1; tokens.push(Token::ReadPtrLeft); }
            'P' => { chars.next(); col += 1; tokens.push(Token::Transfer); }
            'Q' => { chars.next(); col += 1; tokens.push(Token::SwapTapes); }
            'V' => { chars.next(); col += 1; tokens.push(Token::SyncPtrs); }

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

// ---------------------------------------------------------------------------
// Compact token encoding — opt-in serialization for caching or IPC.
//
// The primary token type (Token enum) is used throughout the compiler pipeline.
// This compact encoding is an *optional* serialization format that reduces
// memory/wire footprint when tokens need to be stored or transferred.
//
// Format: 1-byte kind discriminant + varint-encoded value + optional name.
// Tokens without payload (core BF ops, stack ops, etc.) encode as a single byte.
// ---------------------------------------------------------------------------

/// Compact serialized representation of a single token.
/// Kind byte maps 1:1 to Token enum variants. Value holds numeric payloads
/// (NumericLit, SetCellWidth, LetDecl value, fd number). Name holds string
/// payloads (SubDef, SubCall, FfiCall lib+func, LetDecl name).
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub struct CompactToken {
    pub kind: u8,
    pub value: u64,
    pub name: Option<String>,
    /// Second string field — used only by FfiCall (func name) and LetDecl (name).
    pub name2: Option<String>,
    /// Byte payload — used only by StringLit and MultiCell.
    pub bytes: Option<Vec<u8>>,
}

// Kind discriminant constants — one per Token variant.
// Grouped by category to match the Token enum's layout.
#[allow(dead_code)]
mod kind {
    // Core BF (0–7)
    pub const MOVE_RIGHT: u8    = 0;
    pub const MOVE_LEFT: u8     = 1;
    pub const INCREMENT: u8     = 2;
    pub const DECREMENT: u8     = 3;
    pub const OUTPUT: u8        = 4;
    pub const INPUT: u8         = 5;
    pub const LOOP_START: u8    = 6;
    pub const LOOP_END: u8      = 7;
    // Extended memory (8–10)
    pub const ABSOLUTE_ADDR: u8 = 8;
    pub const DEREF: u8         = 9;
    pub const CELL_WIDTH_CYCLE: u8 = 10;
    // String (11)
    pub const STRING_LIT: u8    = 11;
    // Stack (12–13)
    pub const PUSH: u8          = 12;
    pub const POP: u8           = 13;
    // Subroutines (14–17)
    pub const SUB_DEF: u8       = 14;
    pub const SUB_CALL: u8      = 15;
    pub const BRACE_CLOSE: u8   = 16;
    pub const RETURN: u8        = 17;
    // Syscall / fd I/O (18–20)
    pub const SYSCALL: u8       = 18;
    pub const OUTPUT_FD: u8     = 19;
    pub const INPUT_FD: u8      = 20;
    // Bitwise (21–26)
    pub const BIT_OR: u8        = 21;
    pub const BIT_AND: u8       = 22;
    pub const BIT_XOR: u8       = 23;
    pub const SHIFT_LEFT: u8    = 24;
    pub const SHIFT_RIGHT: u8   = 25;
    pub const BIT_NOT: u8       = 26;
    // Error handling (27–31)
    pub const ERROR_READ: u8    = 27;
    pub const ERROR_WRITE: u8   = 28;
    pub const PROPAGATE: u8     = 29;
    pub const RESULT_START: u8  = 30;
    pub const CATCH_START: u8   = 31;
    // Tape / framebuffer (32–33)
    pub const TAPE_ADDR: u8     = 32;
    pub const FB_FLUSH: u8      = 33;
    // Dual-tape (34–39)
    pub const READ_TAPE: u8     = 34;
    pub const READ_PTR_RIGHT: u8 = 35;
    pub const READ_PTR_LEFT: u8  = 36;
    pub const TRANSFER: u8      = 37;
    pub const SWAP_TAPES: u8    = 38;
    pub const SYNC_PTRS: u8     = 39;
    // FFI (40)
    pub const FFI_CALL: u8      = 40;
    // Numeric (41)
    pub const NUMERIC_LIT: u8   = 41;
    // Multi-cell (42)
    pub const MULTI_CELL: u8    = 42;
    // Direct cell width (43)
    pub const SET_CELL_WIDTH: u8 = 43;
    // Conditionals (44–48)
    pub const IF_EQUAL: u8      = 44;
    pub const IF_NOT_EQUAL: u8  = 45;
    pub const IF_LESS: u8       = 46;
    pub const IF_GREATER: u8    = 47;
    pub const COLON: u8         = 48;
    // Let (49)
    pub const LET_DECL: u8      = 49;
    // If/else (50)
    pub const IF_ELSE_START: u8 = 50;
    // Fd variants for indirect (high bit of kind byte)
    pub const OUTPUT_FD_INDIRECT: u8 = 51;
    pub const INPUT_FD_INDIRECT: u8  = 52;
}

#[allow(dead_code)]
impl CompactToken {
    /// Convert a Token to its compact representation.
    pub fn from_token(t: &Token) -> Self {
        let empty = CompactToken { kind: 0, value: 0, name: None, name2: None, bytes: None };
        match t {
            Token::MoveRight      => CompactToken { kind: kind::MOVE_RIGHT, ..empty },
            Token::MoveLeft       => CompactToken { kind: kind::MOVE_LEFT, ..empty },
            Token::Increment      => CompactToken { kind: kind::INCREMENT, ..empty },
            Token::Decrement      => CompactToken { kind: kind::DECREMENT, ..empty },
            Token::Output         => CompactToken { kind: kind::OUTPUT, ..empty },
            Token::Input          => CompactToken { kind: kind::INPUT, ..empty },
            Token::LoopStart      => CompactToken { kind: kind::LOOP_START, ..empty },
            Token::LoopEnd        => CompactToken { kind: kind::LOOP_END, ..empty },
            Token::AbsoluteAddr   => CompactToken { kind: kind::ABSOLUTE_ADDR, ..empty },
            Token::Deref          => CompactToken { kind: kind::DEREF, ..empty },
            Token::CellWidthCycle => CompactToken { kind: kind::CELL_WIDTH_CYCLE, ..empty },
            Token::StringLit(b)   => CompactToken { kind: kind::STRING_LIT, value: 0, name: None, name2: None, bytes: Some(b.clone()) },
            Token::Push           => CompactToken { kind: kind::PUSH, ..empty },
            Token::Pop            => CompactToken { kind: kind::POP, ..empty },
            Token::SubDef(n)      => CompactToken { kind: kind::SUB_DEF, value: 0, name: Some(n.clone()), name2: None, bytes: None },
            Token::SubCall(n)     => CompactToken { kind: kind::SUB_CALL, value: 0, name: Some(n.clone()), name2: None, bytes: None },
            Token::BraceClose     => CompactToken { kind: kind::BRACE_CLOSE, ..empty },
            Token::Return         => CompactToken { kind: kind::RETURN, ..empty },
            Token::Syscall        => CompactToken { kind: kind::SYSCALL, ..empty },
            Token::OutputFd(FdSpec::Literal(fd)) => CompactToken { kind: kind::OUTPUT_FD, value: *fd as u64, name: None, name2: None, bytes: None },
            Token::OutputFd(FdSpec::Indirect)    => CompactToken { kind: kind::OUTPUT_FD_INDIRECT, ..empty },
            Token::InputFd(FdSpec::Literal(fd))  => CompactToken { kind: kind::INPUT_FD, value: *fd as u64, name: None, name2: None, bytes: None },
            Token::InputFd(FdSpec::Indirect)     => CompactToken { kind: kind::INPUT_FD_INDIRECT, ..empty },
            Token::BitOr          => CompactToken { kind: kind::BIT_OR, ..empty },
            Token::BitAnd         => CompactToken { kind: kind::BIT_AND, ..empty },
            Token::BitXor         => CompactToken { kind: kind::BIT_XOR, ..empty },
            Token::ShiftLeft      => CompactToken { kind: kind::SHIFT_LEFT, ..empty },
            Token::ShiftRight     => CompactToken { kind: kind::SHIFT_RIGHT, ..empty },
            Token::BitNot         => CompactToken { kind: kind::BIT_NOT, ..empty },
            Token::ErrorRead      => CompactToken { kind: kind::ERROR_READ, ..empty },
            Token::ErrorWrite     => CompactToken { kind: kind::ERROR_WRITE, ..empty },
            Token::Propagate      => CompactToken { kind: kind::PROPAGATE, ..empty },
            Token::ResultStart    => CompactToken { kind: kind::RESULT_START, ..empty },
            Token::CatchStart     => CompactToken { kind: kind::CATCH_START, ..empty },
            Token::TapeAddr       => CompactToken { kind: kind::TAPE_ADDR, ..empty },
            Token::FramebufferFlush => CompactToken { kind: kind::FB_FLUSH, ..empty },
            Token::ReadTape       => CompactToken { kind: kind::READ_TAPE, ..empty },
            Token::ReadPtrRight   => CompactToken { kind: kind::READ_PTR_RIGHT, ..empty },
            Token::ReadPtrLeft    => CompactToken { kind: kind::READ_PTR_LEFT, ..empty },
            Token::Transfer       => CompactToken { kind: kind::TRANSFER, ..empty },
            Token::SwapTapes      => CompactToken { kind: kind::SWAP_TAPES, ..empty },
            Token::SyncPtrs       => CompactToken { kind: kind::SYNC_PTRS, ..empty },
            Token::FfiCall(lib, func) => CompactToken { kind: kind::FFI_CALL, value: 0, name: Some(lib.clone()), name2: Some(func.clone()), bytes: None },
            Token::NumericLit(n)  => CompactToken { kind: kind::NUMERIC_LIT, value: *n, name: None, name2: None, bytes: None },
            Token::MultiCell(vs)  => {
                // Encode the values as a byte sequence: count as first varint, then each value
                let mut buf = Vec::new();
                encode_varint(vs.len() as u64, &mut buf);
                for v in vs {
                    encode_varint(*v, &mut buf);
                }
                CompactToken { kind: kind::MULTI_CELL, value: 0, name: None, name2: None, bytes: Some(buf) }
            }
            Token::SetCellWidth(w) => CompactToken { kind: kind::SET_CELL_WIDTH, value: *w as u64, name: None, name2: None, bytes: None },
            Token::IfEqual        => CompactToken { kind: kind::IF_EQUAL, ..empty },
            Token::IfNotEqual     => CompactToken { kind: kind::IF_NOT_EQUAL, ..empty },
            Token::IfLess         => CompactToken { kind: kind::IF_LESS, ..empty },
            Token::IfGreater      => CompactToken { kind: kind::IF_GREATER, ..empty },
            Token::Colon          => CompactToken { kind: kind::COLON, ..empty },
            Token::LetDecl(name, val) => CompactToken { kind: kind::LET_DECL, value: *val, name: Some(name.clone()), name2: None, bytes: None },
            Token::IfElseStart    => CompactToken { kind: kind::IF_ELSE_START, ..empty },
        }
    }

    /// Convert back to a Token. Round-trips with `from_token`.
    pub fn to_token(&self) -> Option<Token> {
        Some(match self.kind {
            kind::MOVE_RIGHT      => Token::MoveRight,
            kind::MOVE_LEFT       => Token::MoveLeft,
            kind::INCREMENT       => Token::Increment,
            kind::DECREMENT       => Token::Decrement,
            kind::OUTPUT          => Token::Output,
            kind::INPUT           => Token::Input,
            kind::LOOP_START      => Token::LoopStart,
            kind::LOOP_END        => Token::LoopEnd,
            kind::ABSOLUTE_ADDR   => Token::AbsoluteAddr,
            kind::DEREF           => Token::Deref,
            kind::CELL_WIDTH_CYCLE => Token::CellWidthCycle,
            kind::STRING_LIT      => Token::StringLit(self.bytes.clone().unwrap_or_default()),
            kind::PUSH            => Token::Push,
            kind::POP             => Token::Pop,
            kind::SUB_DEF         => Token::SubDef(self.name.clone()?),
            kind::SUB_CALL        => Token::SubCall(self.name.clone()?),
            kind::BRACE_CLOSE     => Token::BraceClose,
            kind::RETURN          => Token::Return,
            kind::SYSCALL         => Token::Syscall,
            kind::OUTPUT_FD       => Token::OutputFd(FdSpec::Literal(self.value as u32)),
            kind::OUTPUT_FD_INDIRECT => Token::OutputFd(FdSpec::Indirect),
            kind::INPUT_FD        => Token::InputFd(FdSpec::Literal(self.value as u32)),
            kind::INPUT_FD_INDIRECT => Token::InputFd(FdSpec::Indirect),
            kind::BIT_OR          => Token::BitOr,
            kind::BIT_AND         => Token::BitAnd,
            kind::BIT_XOR         => Token::BitXor,
            kind::SHIFT_LEFT      => Token::ShiftLeft,
            kind::SHIFT_RIGHT     => Token::ShiftRight,
            kind::BIT_NOT         => Token::BitNot,
            kind::ERROR_READ      => Token::ErrorRead,
            kind::ERROR_WRITE     => Token::ErrorWrite,
            kind::PROPAGATE       => Token::Propagate,
            kind::RESULT_START    => Token::ResultStart,
            kind::CATCH_START     => Token::CatchStart,
            kind::TAPE_ADDR       => Token::TapeAddr,
            kind::FB_FLUSH        => Token::FramebufferFlush,
            kind::READ_TAPE       => Token::ReadTape,
            kind::READ_PTR_RIGHT  => Token::ReadPtrRight,
            kind::READ_PTR_LEFT   => Token::ReadPtrLeft,
            kind::TRANSFER        => Token::Transfer,
            kind::SWAP_TAPES      => Token::SwapTapes,
            kind::SYNC_PTRS       => Token::SyncPtrs,
            kind::FFI_CALL        => Token::FfiCall(self.name.clone()?, self.name2.clone()?),
            kind::NUMERIC_LIT     => Token::NumericLit(self.value),
            kind::MULTI_CELL      => {
                let buf = self.bytes.as_ref()?;
                let mut pos = 0;
                let count = decode_varint(buf, &mut pos) as usize;
                let mut vals = Vec::with_capacity(count);
                for _ in 0..count {
                    vals.push(decode_varint(buf, &mut pos));
                }
                Token::MultiCell(vals)
            }
            kind::SET_CELL_WIDTH  => Token::SetCellWidth(self.value as u8),
            kind::IF_EQUAL        => Token::IfEqual,
            kind::IF_NOT_EQUAL    => Token::IfNotEqual,
            kind::IF_LESS         => Token::IfLess,
            kind::IF_GREATER      => Token::IfGreater,
            kind::COLON           => Token::Colon,
            kind::LET_DECL        => Token::LetDecl(self.name.clone()?, self.value),
            kind::IF_ELSE_START   => Token::IfElseStart,
            _ => return None,
        })
    }
}

/// Encode a u64 as a variable-length integer (LEB128-style).
/// Each byte stores 7 data bits; the high bit signals continuation.
#[allow(dead_code)]
pub fn encode_varint(mut n: u64, buf: &mut Vec<u8>) {
    while n >= 0x80 {
        buf.push((n as u8) | 0x80);
        n >>= 7;
    }
    buf.push(n as u8);
}

/// Decode a variable-length integer from a byte buffer at position `pos`.
/// Advances `pos` past the consumed bytes.
#[allow(dead_code)]
pub fn decode_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        result |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 { break; }
        shift += 7;
    }
    result
}

/// Serialize a token stream to a compact byte representation.
/// Format per token: kind (1 byte) + varint value + length-prefixed strings/bytes.
#[allow(dead_code)]
pub fn encode_tokens(tokens: &[Token]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(tokens.len() * 2);
    encode_varint(tokens.len() as u64, &mut buf);
    for t in tokens {
        let ct = CompactToken::from_token(t);
        buf.push(ct.kind);
        encode_varint(ct.value, &mut buf);
        // Encode optional name
        if let Some(ref name) = ct.name {
            encode_varint(name.len() as u64, &mut buf);
            buf.extend_from_slice(name.as_bytes());
        } else {
            buf.push(0); // zero-length name = absent
        }
        // Encode optional name2
        if let Some(ref name2) = ct.name2 {
            encode_varint(name2.len() as u64, &mut buf);
            buf.extend_from_slice(name2.as_bytes());
        } else {
            buf.push(0);
        }
        // Encode optional bytes
        if let Some(ref bytes) = ct.bytes {
            encode_varint(bytes.len() as u64, &mut buf);
            buf.extend_from_slice(bytes);
        } else {
            buf.push(0);
        }
    }
    buf
}

/// Deserialize a compact byte representation back into a token stream.
#[allow(dead_code)]
pub fn decode_tokens(buf: &[u8]) -> Option<Vec<Token>> {
    let mut pos = 0;
    let count = decode_varint(buf, &mut pos) as usize;
    let mut tokens = Vec::with_capacity(count);
    for _ in 0..count {
        if pos >= buf.len() { return None; }
        let k = buf[pos]; pos += 1;
        let value = decode_varint(buf, &mut pos);
        // Decode name
        let name_len = decode_varint(buf, &mut pos) as usize;
        let name = if name_len > 0 {
            let s = std::str::from_utf8(&buf[pos..pos + name_len]).ok()?;
            pos += name_len;
            Some(s.to_string())
        } else {
            None
        };
        // Decode name2
        let name2_len = decode_varint(buf, &mut pos) as usize;
        let name2 = if name2_len > 0 {
            let s = std::str::from_utf8(&buf[pos..pos + name2_len]).ok()?;
            pos += name2_len;
            Some(s.to_string())
        } else {
            None
        };
        // Decode bytes
        let bytes_len = decode_varint(buf, &mut pos) as usize;
        let bytes = if bytes_len > 0 {
            let b = buf[pos..pos + bytes_len].to_vec();
            pos += bytes_len;
            Some(b)
        } else {
            None
        };
        let ct = CompactToken { kind: k, value, name, name2, bytes };
        tokens.push(ct.to_token()?);
    }
    Some(tokens)
}

// ---------------------------------------------------------------------------
// Token subsequence deduplication (minimal version).
//
// Detects duplicate subroutine bodies in the token stream. This is a lightweight
// analysis pass, not a transform — it identifies duplicates and returns the info
// without modifying the token stream (that's the parser/optimizer's job).
//
// Assessment: Full sliding-window dedup over arbitrary token subsequences has
// O(n*w) complexity for marginal gain. Subroutine body dedup is the only case
// where token-level dedup is both tractable and useful — identical !#sub{...}
// bodies are a common pattern when macros expand the same template multiple times.
// ---------------------------------------------------------------------------

/// Identify subroutine definitions with identical token bodies.
/// Returns a map from body signature (hash of token kinds) to the list of
/// subroutine names sharing that body. Only entries with 2+ names are interesting.
#[allow(dead_code)]
pub fn find_duplicate_sub_bodies(tokens: &[Token]) -> std::collections::HashMap<u64, Vec<String>> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut bodies: std::collections::HashMap<u64, Vec<String>> = std::collections::HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Token::SubDef(ref name) = tokens[i] {
            // Collect body tokens until matching BraceClose (nesting-aware)
            let mut depth = 1u32;
            let mut j = i + 1;
            let body_start = j;
            while j < tokens.len() && depth > 0 {
                match &tokens[j] {
                    Token::SubDef(_) | Token::ResultStart | Token::CatchStart | Token::IfElseStart => depth += 1,
                    Token::BraceClose => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let body_end = if depth == 0 { j - 1 } else { j }; // exclude closing brace
            // Hash the body token kinds + values for a signature
            let mut hasher = DefaultHasher::new();
            for t in &tokens[body_start..body_end] {
                // Use debug repr as a simple deterministic hash input
                format!("{:?}", t).hash(&mut hasher);
            }
            let sig = hasher.finish();
            bodies.entry(sig).or_default().push(name.clone());
            i = j;
        } else {
            i += 1;
        }
    }
    // Keep only groups with duplicates
    bodies.retain(|_, names| names.len() > 1);
    bodies
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

    // --- Compact token encoding round-trip tests ---

    #[test]
    fn test_compact_roundtrip_core_ops() {
        let tokens = lex("><+-.,[]").unwrap();
        let encoded = encode_tokens(&tokens);
        let decoded = decode_tokens(&encoded).unwrap();
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_compact_roundtrip_complex() {
        let source = r#"!#pr{[.>]^} !#pr .{2} ,{*} #0xFF #{72, 101} "AB\n" let x 42 ?= #5 R{ E } K{ e } $ ~ @ * % %8 | & x s r n T F { ( ) P Q V : ?{ \  "#;
        let tokens = lex(source).unwrap();
        let encoded = encode_tokens(&tokens);
        let decoded = decode_tokens(&encoded).unwrap();
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_compact_roundtrip_ffi() {
        let tokens = lex(r#"\ffi "libm.so.6" "ceil""#).unwrap();
        let encoded = encode_tokens(&tokens);
        let decoded = decode_tokens(&encoded).unwrap();
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_varint_roundtrip() {
        for &val in &[0u64, 1, 127, 128, 255, 16383, 16384, u64::MAX] {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            let mut pos = 0;
            let decoded = decode_varint(&buf, &mut pos);
            assert_eq!(val, decoded, "varint roundtrip failed for {}", val);
            assert_eq!(pos, buf.len(), "varint didn't consume all bytes for {}", val);
        }
    }

    #[test]
    fn test_find_duplicate_sub_bodies() {
        // Two subs with identical bodies
        let tokens = lex("!#a{ + + - } !#b{ + + - } !#c{ + - }").unwrap();
        let dupes = find_duplicate_sub_bodies(&tokens);
        // a and b should be grouped; c is different
        let mut found_ab = false;
        for (_sig, names) in &dupes {
            if names.contains(&"a".to_string()) && names.contains(&"b".to_string()) {
                found_ab = true;
                assert_eq!(names.len(), 2);
            }
        }
        assert!(found_ab, "Expected a and b to be identified as duplicates");
    }

    #[test]
    fn test_find_duplicate_sub_bodies_no_dupes() {
        let tokens = lex("!#a{ + } !#b{ - }").unwrap();
        let dupes = find_duplicate_sub_bodies(&tokens);
        assert!(dupes.is_empty());
    }
}
