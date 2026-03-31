#include "bfpp_rt.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#include <unistd.h>
#include <termios.h>
#include <sys/ioctl.h>
#include <poll.h>

/* ── Cell type: one character position in the terminal ─────────── */

/* Each cell holds a single Unicode codepoint encoded as up to 4 UTF-8
   bytes.  len=0 means empty (treated as space). For ASCII, len=1 and
   only utf8[0] is used.  The double-buffer diff compares all fields. */
typedef struct {
    uint8_t utf8[4];
    uint8_t len;      /* 1-4 for valid content, 0 = blank/space */
    int16_t fg;       /* -1 = default */
    int16_t bg;       /* -1 = default */
} Cell;

/* ── Static state ──────────────────────────────────────────────── */

static struct termios orig_termios;
static int raw_mode       = 0;
static int tui_initialized = 0;
static int term_cols      = 80;
static int term_rows      = 24;
static Cell *front_buf    = NULL;
static Cell *back_buf     = NULL;

/* Track current terminal color state to avoid redundant SGR sequences.
   -2 = unknown/uninitialized, forcing first cell to always emit colors. */
static int last_fg = -2;
static int last_bg = -2;

/* Output buffer for batching ANSI writes before flush */
#define OUT_BUF_SIZE 8192
static char out_buf[OUT_BUF_SIZE];
static int  out_len = 0;

/* ── Output buffer helpers ─────────────────────────────────────── */

static void flush_out(void)
{
    if (out_len > 0) {
        write(STDOUT_FILENO, out_buf, (size_t)out_len);
        out_len = 0;
    }
}

static void out_raw(const char *data, int len)
{
    while (len > 0) {
        int avail = OUT_BUF_SIZE - out_len;
        if (avail <= 0) {
            flush_out();
            avail = OUT_BUF_SIZE;
        }
        int chunk = len < avail ? len : avail;
        memcpy(out_buf + out_len, data, (size_t)chunk);
        out_len += chunk;
        data += chunk;
        len -= chunk;
    }
}

static void out_str(const char *s)
{
    out_raw(s, (int)strlen(s));
}

static void out_fmt(const char *fmt, ...)
{
    char tmp[256];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(tmp, sizeof(tmp), fmt, ap);
    va_end(ap);
    if (n > 0) {
        out_raw(tmp, n < (int)sizeof(tmp) ? n : (int)sizeof(tmp) - 1);
    }
}

/* ── Terminal raw mode ─────────────────────────────────────────── */

static void enter_raw_mode(void)
{
    if (raw_mode) return;
    tcgetattr(STDIN_FILENO, &orig_termios);

    struct termios raw = orig_termios;
    /* Input: disable break signal, CR→NL, parity, strip, flow control */
    raw.c_iflag &= ~(unsigned)(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
    /* Output: disable post-processing */
    raw.c_oflag &= ~(unsigned)(OPOST);
    /* Control: 8-bit chars */
    raw.c_cflag |= (unsigned)(CS8);
    /* Local: disable echo, canonical mode, signals, extended input */
    raw.c_lflag &= ~(unsigned)(ECHO | ICANON | ISIG | IEXTEN);
    /* Immediate character reads */
    raw.c_cc[VMIN]  = 1;
    raw.c_cc[VTIME] = 0;

    tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw);
    raw_mode = 1;
}

static void exit_raw_mode(void)
{
    if (!raw_mode) return;
    tcsetattr(STDIN_FILENO, TCSAFLUSH, &orig_termios);
    raw_mode = 0;
}

/* ── Buffer allocation ─────────────────────────────────────────── */

static Cell make_blank_cell(void)
{
    Cell c;
    c.utf8[0] = ' ';
    c.utf8[1] = 0;
    c.utf8[2] = 0;
    c.utf8[3] = 0;
    c.len = 1;
    c.fg = -1;
    c.bg = -1;
    return c;
}

/* Determine the byte length of a UTF-8 codepoint from its lead byte */
static int utf8_char_len(uint8_t lead)
{
    if (lead < 0x80) return 1;
    if ((lead & 0xE0) == 0xC0) return 2;
    if ((lead & 0xF0) == 0xE0) return 3;
    if ((lead & 0xF8) == 0xF0) return 4;
    return 1; /* invalid byte, treat as single */
}

/* Compare two cells for equality */
static int cell_eq(const Cell *a, const Cell *b)
{
    if (a->len != b->len || a->fg != b->fg || a->bg != b->bg) return 0;
    return memcmp(a->utf8, b->utf8, a->len) == 0;
}

static void alloc_buffers(void)
{
    size_t count = (size_t)term_cols * (size_t)term_rows;
    front_buf = realloc(front_buf, count * sizeof(Cell));
    back_buf  = realloc(back_buf,  count * sizeof(Cell));
    if (!front_buf || !back_buf) {
        fprintf(stderr, "bfpp_rt: allocation failure\n");
        abort();
    }
    /* Clear both buffers to force full redraw on first frame */
    Cell blank = make_blank_cell();
    for (size_t i = 0; i < count; i++) {
        front_buf[i] = blank;
        back_buf[i]  = blank;
    }
    /* Mark front buffer as "unknown" so first end_frame redraws everything.
       Use a sentinel fg value that no real cell will match. */
    for (size_t i = 0; i < count; i++) {
        front_buf[i].fg = -2;
    }
}

/* ── Query terminal size ───────────────────────────────────────── */

static void query_size(void)
{
    struct winsize ws;
    if (ioctl(STDOUT_FILENO, TIOCGWINSZ, &ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0) {
        term_cols = ws.ws_col;
        term_rows = ws.ws_row;
    }
}

/* ── Public API ────────────────────────────────────────────────── */

void bfpp_tui_cleanup(void)
{
    if (!tui_initialized) return;
    tui_initialized = 0;

    /* Reset colors, show cursor, exit alternate screen */
    out_str("\033[0m");
    out_str("\033[?25h");
    out_str("\033[?1049l");
    flush_out();

    exit_raw_mode();
}

void bfpp_tui_init(void)
{
    if (tui_initialized) return;

    enter_raw_mode();

    /* Enter alternate screen, hide cursor, clear */
    out_str("\033[?1049h");
    out_str("\033[?25l");
    out_str("\033[2J");
    out_str("\033[H");
    flush_out();

    query_size();
    alloc_buffers();

    tui_initialized = 1;
    atexit(bfpp_tui_cleanup);
}

void bfpp_tui_get_size(int *cols, int *rows)
{
    if (cols) *cols = term_cols;
    if (rows) *rows = term_rows;
}

/* ── Frame lifecycle ───────────────────────────────────────────── */

void bfpp_tui_begin_frame(void)
{
    /* Check for terminal resize */
    int old_cols = term_cols;
    int old_rows = term_rows;
    query_size();
    if (term_cols != old_cols || term_rows != old_rows) {
        alloc_buffers();
    }

    /* Clear back buffer */
    size_t count = (size_t)term_cols * (size_t)term_rows;
    Cell blank = make_blank_cell();
    for (size_t i = 0; i < count; i++) {
        back_buf[i] = blank;
    }
}

/* Emit ANSI color codes, only when color actually changes */
static void emit_color(int fg, int bg)
{
    if (fg != last_fg) {
        if (fg == -1) {
            out_str("\033[39m");
        } else {
            out_fmt("\033[38;5;%dm", fg);
        }
        last_fg = fg;
    }
    if (bg != last_bg) {
        if (bg == -1) {
            out_str("\033[49m");
        } else {
            out_fmt("\033[48;5;%dm", bg);
        }
        last_bg = bg;
    }
}

void bfpp_tui_end_frame(void)
{
    size_t count = (size_t)term_cols * (size_t)term_rows;

    /* Reset color tracking at start of frame emit */
    last_fg = -2;
    last_bg = -2;

    int cursor_row = -1;
    int cursor_col = -1;

    for (size_t i = 0; i < count; i++) {
        Cell *f = &front_buf[i];
        Cell *b = &back_buf[i];

        /* Skip cells that haven't changed */
        if (cell_eq(f, b)) {
            continue;
        }

        int row = (int)(i / (size_t)term_cols);
        int col = (int)(i % (size_t)term_cols);

        /* Cursor movement optimization:
           If this cell is exactly where the cursor already is (auto-advanced
           from previous sequential write), skip the cursor move. */
        if (row != cursor_row || col != cursor_col) {
            out_fmt("\033[%d;%dH", row + 1, col + 1);
        }

        emit_color(b->fg, b->bg);

        /* Write the character (all UTF-8 bytes) */
        if (b->len > 0) {
            out_raw((const char *)b->utf8, (int)b->len);
        } else {
            out_raw(" ", 1);
        }

        /* Cursor auto-advances to next column */
        cursor_row = row;
        cursor_col = col + 1;
        if (cursor_col >= term_cols) {
            /* Terminal wraps to next row (or stays at last position) */
            cursor_row = -1;
            cursor_col = -1;
        }
    }

    flush_out();

    /* Swap: copy back buffer into front buffer */
    memcpy(front_buf, back_buf, count * sizeof(Cell));
}

/* ── Draw primitives ───────────────────────────────────────────── */

void bfpp_tui_put(int row, int col, uint8_t ch, int fg, int bg)
{
    if (row < 0 || row >= term_rows || col < 0 || col >= term_cols) return;
    size_t idx = (size_t)row * (size_t)term_cols + (size_t)col;
    back_buf[idx].utf8[0] = ch;
    back_buf[idx].utf8[1] = 0;
    back_buf[idx].utf8[2] = 0;
    back_buf[idx].utf8[3] = 0;
    back_buf[idx].len = 1;
    back_buf[idx].fg = (int16_t)fg;
    back_buf[idx].bg = (int16_t)bg;
}

void bfpp_tui_puts(int row, int col, const char *str, int fg, int bg)
{
    if (!str || row < 0 || row >= term_rows) return;
    int c = col;
    const uint8_t *p = (const uint8_t *)str;
    while (*p && c < term_cols) {
        int clen = utf8_char_len(*p);
        /* Verify we have enough bytes remaining */
        int valid = 1;
        for (int j = 1; j < clen; j++) {
            if (p[j] == 0) { valid = 0; break; }
        }
        if (!valid) break;

        if (c >= 0) {
            size_t idx = (size_t)row * (size_t)term_cols + (size_t)c;
            for (int j = 0; j < clen && j < 4; j++) {
                back_buf[idx].utf8[j] = p[j];
            }
            for (int j = clen; j < 4; j++) {
                back_buf[idx].utf8[j] = 0;
            }
            back_buf[idx].len = (uint8_t)clen;
            back_buf[idx].fg = (int16_t)fg;
            back_buf[idx].bg = (int16_t)bg;
        }
        p += clen;
        c++;
    }
}

void bfpp_tui_fill(int row, int col, int w, int h, uint8_t ch, int fg, int bg)
{
    for (int r = row; r < row + h; r++) {
        if (r < 0 || r >= term_rows) continue;
        for (int c = col; c < col + w; c++) {
            if (c < 0 || c >= term_cols) continue;
            size_t idx = (size_t)r * (size_t)term_cols + (size_t)c;
            back_buf[idx].utf8[0] = ch;
            back_buf[idx].utf8[1] = 0;
            back_buf[idx].utf8[2] = 0;
            back_buf[idx].utf8[3] = 0;
            back_buf[idx].len = 1;
            back_buf[idx].fg = (int16_t)fg;
            back_buf[idx].bg = (int16_t)bg;
        }
    }
}

/* ── Box drawing ───────────────────────────────────────────────── */

/* Box drawing character sets.
   Style 0: ASCII      +--+  |  |  +--+
   Style 1: Single     ┌──┐  │  │  └──┘
   Style 2: Rounded    ╭──╮  │  │  ╰──╯  */

/* For ASCII style we can use put() directly. For Unicode styles we
   use puts() since the characters are multi-byte UTF-8. */

static const char *box_tl[] = { "+",  "\xe2\x94\x8c", "\xe2\x95\xad" };  /* + ┌ ╭ */
static const char *box_tr[] = { "+",  "\xe2\x94\x90", "\xe2\x95\xae" };  /* + ┐ ╮ */
static const char *box_bl[] = { "+",  "\xe2\x94\x94", "\xe2\x95\xb0" };  /* + └ ╰ */
static const char *box_br[] = { "+",  "\xe2\x94\x98", "\xe2\x95\xaf" };  /* + ┘ ╯ */
static const char *box_hz[] = { "-",  "\xe2\x94\x80", "\xe2\x94\x80" };  /* - ─ ─ */
static const char *box_vt[] = { "|",  "\xe2\x94\x82", "\xe2\x94\x82" };  /* | │ │ */

void bfpp_tui_box(int row, int col, int w, int h, int style)
{
    if (w < 2 || h < 2) return;
    if (style < 0 || style > 2) style = 0;

    int fg = -1;
    int bg = -1;

    if (style == 0) {
        /* ASCII box: use put() for single-byte chars */
        bfpp_tui_put(row, col,             '+', fg, bg);
        bfpp_tui_put(row, col + w - 1,     '+', fg, bg);
        bfpp_tui_put(row + h - 1, col,     '+', fg, bg);
        bfpp_tui_put(row + h - 1, col + w - 1, '+', fg, bg);

        for (int c = col + 1; c < col + w - 1; c++) {
            bfpp_tui_put(row, c,         '-', fg, bg);
            bfpp_tui_put(row + h - 1, c, '-', fg, bg);
        }
        for (int r = row + 1; r < row + h - 1; r++) {
            bfpp_tui_put(r, col,         '|', fg, bg);
            bfpp_tui_put(r, col + w - 1, '|', fg, bg);
        }
    } else {
        /* Unicode box: use puts() since chars are multi-byte.
           Each Unicode box char occupies 1 terminal column but 3 bytes UTF-8.
           We write each corner/edge char as a 1-char string via puts(). */

        /* Corners */
        bfpp_tui_puts(row,         col,         box_tl[style], fg, bg);
        bfpp_tui_puts(row,         col + w - 1, box_tr[style], fg, bg);
        bfpp_tui_puts(row + h - 1, col,         box_bl[style], fg, bg);
        bfpp_tui_puts(row + h - 1, col + w - 1, box_br[style], fg, bg);

        /* Horizontal edges */
        for (int c = col + 1; c < col + w - 1; c++) {
            bfpp_tui_puts(row,         c, box_hz[style], fg, bg);
            bfpp_tui_puts(row + h - 1, c, box_hz[style], fg, bg);
        }

        /* Vertical edges */
        for (int r = row + 1; r < row + h - 1; r++) {
            bfpp_tui_puts(r, col,         box_vt[style], fg, bg);
            bfpp_tui_puts(r, col + w - 1, box_vt[style], fg, bg);
        }
    }
}

/* ── Key input ─────────────────────────────────────────────────── */

/* Read a single byte from stdin, returns -1 on failure/timeout.
   Uses poll() with the given timeout in milliseconds. */
static int read_byte(int timeout_ms)
{
    struct pollfd pfd;
    pfd.fd = STDIN_FILENO;
    pfd.events = POLLIN;
    pfd.revents = 0;

    int ret = poll(&pfd, 1, timeout_ms);
    if (ret <= 0) return -1;

    unsigned char c;
    ssize_t n = read(STDIN_FILENO, &c, 1);
    if (n != 1) return -1;
    return (int)c;
}

int bfpp_tui_poll_key(int timeout_ms)
{
    int c = read_byte(timeout_ms);
    if (c < 0) return -1;

    /* Not an escape sequence */
    if (c != 27) return c;

    /* Could be ESC key or start of escape sequence.
       Try to read the next byte with a short timeout.
       If nothing follows, it's a bare ESC keypress. */
    int c2 = read_byte(50);
    if (c2 < 0) return BFPP_KEY_ESC;

    if (c2 == '[') {
        /* CSI sequence: ESC [ ... */
        int c3 = read_byte(50);
        if (c3 < 0) return BFPP_KEY_ESC;

        switch (c3) {
            case 'A': return BFPP_KEY_UP;
            case 'B': return BFPP_KEY_DOWN;
            case 'C': return BFPP_KEY_RIGHT;
            case 'D': return BFPP_KEY_LEFT;
            case 'H': return BFPP_KEY_HOME;
            case 'F': return BFPP_KEY_END;
            case '3': {
                /* ESC [ 3 ~ = Delete */
                int c4 = read_byte(50);
                if (c4 == '~') return BFPP_KEY_DEL;
                /* Unknown sequence; discard */
                return -1;
            }
            case '5': {
                /* ESC [ 5 ~ = Page Up */
                int c4 = read_byte(50);
                if (c4 == '~') return BFPP_KEY_PGUP;
                return -1;
            }
            case '6': {
                /* ESC [ 6 ~ = Page Down */
                int c4 = read_byte(50);
                if (c4 == '~') return BFPP_KEY_PGDN;
                return -1;
            }
            default:
                /* Unknown CSI sequence */
                return -1;
        }
    } else if (c2 == 'O') {
        /* SS3 sequence: ESC O ... (some terminals send this for arrow keys) */
        int c3 = read_byte(50);
        if (c3 < 0) return BFPP_KEY_ESC;

        switch (c3) {
            case 'A': return BFPP_KEY_UP;
            case 'B': return BFPP_KEY_DOWN;
            case 'C': return BFPP_KEY_RIGHT;
            case 'D': return BFPP_KEY_LEFT;
            case 'H': return BFPP_KEY_HOME;
            case 'F': return BFPP_KEY_END;
            default:  return -1;
        }
    }

    /* ESC followed by something unexpected — treat as Alt+key or discard */
    return -1;
}
