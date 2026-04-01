/*
 * bfpp_fb_terminal.c — Terminal framebuffer backend for headless/SSH rendering
 *
 * Architecture:
 *   When no display server is available ($DISPLAY/$WAYLAND_DISPLAY unset),
 *   the framebuffer pipeline falls back to this terminal renderer. It
 *   downsamples the framebuffer to terminal dimensions and outputs
 *   true-color ANSI escape sequences (24-bit: \033[38;2;R;G;Bm).
 *
 *   Bandwidth-optimized for SSH (~256KB/s target):
 *   - Delta encoding: only changed cells are re-emitted
 *   - Adaptive frame rate: measures write throughput, throttles to budget
 *   - Cursor movement minimized: sequential writes skip cursor-move escapes
 *   - Double-buffered: front_buf vs back_buf diff drives output
 *
 *   Each terminal cell represents a block of framebuffer pixels, averaged
 *   to a single RGB color, rendered as a colored block character (█).
 *
 * Bandwidth analysis at 80x24:
 *   Full redraw:  ~20 bytes/cell × 1920 cells = 38.4 KB
 *   Delta (10%):  ~3.8 KB typical
 *   At 256KB/s:   ~67 fps delta, ~6.6 fps full redraw
 *
 * Threading: called from the presenter thread (same as SDL path).
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <unistd.h>
#include <time.h>
#include <termios.h>
#include <sys/ioctl.h>
#include <poll.h>

#ifdef __x86_64__
#include <immintrin.h>
#endif

/* ── Terminal cell ──────────────────────────────────────────── */

typedef struct {
    uint8_t r, g, b;
} term_cell_t;

/* ── State ──────────────────────────────────────────────────── */

static struct {
    int term_cols, term_rows;   /* terminal dimensions */
    int fb_width, fb_height;    /* framebuffer pixel dimensions */
    uint8_t *tape;
    int fb_offset;
    int stride;                 /* fb_width * 3 */

    /* Double buffer for delta encoding */
    term_cell_t *front;         /* what the terminal currently shows */
    term_cell_t *back;          /* what we want to show next */
    int buf_size;               /* term_cols * term_rows */

    /* Output buffer — batch all ANSI output into one write() */
    char *out_buf;
    int out_len;
    int out_cap;

    /* Adaptive frame rate */
    uint64_t bandwidth_budget;  /* bytes per second (default 256KB) */
    uint64_t frame_budget_bytes;/* max bytes per frame at current fps */
    int target_fps;             /* current target fps (adapts) */
    uint64_t last_frame_ns;
    uint64_t bytes_this_second;
    uint64_t second_start_ns;

    /* Terminal state */
    struct termios orig_termios;
    int raw_mode;
    int initialized;

    /* Input */
    atomic_int quit_requested;
} tctx;

/* ── Time helper ────────────────────────────────────────────── */

static uint64_t now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + ts.tv_nsec;
}

/* ── Output buffer ──────────────────────────────────────────── */

static void out_ensure(int needed)
{
    if (tctx.out_len + needed > tctx.out_cap) {
        tctx.out_cap = (tctx.out_len + needed) * 2;
        tctx.out_buf = realloc(tctx.out_buf, tctx.out_cap);
    }
}

static void out_raw(const char *data, int len)
{
    out_ensure(len);
    memcpy(tctx.out_buf + tctx.out_len, data, len);
    tctx.out_len += len;
}

static void out_fmt(const char *fmt, ...)
{
    char tmp[128];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(tmp, sizeof(tmp), fmt, ap);
    va_end(ap);
    if (n > 0) out_raw(tmp, n);
}

static void out_flush(void)
{
    if (tctx.out_len > 0) {
        write(STDOUT_FILENO, tctx.out_buf, tctx.out_len);
        tctx.bytes_this_second += tctx.out_len;
        tctx.out_len = 0;
    }
}

/* ── Terminal setup ─────────────────────────────────────────── */

static void query_term_size(void)
{
    struct winsize ws;
    if (ioctl(STDOUT_FILENO, TIOCGWINSZ, &ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0) {
        tctx.term_cols = ws.ws_col;
        tctx.term_rows = ws.ws_row - 1; /* reserve bottom row for status */
    } else {
        tctx.term_cols = 80;
        tctx.term_rows = 23;
    }
}

static void enter_raw_mode(void)
{
    if (tctx.raw_mode) return;
    tcgetattr(STDIN_FILENO, &tctx.orig_termios);
    struct termios raw = tctx.orig_termios;
    raw.c_lflag &= ~(unsigned)(ECHO | ICANON | ISIG);
    raw.c_cc[VMIN] = 0;
    raw.c_cc[VTIME] = 0;
    tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw);
    tctx.raw_mode = 1;
}

static void exit_raw_mode(void)
{
    if (!tctx.raw_mode) return;
    tcsetattr(STDIN_FILENO, TCSAFLUSH, &tctx.orig_termios);
    tctx.raw_mode = 0;
}

/* ── Downsample framebuffer to terminal grid ────────────────── */

static void downsample(void)
{
    uint8_t *fb = tctx.tape + tctx.fb_offset;
    int fw = tctx.fb_width;
    int fh = tctx.fb_height;
    int tw = tctx.term_cols;
    int th = tctx.term_rows;

    /* Block size: each terminal cell covers a block of pixels */
    /* Use floating-point-free integer math */
    for (int ty = 0; ty < th; ty++) {
        int py0 = ty * fh / th;
        int py1 = (ty + 1) * fh / th;
        if (py1 > fh) py1 = fh;
        if (py1 <= py0) py1 = py0 + 1;

        for (int tx = 0; tx < tw; tx++) {
            int px0 = tx * fw / tw;
            int px1 = (tx + 1) * fw / tw;
            if (px1 > fw) px1 = fw;
            if (px1 <= px0) px1 = px0 + 1;

            /* Average the pixel block */
            int sum_r = 0, sum_g = 0, sum_b = 0;
            int count = 0;
            int block_w = px1 - px0;

#ifdef __AVX2__
            /* AVX2: accumulate 8 pixels at a time per row.
             * Load 8 RGB triplets (24 bytes) as two 128-bit loads,
             * widen bytes to 16-bit, horizontal-add R/G/B channels. */
            if (block_w >= 8) {
                __m256i acc_r = _mm256_setzero_si256();
                __m256i acc_g = _mm256_setzero_si256();
                __m256i acc_b = _mm256_setzero_si256();

                for (int py = py0; py < py1; py++) {
                    int row_off = py * fw * 3;
                    int px = px0;
                    for (; px + 7 < px1; px += 8) {
                        /* Load 24 bytes (8 RGB pixels) — use unaligned load */
                        const uint8_t *p = fb + row_off + px * 3;
                        /* Extract R, G, B for 8 pixels manually */
                        __m128i rvals = _mm_set_epi8(0,0,0,0,0,0,0,0,
                            p[21],p[18],p[15],p[12],p[9],p[6],p[3],p[0]);
                        __m128i gvals = _mm_set_epi8(0,0,0,0,0,0,0,0,
                            p[22],p[19],p[16],p[13],p[10],p[7],p[4],p[1]);
                        __m128i bvals = _mm_set_epi8(0,0,0,0,0,0,0,0,
                            p[23],p[20],p[17],p[14],p[11],p[8],p[5],p[2]);
                        /* Widen to 16-bit and accumulate */
                        acc_r = _mm256_add_epi16(acc_r, _mm256_cvtepu8_epi16(rvals));
                        acc_g = _mm256_add_epi16(acc_g, _mm256_cvtepu8_epi16(gvals));
                        acc_b = _mm256_add_epi16(acc_b, _mm256_cvtepu8_epi16(bvals));
                        count += 8;
                    }
                    /* Scalar tail for remaining pixels in this row */
                    for (; px < px1; px++) {
                        int idx = row_off + px * 3;
                        sum_r += fb[idx]; sum_g += fb[idx+1]; sum_b += fb[idx+2];
                        count++;
                    }
                }
                /* Horizontal reduce the AVX accumulators */
                int16_t rbuf[16], gbuf[16], bbuf[16];
                _mm256_storeu_si256((__m256i*)rbuf, acc_r);
                _mm256_storeu_si256((__m256i*)gbuf, acc_g);
                _mm256_storeu_si256((__m256i*)bbuf, acc_b);
                for (int k = 0; k < 16; k++) {
                    sum_r += rbuf[k]; sum_g += gbuf[k]; sum_b += bbuf[k];
                }
            } else
#endif /* __AVX2__ */
            {
                /* Scalar path */
                for (int py = py0; py < py1; py++) {
                    for (int px = px0; px < px1; px++) {
                        int idx = (py * fw + px) * 3;
                        sum_r += fb[idx]; sum_g += fb[idx+1]; sum_b += fb[idx+2];
                        count++;
                    }
                }
            }
            if (count > 0) {
                tctx.back[ty * tw + tx].r = sum_r / count;
                tctx.back[ty * tw + tx].g = sum_g / count;
                tctx.back[ty * tw + tx].b = sum_b / count;
            }
        }
    }
}

/* ── Delta-encoded render to terminal ───────────────────────── */

static void render_delta(void)
{
    int tw = tctx.term_cols;
    int th = tctx.term_rows;
    int cursor_row = -1, cursor_col = -1;
    int last_r = -1, last_g = -1, last_b = -1;

    for (int y = 0; y < th; y++) {
        for (int x = 0; x < tw; x++) {
            int idx = y * tw + x;
            term_cell_t *f = &tctx.front[idx];
            term_cell_t *b = &tctx.back[idx];

            /* Skip unchanged cells */
            if (f->r == b->r && f->g == b->g && f->b == b->b)
                continue;

            /* Cursor movement — skip if we're already at the right position */
            if (y != cursor_row || x != cursor_col) {
                out_fmt("\033[%d;%dH", y + 1, x + 1);
            }

            /* Color change — skip if same as last emitted */
            if (b->r != last_r || b->g != last_g || b->b != last_b) {
                out_fmt("\033[38;2;%d;%d;%dm", b->r, b->g, b->b);
                last_r = b->r;
                last_g = b->g;
                last_b = b->b;
            }

            /* RLE: count consecutive same-color changed cells on this row */
            int run = 1;
            while (x + run < tw) {
                int ni = y * tw + x + run;
                term_cell_t *nf = &tctx.front[ni];
                term_cell_t *nb = &tctx.back[ni];
                /* Next cell must be: changed AND same color as current */
                if (nf->r == nb->r && nf->g == nb->g && nf->b == nb->b)
                    break; /* unchanged — stop run */
                if (nb->r != b->r || nb->g != b->g || nb->b != b->b)
                    break; /* different color — stop run */
                run++;
            }

            /* Emit `run` block characters (all same color, no re-emit needed) */
            /* UTF-8 for █ (U+2588): 0xE2 0x96 0x88 — 3 bytes per char */
            for (int k = 0; k < run; k++)
                out_raw("\xe2\x96\x88", 3);

            x += run - 1; /* advance past the run (loop increments by 1 more) */

            cursor_row = y;
            cursor_col = x + 1;
            if (cursor_col >= tw) {
                cursor_row = -1;
                cursor_col = -1;
            }
        }
    }

    /* Swap front ← back */
    memcpy(tctx.front, tctx.back, tctx.buf_size * sizeof(term_cell_t));
}

/* ── Status bar ─────────────────────────────────────────────── */

static void render_status(void)
{
    int row = tctx.term_rows + 1;
    out_fmt("\033[%d;1H\033[0m\033[7m", row); /* inverse video */

    char status[128];
    int n = snprintf(status, sizeof(status),
        " BF++ %dx%d -> %dx%d | %d fps | %lu KB/s ",
        tctx.fb_width, tctx.fb_height,
        tctx.term_cols, tctx.term_rows,
        tctx.target_fps,
        (unsigned long)(tctx.bytes_this_second / 1024));

    out_raw(status, n);
    /* Pad to full width */
    for (int i = n; i < tctx.term_cols; i++)
        out_raw(" ", 1);
    out_raw("\033[0m", 4); /* reset */
}

/* ── Adaptive frame rate ────────────────────────────────────── */

static void adapt_framerate(void)
{
    uint64_t now = now_ns();
    uint64_t elapsed = now - tctx.second_start_ns;

    /* Reset bandwidth counter every second */
    if (elapsed >= 1000000000ULL) {
        uint64_t actual_bps = tctx.bytes_this_second * 1000000000ULL / elapsed;

        /* Adjust target fps based on bandwidth utilization */
        if (actual_bps > tctx.bandwidth_budget * 9 / 10) {
            /* Over budget — reduce fps */
            tctx.target_fps = tctx.target_fps * 3 / 4;
            if (tctx.target_fps < 1) tctx.target_fps = 1;
        } else if (actual_bps < tctx.bandwidth_budget * 6 / 10 && tctx.target_fps < 60) {
            /* Under budget — increase fps */
            tctx.target_fps = tctx.target_fps * 5 / 4;
            if (tctx.target_fps > 60) tctx.target_fps = 60;
        }

        tctx.bytes_this_second = 0;
        tctx.second_start_ns = now;
    }
}

/* ── Keyboard input (non-blocking) ──────────────────────────── */

static int poll_stdin(void)
{
    struct pollfd pfd = { .fd = STDIN_FILENO, .events = POLLIN };
    if (poll(&pfd, 1, 0) > 0) {
        unsigned char c;
        if (read(STDIN_FILENO, &c, 1) == 1) {
            if (c == 3 || c == 17) { /* Ctrl+C or Ctrl+Q */
                atomic_store(&tctx.quit_requested, 1);
                return 1;
            }
        }
    }
    return 0;
}

/* ── Public API ─────────────────────────────────────────────── */

/*
 * Detect if we should use the terminal backend.
 * Returns 1 if headless (no display server), 0 if display available.
 */
int bfpp_fb_terminal_detect(void)
{
    const char *display = getenv("DISPLAY");
    const char *wayland = getenv("WAYLAND_DISPLAY");
    const char *force = getenv("BFPP_TERMINAL_FB");

    /* Force terminal mode if explicitly requested */
    if (force && force[0] != '0') return 1;

    /* No display server → use terminal */
    if (!display && !wayland) return 1;
    if (display && display[0] == '\0') return 1;

    return 0;
}

/*
 * Initialize the terminal framebuffer backend.
 * Called instead of SDL init when headless is detected.
 */
void bfpp_fb_terminal_init(int width, int height, uint8_t *tape, int fb_offset)
{
    memset(&tctx, 0, sizeof(tctx));

    tctx.fb_width = width;
    tctx.fb_height = height;
    tctx.tape = tape;
    tctx.fb_offset = fb_offset;
    tctx.stride = width * 3;

    query_term_size();

    tctx.buf_size = tctx.term_cols * tctx.term_rows;
    tctx.front = calloc(tctx.buf_size, sizeof(term_cell_t));
    tctx.back = calloc(tctx.buf_size, sizeof(term_cell_t));

    tctx.out_cap = 65536;
    tctx.out_buf = malloc(tctx.out_cap);
    tctx.out_len = 0;

    /* Default 256KB/s bandwidth budget */
    const char *bw = getenv("BFPP_TERMINAL_BW");
    tctx.bandwidth_budget = bw ? (uint64_t)atoi(bw) * 1024 : 256 * 1024;
    tctx.target_fps = 15; /* start conservative, adapt up */
    tctx.second_start_ns = now_ns();

    enter_raw_mode();

    /* Alt screen + hide cursor + clear */
    out_raw("\033[?1049h\033[?25l\033[2J\033[H", 23);
    out_flush();

    /* Force first frame to be a full redraw by poisoning front buffer */
    memset(tctx.front, 0xFF, tctx.buf_size * sizeof(term_cell_t));

    tctx.initialized = 1;

    fprintf(stderr, "bfpp_fb: terminal mode %dx%d (fb %dx%d, budget %lu KB/s)\n",
            tctx.term_cols, tctx.term_rows, width, height,
            (unsigned long)(tctx.bandwidth_budget / 1024));
}

/*
 * Present a frame via the terminal backend.
 * Called by the presenter thread on each flush.
 */
void bfpp_fb_terminal_present(void)
{
    if (!tctx.initialized) return;

    /* Adaptive frame rate: skip if too soon */
    uint64_t now = now_ns();
    uint64_t min_interval = 1000000000ULL / (tctx.target_fps > 0 ? tctx.target_fps : 1);
    if (now - tctx.last_frame_ns < min_interval)
        return;
    tctx.last_frame_ns = now;

    /* Check for terminal resize */
    int old_cols = tctx.term_cols, old_rows = tctx.term_rows;
    query_term_size();
    if (tctx.term_cols != old_cols || tctx.term_rows != old_rows) {
        tctx.buf_size = tctx.term_cols * tctx.term_rows;
        tctx.front = realloc(tctx.front, tctx.buf_size * sizeof(term_cell_t));
        tctx.back = realloc(tctx.back, tctx.buf_size * sizeof(term_cell_t));
        memset(tctx.front, 0xFF, tctx.buf_size * sizeof(term_cell_t));
        out_raw("\033[2J", 4); /* clear on resize */
    }

    /* Check for quit key */
    poll_stdin();

    /* Downsample framebuffer → terminal grid */
    downsample();

    /* Delta-encoded render */
    render_delta();

    /* Status bar */
    render_status();

    /* Flush all output as one write() call */
    out_flush();

    /* Adapt frame rate based on bandwidth usage */
    adapt_framerate();
}

/*
 * Cleanup: restore terminal state.
 */
void bfpp_fb_terminal_cleanup(void)
{
    if (!tctx.initialized) return;

    /* Show cursor + exit alt screen + reset colors */
    out_raw("\033[?25h\033[?1049l\033[0m", 19);
    out_flush();

    exit_raw_mode();

    free(tctx.front);
    free(tctx.back);
    free(tctx.out_buf);

    tctx.initialized = 0;
}

/*
 * Returns 1 if quit was requested (Ctrl+C/Ctrl+Q).
 */
int bfpp_fb_terminal_should_quit(void)
{
    return atomic_load(&tctx.quit_requested);
}
