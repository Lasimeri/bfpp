// BF++ error code constants and errno mapping.

pub const OK: u64 = 0;
pub const ERR_GENERIC: u64 = 1;
pub const ERR_NOT_FOUND: u64 = 2;
pub const ERR_PERMISSION: u64 = 3;
pub const ERR_OOM: u64 = 4;
pub const ERR_CONN_REFUSED: u64 = 5;
pub const ERR_INVALID_ARG: u64 = 6;
pub const ERR_TIMEOUT: u64 = 7;
pub const ERR_EXISTS: u64 = 8;
pub const ERR_BUSY: u64 = 9;
pub const ERR_PIPE: u64 = 10;
pub const ERR_CONN_RESET: u64 = 11;
pub const ERR_ADDR_IN_USE: u64 = 12;
pub const ERR_NOT_CONNECTED: u64 = 13;
pub const ERR_INTERRUPTED: u64 = 14;
pub const ERR_IO: u64 = 15;
pub const ERR_NOLIB: u64 = 16;
pub const ERR_NOSYM: u64 = 17;

/// Returns the C source for the errno-to-bfpp mapping function.
pub fn errno_mapping_c_source() -> &'static str {
    r#"
int bfpp_errno_to_code(int sys_errno) {
    switch (sys_errno) {
        case 0:            return 0;
        case ENOENT:       return 2;
        case EACCES:       return 3;
        case EROFS:        return 3;
        case ENOMEM:       return 4;
        case ECONNREFUSED: return 5;
        case EINVAL:       return 6;
        case EBADF:        return 6;
        case ETIMEDOUT:    return 7;
        case EEXIST:       return 8;
        case EBUSY:        return 9;
        case EAGAIN:       return 9;
        case EPIPE:        return 10;
        case ECONNRESET:   return 11;
        case EADDRINUSE:   return 12;
        case ENOTCONN:     return 13;
        case EINTR:        return 14;
        case EIO:          return 15;
        default:           return 1;
    }
}
"#
}
