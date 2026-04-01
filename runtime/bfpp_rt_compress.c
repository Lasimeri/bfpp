/*
 * bfpp_rt_compress.c -- Compressed I/O and tape checkpoint runtime
 *
 * Implements:
 *   - Item 6: bfpp_net_send_compressed / bfpp_net_recv_compressed
 *             bfpp_file_write_compressed / bfpp_file_read_compressed
 *   - Item 7: bfpp_tape_save / bfpp_tape_load
 *
 * Requires zlib (-lz). Entire file is guarded by HAVE_ZLIB.
 * Compile with: gcc -DHAVE_ZLIB -c bfpp_rt_compress.c -lz
 *
 * Wire format for compressed data:
 *   Network: [compressed_len:4 (network byte order)][compressed_data]
 *   File:    [original_size:4 (LE)][compressed_size:4 (LE)][compressed_data]
 *
 * Tape checkpoint format:
 *   [original_size:4 (LE)][compressed_size:4 (LE)][compressed_data]
 *   Trailing zero bytes are stripped before compression.
 *
 * Link with: -lz
 */

#ifdef HAVE_ZLIB

#include "bfpp_rt_compress.h"
#include <zlib.h>
#include <stdlib.h>
#include <stdio.h>
#include <unistd.h>
#include <arpa/inet.h>  /* htonl, ntohl */

/* ── Helpers ──────────────────────────────────────────────────── */

/* Write exactly `len` bytes to fd, handling partial writes. */
static int write_all(int fd, const void *buf, size_t len)
{
    const uint8_t *p = (const uint8_t *)buf;
    size_t remaining = len;
    while (remaining > 0) {
        ssize_t n = write(fd, p, remaining);
        if (n <= 0) return -1;
        p += n;
        remaining -= (size_t)n;
    }
    return 0;
}

/* Read exactly `len` bytes from fd. Returns 0 on success, -1 on error/EOF. */
static int read_all(int fd, void *buf, size_t len)
{
    uint8_t *p = (uint8_t *)buf;
    size_t remaining = len;
    while (remaining > 0) {
        ssize_t n = read(fd, p, remaining);
        if (n <= 0) return -1;
        p += n;
        remaining -= (size_t)n;
    }
    return 0;
}

/* ── Item 6: Compressed Network I/O ──────────────────────────── */

int bfpp_net_send_compressed(int fd, const uint8_t *data, int len)
{
    uLongf compressed_len = compressBound((uLong)len);
    uint8_t *compressed = (uint8_t *)malloc(compressed_len);
    if (!compressed) return -1;

    if (compress2(compressed, &compressed_len, data, (uLong)len,
                  Z_DEFAULT_COMPRESSION) != Z_OK) {
        free(compressed);
        return -1;
    }

    /* Send: [compressed_len:4 NBO][compressed_data] */
    uint32_t header = htonl((uint32_t)compressed_len);
    if (write_all(fd, &header, 4) != 0) {
        free(compressed);
        return -1;
    }
    if (write_all(fd, compressed, compressed_len) != 0) {
        free(compressed);
        return -1;
    }

    free(compressed);
    return 0;
}

int bfpp_net_recv_compressed(int fd, uint8_t *dst, int max_len)
{
    uint32_t header;
    if (read_all(fd, &header, 4) != 0) return -1;
    uint32_t compressed_len = ntohl(header);

    if (compressed_len > (uint32_t)(max_len * 2)) return -1; /* sanity */

    uint8_t *compressed = (uint8_t *)malloc(compressed_len);
    if (!compressed) return -1;

    if (read_all(fd, compressed, compressed_len) != 0) {
        free(compressed);
        return -1;
    }

    uLongf actual = (uLongf)max_len;
    if (uncompress(dst, &actual, compressed, compressed_len) != Z_OK) {
        free(compressed);
        return -1;
    }

    free(compressed);
    return (int)actual;
}

/* ── Item 6: Compressed File I/O ─────────────────────────────── */

int bfpp_file_write_compressed(int fd, const uint8_t *data, int len)
{
    uLongf compressed_len = compressBound((uLong)len);
    uint8_t *compressed = (uint8_t *)malloc(compressed_len);
    if (!compressed) return -1;

    if (compress2(compressed, &compressed_len, data, (uLong)len,
                  Z_DEFAULT_COMPRESSION) != Z_OK) {
        free(compressed);
        return -1;
    }

    /* Header: [original_size:4 LE][compressed_size:4 LE] */
    uint32_t hdr[2];
    hdr[0] = (uint32_t)len;
    hdr[1] = (uint32_t)compressed_len;

    if (write_all(fd, hdr, 8) != 0) {
        free(compressed);
        return -1;
    }
    if (write_all(fd, compressed, compressed_len) != 0) {
        free(compressed);
        return -1;
    }

    free(compressed);
    return 0;
}

int bfpp_file_read_compressed(int fd, uint8_t *dst, int max_len)
{
    uint32_t hdr[2];
    if (read_all(fd, hdr, 8) != 0) return -1;

    uint32_t original_size = hdr[0];
    uint32_t compressed_size = hdr[1];

    if ((int)original_size > max_len) return -1;

    uint8_t *compressed = (uint8_t *)malloc(compressed_size);
    if (!compressed) return -1;

    if (read_all(fd, compressed, compressed_size) != 0) {
        free(compressed);
        return -1;
    }

    uLongf actual = (uLongf)original_size;
    if (uncompress(dst, &actual, compressed, compressed_size) != Z_OK) {
        free(compressed);
        return -1;
    }

    free(compressed);
    return (int)actual;
}

/* ── Item 7: Tape Checkpoint Compression ─────────────────────── */

int bfpp_tape_save(const char *path, const uint8_t *tape, int size)
{
    /* Find actual used region — skip trailing zeros */
    int actual_size = size;
    while (actual_size > 0 && tape[actual_size - 1] == 0) actual_size--;

    if (actual_size == 0) {
        /* Empty tape — write a minimal header */
        FILE *f = fopen(path, "wb");
        if (!f) return -1;
        uint32_t hdr[2] = { 0, 0 };
        fwrite(hdr, 4, 2, f);
        fclose(f);
        return 0;
    }

    uLongf clen = compressBound((uLong)actual_size);
    uint8_t *compressed = (uint8_t *)malloc(clen);
    if (!compressed) return -1;

    if (compress2(compressed, &clen, tape, (uLong)actual_size,
                  Z_BEST_COMPRESSION) != Z_OK) {
        free(compressed);
        return -1;
    }

    FILE *f = fopen(path, "wb");
    if (!f) {
        free(compressed);
        return -1;
    }

    uint32_t hdr[2];
    hdr[0] = (uint32_t)actual_size;
    hdr[1] = (uint32_t)clen;
    fwrite(hdr, 4, 2, f);
    fwrite(compressed, 1, clen, f);
    fclose(f);

    free(compressed);
    return 0;
}

int bfpp_tape_load(const char *path, uint8_t *tape, int max_size)
{
    FILE *f = fopen(path, "rb");
    if (!f) return -1;

    uint32_t hdr[2];
    if (fread(hdr, 4, 2, f) != 2) {
        fclose(f);
        return -1;
    }

    uint32_t original_size = hdr[0];
    uint32_t compressed_size = hdr[1];

    /* Empty checkpoint */
    if (original_size == 0 && compressed_size == 0) {
        fclose(f);
        return 0;
    }

    if ((int)original_size > max_size) {
        fclose(f);
        return -1;
    }

    uint8_t *compressed = (uint8_t *)malloc(compressed_size);
    if (!compressed) {
        fclose(f);
        return -1;
    }

    if (fread(compressed, 1, compressed_size, f) != compressed_size) {
        free(compressed);
        fclose(f);
        return -1;
    }
    fclose(f);

    /* Zero the tape before loading — checkpoint only stores non-zero prefix */
    memset(tape, 0, (size_t)max_size);

    uLongf actual = (uLongf)original_size;
    if (uncompress(tape, &actual, compressed, compressed_size) != Z_OK) {
        free(compressed);
        return -1;
    }

    free(compressed);
    return (int)actual;
}

#endif /* HAVE_ZLIB */
