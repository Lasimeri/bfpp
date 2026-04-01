/*
 * bfpp_rt_compress.h -- Data structure compression and compact encodings for BF++
 *
 * Provides:
 *   - Length-prefixed hashmap key comparison (Item 1)
 *   - Gap buffer for O(1) amortized array insert/remove (Item 2)
 *   - Length-prefixed string operations (2-byte prefix, max 65535) (Item 3)
 *   - Varint-encoded value stack (tagged encoding) (Item 4)
 *   - Packed call stack bitfield (Item 5)
 *
 * Items 1-4 require codegen.rs changes to emit calls to these functions
 * instead of the current inline C. See CODEGEN_CHANGES at bottom of file.
 *
 * Items 6-7 (compressed I/O, tape checkpoints) are in bfpp_rt_compress.c
 * and require zlib linkage.
 */

#ifndef BFPP_RT_COMPRESS_H
#define BFPP_RT_COMPRESS_H

#include <stdint.h>
#include <string.h>

/* ── Item 1: Length-Prefixed Hashmap Key Comparison ────────────── */

/*
 * Key layout: [len:1][data:len]
 * len = number of data bytes (0-255). No null terminator needed.
 * Hash is computed over the data bytes only (not the length prefix).
 *
 * Currently the codegen emits inline hashmap code with null-terminated
 * key scans. To use these, codegen must be updated to:
 *   1. Store keys as [len][data...] instead of [data...\0]
 *   2. Replace null-terminated comparison loops with bfpp_key_eq()
 *   3. Replace null-terminated hashing with bfpp_key_hash()
 */

static inline int bfpp_key_eq(const uint8_t *a, const uint8_t *b) {
    if (a[0] != b[0]) return 0;         /* length mismatch: O(1) reject */
    return memcmp(a + 1, b + 1, a[0]) == 0;
}

/* djb2 hash over length-prefixed key data */
static inline uint32_t bfpp_key_hash(const uint8_t *key) {
    uint32_t h = 5381;
    uint8_t len = key[0];
    for (uint8_t i = 0; i < len; i++) {
        h = h * 33 + key[1 + i];
    }
    return h;
}

/* Encode a null-terminated string into length-prefixed format.
 * dst must have room for 1 + strlen(src) bytes.
 * Returns total encoded length (1 + data_len). */
static inline int bfpp_key_encode(uint8_t *dst, const uint8_t *src) {
    int len = 0;
    while (src[len] != 0 && len < 255) len++;
    dst[0] = (uint8_t)len;
    memcpy(dst + 1, src, len);
    return 1 + len;
}

/* ── Item 2: Gap Buffer for Array Operations ──────────────────── */

/*
 * Replaces memmove-based __array_insert / __array_remove with a gap
 * buffer that provides O(1) amortized insert/remove at any position
 * (O(gap_distance) for repositioning the gap, amortized O(1) for
 * sequential inserts at the same position).
 *
 * The gap buffer operates on the BF++ tape: data is stored in-place
 * on tape[], with the gap tracked by external metadata.
 *
 * CODEGEN CHANGE REQUIRED: codegen must emit bfpp_gap_*() calls
 * instead of inline memmove for __array_insert / __array_remove.
 */

typedef struct {
    uint8_t *data;      /* pointer into tape (NOT separately allocated) */
    int capacity;       /* total allocated size in bytes */
    int gap_start;      /* byte offset where gap begins */
    int gap_end;        /* byte offset where gap ends (gap size = gap_end - gap_start) */
} bfpp_gap_buffer_t;

/* Initialize a gap buffer over a tape region.
 * data = &tape[array_addr], capacity = allocated region size.
 * gap starts at the end (no gap = contiguous data). */
static inline void bfpp_gap_init(bfpp_gap_buffer_t *gb, uint8_t *data, int capacity) {
    gb->data = data;
    gb->capacity = capacity;
    gb->gap_start = capacity;
    gb->gap_end = capacity;
}

/* Initialize with existing data of `used` bytes. Gap is after the data. */
static inline void bfpp_gap_init_with_data(bfpp_gap_buffer_t *gb, uint8_t *data, int capacity, int used) {
    gb->data = data;
    gb->capacity = capacity;
    gb->gap_start = used;
    gb->gap_end = capacity;
}

static inline void bfpp_gap_move_to(bfpp_gap_buffer_t *gb, int pos) {
    if (pos < gb->gap_start) {
        int n = gb->gap_start - pos;
        memmove(gb->data + gb->gap_end - n, gb->data + pos, n);
        gb->gap_start -= n;
        gb->gap_end -= n;
    } else if (pos > gb->gap_start) {
        int n = pos - gb->gap_start;
        memmove(gb->data + gb->gap_start, gb->data + gb->gap_end, n);
        gb->gap_start += n;
        gb->gap_end += n;
    }
}

/* Insert `esz` bytes from `src` at logical position `pos` (byte offset).
 * Returns 0 on success, -1 if gap is too small. */
static inline int bfpp_gap_insert(bfpp_gap_buffer_t *gb, int pos, const uint8_t *src, int esz) {
    int gap_size = gb->gap_end - gb->gap_start;
    if (gap_size < esz) return -1;     /* no room */
    bfpp_gap_move_to(gb, pos);
    memcpy(gb->data + gb->gap_start, src, esz);
    gb->gap_start += esz;
    return 0;
}

/* Remove `esz` bytes at logical position `pos` (byte offset).
 * Removed bytes become part of the gap. */
static inline void bfpp_gap_remove(bfpp_gap_buffer_t *gb, int pos, int esz) {
    bfpp_gap_move_to(gb, pos);
    gb->gap_end += esz;
}

/* Get the logical size (total bytes minus gap). */
static inline int bfpp_gap_logical_size(const bfpp_gap_buffer_t *gb) {
    return gb->capacity - (gb->gap_end - gb->gap_start);
}

/* Read a byte at logical position `pos`. Translates across the gap. */
static inline uint8_t bfpp_gap_get(const bfpp_gap_buffer_t *gb, int pos) {
    if (pos < gb->gap_start) return gb->data[pos];
    return gb->data[pos + (gb->gap_end - gb->gap_start)];
}

/* Flatten the gap buffer: move gap to end so data is contiguous.
 * Call this before returning data to tape-reading code that expects
 * a flat byte array. */
static inline void bfpp_gap_flatten(bfpp_gap_buffer_t *gb) {
    bfpp_gap_move_to(gb, bfpp_gap_logical_size(gb));
}

/* ── Item 3: Length-Prefixed Strings ──────────────────────────── */

/*
 * String layout: [len_lo:1][len_hi:1][char0][char1]...
 * Length is stored as 2-byte little-endian uint16_t. Max: 65535 bytes.
 * No null terminator (length is authoritative).
 *
 * CODEGEN CHANGE REQUIRED: codegen must emit bfpp_lps_*() calls
 * for __strlen, __strcpy, __strcmp and update string storage format.
 *
 * Migration: existing null-terminated tape strings need conversion.
 * bfpp_lps_from_cstr() converts in-place or to a destination.
 */

static inline uint16_t bfpp_lps_len(const uint8_t *str) {
    return (uint16_t)(str[0] | (str[1] << 8));
}

static inline void bfpp_lps_set_len(uint8_t *str, uint16_t len) {
    str[0] = (uint8_t)(len & 0xFF);
    str[1] = (uint8_t)((len >> 8) & 0xFF);
}

/* Copy a length-prefixed string (prefix + data). */
static inline void bfpp_lps_copy(uint8_t *dst, const uint8_t *src) {
    uint16_t len = bfpp_lps_len(src);
    memcpy(dst, src, (size_t)len + 2);
}

/* Compare two length-prefixed strings. Returns 0 if equal,
 * <0 if a < b, >0 if a > b (lexicographic). */
static inline int bfpp_lps_cmp(const uint8_t *a, const uint8_t *b) {
    uint16_t alen = bfpp_lps_len(a);
    uint16_t blen = bfpp_lps_len(b);
    uint16_t min_len = alen < blen ? alen : blen;
    int r = memcmp(a + 2, b + 2, min_len);
    if (r != 0) return r;
    return (int)alen - (int)blen;
}

/* Encode a C string (null-terminated) into length-prefixed format.
 * dst must have room for 2 + strlen(src) bytes.
 * Returns total size (2 + len). */
static inline int bfpp_lps_from_cstr(uint8_t *dst, const char *src) {
    int len = 0;
    while (src[len] != '\0' && len < 65535) len++;
    bfpp_lps_set_len(dst, (uint16_t)len);
    memcpy(dst + 2, src, len);
    return 2 + len;
}

/* ── Item 4: Varint-Encoded Value Stack ──────────────────────── */

/*
 * Tagged encoding for the value stack. Most BF++ programs push small
 * values (0-127). This encoding stores them in 1 byte instead of 8.
 *
 * Tag byte (first byte of each entry, stored in FORWARD order):
 *   0x00-0x7F : literal value (1 byte total, no payload)
 *   0x80      : 2-byte little-endian uint16_t follows (3 bytes total)
 *   0x81      : 4-byte little-endian uint32_t follows (5 bytes total)
 *   0x82      : 8-byte little-endian uint64_t follows (9 bytes total)
 *
 * Forward-only stack: entries are pushed with a tag prefix and an
 * entry-count index tracks entry boundaries for O(1) pop.
 *
 * CODEGEN CHANGE REQUIRED: codegen must emit bfpp_vpush/bfpp_vpop
 * calls instead of direct stack[sp++]/stack[--sp].
 */

#ifndef BFPP_VSTACK_SIZE
#define BFPP_VSTACK_SIZE (65536 * 9)   /* worst case: 9 bytes per entry */
#endif
#ifndef BFPP_VSTACK_MAX_ENTRIES
#define BFPP_VSTACK_MAX_ENTRIES 65536
#endif

typedef struct {
    uint8_t data[BFPP_VSTACK_SIZE];
    int offsets[BFPP_VSTACK_MAX_ENTRIES]; /* byte offset of each entry in data[] */
    int top;           /* byte offset past last written byte in data[] */
    int count;         /* number of entries */
} bfpp_vstack_t;

static inline void bfpp_vstack_init(bfpp_vstack_t *vs) {
    vs->top = 0;
    vs->count = 0;
}

/* Returns 0 on success, -1 on overflow. */
static inline int bfpp_vstack_push(bfpp_vstack_t *vs, uint64_t value) {
    if (vs->count >= BFPP_VSTACK_MAX_ENTRIES) return -1;
    int pos = vs->top;
    if (value <= 0x7F) {
        if (pos + 1 > BFPP_VSTACK_SIZE) return -1;
        vs->data[pos] = (uint8_t)value;
        vs->offsets[vs->count] = pos;
        vs->top = pos + 1;
    } else if (value <= 0xFFFF) {
        if (pos + 3 > BFPP_VSTACK_SIZE) return -1;
        vs->data[pos] = 0x80;
        vs->data[pos + 1] = (uint8_t)(value & 0xFF);
        vs->data[pos + 2] = (uint8_t)((value >> 8) & 0xFF);
        vs->offsets[vs->count] = pos;
        vs->top = pos + 3;
    } else if (value <= 0xFFFFFFFFULL) {
        if (pos + 5 > BFPP_VSTACK_SIZE) return -1;
        vs->data[pos] = 0x81;
        memcpy(vs->data + pos + 1, &value, 4);
        vs->offsets[vs->count] = pos;
        vs->top = pos + 5;
    } else {
        if (pos + 9 > BFPP_VSTACK_SIZE) return -1;
        vs->data[pos] = 0x82;
        memcpy(vs->data + pos + 1, &value, 8);
        vs->offsets[vs->count] = pos;
        vs->top = pos + 9;
    }
    vs->count++;
    return 0;
}

/* Returns the popped value. Sets *ok = 0 on underflow, 1 on success. */
static inline uint64_t bfpp_vstack_pop(bfpp_vstack_t *vs, int *ok) {
    if (vs->count <= 0) { *ok = 0; return 0; }
    *ok = 1;
    vs->count--;
    int pos = vs->offsets[vs->count];
    uint8_t tag = vs->data[pos];
    vs->top = pos;    /* reclaim space */

    if (tag <= 0x7F) {
        return tag;
    } else if (tag == 0x80) {
        return (uint64_t)vs->data[pos + 1] |
               ((uint64_t)vs->data[pos + 2] << 8);
    } else if (tag == 0x81) {
        uint32_t v;
        memcpy(&v, vs->data + pos + 1, 4);
        return v;
    } else {  /* 0x82 */
        uint64_t v;
        memcpy(&v, vs->data + pos + 1, 8);
        return v;
    }
}

static inline int bfpp_vstack_count(const bfpp_vstack_t *vs) {
    return vs->count;
}

/* ── Item 5: Packed Call Stack Bitfield ───────────────────────── */

/*
 * The current BF++ runtime does NOT store a call stack with return
 * addresses + error state per frame. It uses a simple depth counter
 * (bfpp_call_depth) because return addresses are implicit in C's
 * call stack (subroutines are C functions, so `return` handles it).
 *
 * This packed frame type is provided for future use if BF++ moves
 * to an explicit call stack (e.g., for computed goto dispatch or
 * interpreter mode where return addresses must be stored manually).
 *
 * 4 bytes per frame instead of 8 (if the old layout were two ints).
 */

typedef struct {
    uint32_t return_addr : 24;    /* up to 16M addresses (program counter) */
    uint32_t error_state : 8;     /* 256 error codes */
} bfpp_call_frame_t;              /* 4 bytes total */

#define BFPP_MAX_CALL_FRAMES 256

typedef struct {
    bfpp_call_frame_t frames[BFPP_MAX_CALL_FRAMES];
    int depth;
} bfpp_call_stack_t;

static inline void bfpp_cstack_init(bfpp_call_stack_t *cs) {
    cs->depth = 0;
}

static inline int bfpp_cstack_push(bfpp_call_stack_t *cs, uint32_t addr, uint8_t err) {
    if (cs->depth >= BFPP_MAX_CALL_FRAMES) return -1;
    cs->frames[cs->depth].return_addr = addr & 0xFFFFFF;
    cs->frames[cs->depth].error_state = err;
    cs->depth++;
    return 0;
}

static inline int bfpp_cstack_pop(bfpp_call_stack_t *cs, uint32_t *addr, uint8_t *err) {
    if (cs->depth <= 0) return -1;
    cs->depth--;
    if (addr) *addr = cs->frames[cs->depth].return_addr;
    if (err) *err = cs->frames[cs->depth].error_state;
    return 0;
}

/* ── Compressed I/O and Tape Checkpoint (Items 6-7) ──────────── */

/* These are implemented in bfpp_rt_compress.c, guarded by HAVE_ZLIB. */

#ifdef HAVE_ZLIB

/* Item 6: Send compressed data over a socket fd. Returns 0 on success. */
int bfpp_net_send_compressed(int fd, const uint8_t *data, int len);

/* Item 6: Receive compressed data from a socket fd.
 * Reads a 4-byte compressed-length header, then decompresses.
 * dst must be at least max_len bytes. Returns decompressed size, or -1. */
int bfpp_net_recv_compressed(int fd, uint8_t *dst, int max_len);

/* Item 6: Write compressed data to a file fd.
 * Header: [original_size:4][compressed_size:4][compressed_data] */
int bfpp_file_write_compressed(int fd, const uint8_t *data, int len);

/* Item 6: Read compressed data from a file fd.
 * Reads header, decompresses. Returns original size, or -1. */
int bfpp_file_read_compressed(int fd, uint8_t *dst, int max_len);

/* Item 7: Save tape region to file (skip trailing zeros, compress). */
int bfpp_tape_save(const char *path, const uint8_t *tape, int size);

/* Item 7: Load tape checkpoint from file. Returns loaded byte count, or -1. */
int bfpp_tape_load(const char *path, uint8_t *tape, int max_size);

#endif /* HAVE_ZLIB */

#endif /* BFPP_RT_COMPRESS_H */

/*
 * ══════════════════════════════════════════════════════════════════
 * CODEGEN_CHANGES — Required modifications to src/codegen.rs
 * ══════════════════════════════════════════════════════════════════
 *
 * The codegen currently emits all hashmap, array, string, and stack
 * operations as inline C code in the generated output. To use the
 * compressed/optimized implementations in this header, codegen.rs
 * must be modified to:
 *
 * 1. Add `#include "bfpp_rt_compress.h"` to the generated C preamble
 *    (near the existing #include <string.h> at line ~355 of codegen.rs).
 *
 * 2. HASHMAP (Item 1) — Lines ~1666-1681 of codegen.rs:
 *    - __hashmap_init: zero the entry area the same way, but store keys
 *      as [len][data] not [data\0].
 *    - __hashmap_get: replace the null-terminated key scan loop:
 *        OLD:  while (tape[_ka] && tape[_ki]) { if (...) ... }
 *        NEW:  if (bfpp_key_eq(&tape[_s + 4], &tape[_kaddr_encoded]))
 *      The input key at _kaddr must first be encoded via bfpp_key_encode()
 *      or the tape format must switch to length-prefixed at the source.
 *    - __hashmap_set: same comparison change, plus store key as
 *      [len][data] instead of [data\0] on insertion.
 *    - Hash computation: replace `for (_i = _kaddr; tape[_i]; _i++)`
 *      with bfpp_key_hash(&tape[_kaddr]).
 *
 * 3. ARRAY OPS (Item 2) — Lines ~1641-1651 of codegen.rs:
 *    - __array_insert: instead of inline memmove, the codegen should
 *      emit code that initializes a bfpp_gap_buffer_t over the array
 *      region and calls bfpp_gap_insert(). For programs that do many
 *      sequential inserts at similar positions, this amortizes to O(1).
 *    - __array_remove: similarly, use bfpp_gap_remove() + bfpp_gap_flatten().
 *    - NOTE: the gap buffer metadata (gap_start, gap_end) needs to
 *      persist between calls. Two options:
 *      (a) Store gap metadata on tape next to the array.
 *      (b) Maintain a static gap buffer cache in the runtime.
 *      Option (a) changes the array layout on tape; option (b) is
 *      simpler but limited to one active gap buffer at a time.
 *
 * 4. STRINGS (Item 3) — Lines ~1626-1636 of codegen.rs:
 *    - __strlen: emit `bfpp_set(ptr, bfpp_lps_len(&tape[_a]));`
 *    - __strcpy: emit `bfpp_lps_copy(&tape[_d], &tape[_s]);`
 *    - __strcmp: emit comparison using bfpp_lps_cmp()
 *    - All intrinsics that store/read strings on tape must switch to
 *      the [len_lo][len_hi][data] format. This is a breaking change
 *      for existing BF++ programs that assume null-terminated strings.
 *    - Consider providing both modes via a compiler flag.
 *
 * 5. VALUE STACK (Item 4) — Lines ~584-591 of codegen.rs:
 *    - Replace `uint64_t stack[STACK_SIZE]` with `bfpp_vstack_t vstack;`
 *    - Replace `bfpp_push(val)` with `bfpp_vstack_push(&vstack, val)`
 *    - Replace `bfpp_pop()` with `bfpp_vstack_pop(&vstack, &ok)`
 *    - The sp variable becomes vstack.count.
 *    - Error handling: check return values and set bfpp_err accordingly.
 *
 * 6. CALL STACK (Item 5):
 *    - The current runtime uses only bfpp_call_depth (a plain int counter).
 *      There is NO call frame array with return addresses — C's native
 *      call stack handles returns via `return` statements.
 *    - The packed bfpp_call_frame_t is provided for FUTURE use if BF++
 *      moves to an explicit dispatch table or interpreter loop where
 *      return addresses must be stored manually.
 *    - No codegen change needed for the current architecture.
 *
 * 7. COMPRESSED I/O (Item 6):
 *    - New intrinsics (__net_send_compressed, __file_write_compressed, etc.)
 *      need to be added to the intrinsic dispatch in codegen.rs.
 *    - Add a `compressed_io` flag to the IntrinsicsUsed struct.
 *    - Generated code needs: `#include "bfpp_rt_compress.h"` and `-lz` link flag.
 *
 * 8. TAPE CHECKPOINT (Item 7):
 *    - New intrinsics (__tape_save, __tape_load) need codegen support.
 *    - Input layout: null-terminated file path at tape[ptr], save region
 *      size at tape[ptr+4] (or use full TAPE_SIZE for load).
 *
 * ══════════════════════════════════════════════════════════════════
 */
