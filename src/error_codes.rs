// BF++ error code constants and errno mapping.
//
// These define the BF++ error code space — a compact, platform-independent
// set of error values that BF++ programs observe via `E` (ErrorRead).
// The Rust constants here are the single source of truth; codegen.rs
// emits matching C #defines (BFPP_ERR_*) into every generated program,
// keeping the Rust compiler and the C runtime in sync without duplication.

// Success / no error.
pub const OK: u64 = 0;

// Fallback for any errno that doesn't have a specific mapping.
pub const ERR_GENERIC: u64 = 1;

// File/path not found (ENOENT).
pub const ERR_NOT_FOUND: u64 = 2;

// Permission denied (EACCES, EROFS).
pub const ERR_PERMISSION: u64 = 3;

// Out of memory (ENOMEM). Also used for stack overflow in the BF++ runtime.
pub const ERR_OOM: u64 = 4;

// Connection refused (ECONNREFUSED).
pub const ERR_CONN_REFUSED: u64 = 5;

// Invalid argument (EINVAL, EBADF). Also set by the cell-width system
// when accessing a continuation byte or when bfpp_cycle_width can't widen.
pub const ERR_INVALID_ARG: u64 = 6;

// Operation timed out (ETIMEDOUT).
pub const ERR_TIMEOUT: u64 = 7;

// Resource already exists (EEXIST).
pub const ERR_EXISTS: u64 = 8;

// Device or resource busy (EBUSY, EAGAIN).
pub const ERR_BUSY: u64 = 9;

// Broken pipe (EPIPE).
pub const ERR_PIPE: u64 = 10;

// Connection reset by peer (ECONNRESET).
pub const ERR_CONN_RESET: u64 = 11;

// Address already in use (EADDRINUSE).
pub const ERR_ADDR_IN_USE: u64 = 12;

// Socket not connected (ENOTCONN).
pub const ERR_NOT_CONNECTED: u64 = 13;

// Interrupted system call (EINTR).
pub const ERR_INTERRUPTED: u64 = 14;

// General I/O error (EIO).
pub const ERR_IO: u64 = 15;

// FFI: shared library could not be loaded (dlopen failed).
pub const ERR_NOLIB: u64 = 16;

// FFI: symbol not found in loaded library (dlsym failed).
pub const ERR_NOSYM: u64 = 17;

/// Returns the C source for the errno-to-bfpp mapping function.
///
/// This is emitted verbatim into the generated C program's header section.
/// It translates POSIX errno values into the BF++ error code space so that
/// syscall failures surface as values a BF++ program can inspect with `E`.
///
/// Coverage: maps the ~17 most common POSIX errnos. Some many-to-one
/// collapses exist (e.g. EACCES and EROFS both map to ERR_PERMISSION,
/// EBUSY and EAGAIN both map to ERR_BUSY). Any unmapped errno falls
/// through to ERR_GENERIC (1) — programs that need the raw errno can
/// use the syscall interface directly.
pub fn errno_mapping_c_source() -> &'static str {
    r#"
int bfpp_errno_to_code(int sys_errno) {
    switch (sys_errno) {
        case 0:            return 0;
        case ENOENT:       return 2;  /* ERR_NOT_FOUND */
        case EACCES:       return 3;  /* ERR_PERMISSION */
        case EROFS:        return 3;  /* ERR_PERMISSION (read-only FS treated as permission) */
        case ENOMEM:       return 4;  /* ERR_OOM */
        case ECONNREFUSED: return 5;  /* ERR_CONN_REFUSED */
        case EINVAL:       return 6;  /* ERR_INVALID_ARG */
        case EBADF:        return 6;  /* ERR_INVALID_ARG (bad fd is an invalid-arg variant) */
        case ETIMEDOUT:    return 7;  /* ERR_TIMEOUT */
        case EEXIST:       return 8;  /* ERR_EXISTS */
        case EBUSY:        return 9;  /* ERR_BUSY */
        case EAGAIN:       return 9;  /* ERR_BUSY (would-block collapsed into busy) */
        case EPIPE:        return 10; /* ERR_PIPE */
        case ECONNRESET:   return 11; /* ERR_CONN_RESET */
        case EADDRINUSE:   return 12; /* ERR_ADDR_IN_USE */
        case ENOTCONN:     return 13; /* ERR_NOT_CONNECTED */
        case EINTR:        return 14; /* ERR_INTERRUPTED */
        case EIO:          return 15; /* ERR_IO */
        default:           return 1;  /* ERR_GENERIC — unmapped errno */
    }
}
"#
}
