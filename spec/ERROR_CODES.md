# BF++ Error Codes

**Version**: 0.3.0

---

## Error Register

The error register (`bfpp_err`) is a 64-bit integer separate from the tape. Value 0 means no error.

---

## Error Code Table

| BF++ Code | Name | Description | C errno Mapping |
|-----------|------|-------------|-----------------|
| 0 | `OK` | No error | — |
| 1 | `ERR_GENERIC` | Unspecified error | `EPERM`, unmapped errors |
| 2 | `ERR_NOT_FOUND` | File or resource not found | `ENOENT` |
| 3 | `ERR_PERMISSION` | Permission denied | `EACCES`, `EROFS` |
| 4 | `ERR_OOM` | Out of memory / resource exhaustion | `ENOMEM`, stack overflow |
| 5 | `ERR_CONN_REFUSED` | Connection refused | `ECONNREFUSED` |
| 6 | `ERR_INVALID_ARG` | Invalid argument or operation | `EINVAL`, `EBADF`, stack underflow |
| 7 | `ERR_TIMEOUT` | Operation timed out | `ETIMEDOUT` |
| 8 | `ERR_EXISTS` | File/resource already exists | `EEXIST` |
| 9 | `ERR_BUSY` | Resource busy | `EBUSY`, `EAGAIN`, `EWOULDBLOCK` |
| 10 | `ERR_PIPE` | Broken pipe | `EPIPE` |
| 11 | `ERR_CONN_RESET` | Connection reset | `ECONNRESET` |
| 12 | `ERR_ADDR_IN_USE` | Address already in use | `EADDRINUSE` |
| 13 | `ERR_NOT_CONNECTED` | Not connected | `ENOTCONN` |
| 14 | `ERR_INTERRUPTED` | Interrupted | `EINTR` |
| 15 | `ERR_IO` | I/O error | `EIO` |
| 16–255 | — | Reserved for future standard use | — |
| 256+ | — | User-defined error codes | — |

---

## errno → BF++ Mapping Table (C Implementation)

```c
int bfpp_errno_to_code(int sys_errno) {
    switch (sys_errno) {
        case 0:            return 0;  // OK
        case ENOENT:       return 2;  // ERR_NOT_FOUND
        case EACCES:       return 3;  // ERR_PERMISSION
        case EROFS:        return 3;  // ERR_PERMISSION
        case ENOMEM:       return 4;  // ERR_OOM
        case ECONNREFUSED: return 5;  // ERR_CONN_REFUSED
        case EINVAL:       return 6;  // ERR_INVALID_ARG
        case EBADF:        return 6;  // ERR_INVALID_ARG
        case ETIMEDOUT:    return 7;  // ERR_TIMEOUT
        case EEXIST:       return 8;  // ERR_EXISTS
        case EBUSY:        return 9;  // ERR_BUSY
        case EAGAIN:       return 9;  // ERR_BUSY
        case EPIPE:        return 10; // ERR_PIPE
        case ECONNRESET:   return 11; // ERR_CONN_RESET
        case EADDRINUSE:   return 12; // ERR_ADDR_IN_USE
        case ENOTCONN:     return 13; // ERR_NOT_CONNECTED
        case EINTR:        return 14; // ERR_INTERRUPTED
        case EIO:          return 15; // ERR_IO
        default:           return 1;  // ERR_GENERIC
    }
}
```

---

## Usage Patterns

### Setting an error
```
++++++ e    ; set error register to 6 (ERR_INVALID_ARG)
```

### Reading an error
```
E           ; copy error register into current cell
```

### Propagating an error
```
!#some_sub ?  ; call subroutine, propagate error if any
```

### Handling an error locally
```
R{
  !#risky_op
}K{
  E           ; load error code
  ; handle...
}
```

---

## Design Rationale

- Error codes are intentionally small integers (fit in 8 bits for basic use) for BF compatibility
- The mapping from errno covers the most common POSIX errors
- User-defined codes start at 256 to avoid collision with future standard codes
- The `?` operator provides zero-overhead error propagation in the success path (transpiles to a single branch)
