#ifndef BFPP_RT_H
#define BFPP_RT_H

/*
 * bfpp_rt.h — Public API for the BF++ double-buffered TUI runtime.
 *
 * Lifecycle: init → (begin_frame → draw → end_frame)* → cleanup
 * Drawing functions operate on a back buffer. end_frame() diffs against the
 * front buffer and emits minimal ANSI updates to the terminal.
 * Input is non-blocking via poll_key() with configurable timeout.
 */

#include <stdint.h>

/* ── Terminal UI subsystem ─────────────────────────────────────── */

// Initialize: save termios, enter raw mode, alternate screen, hide cursor.
// Registers atexit cleanup for crash safety.
void bfpp_tui_init(void);

// Restore terminal: show cursor, exit alternate screen, restore termios.
void bfpp_tui_cleanup(void);

// Get terminal dimensions.
void bfpp_tui_get_size(int *cols, int *rows);

// Double-buffered rendering. Call begin_frame before drawing, end_frame
// to diff and emit minimal ANSI updates to the real terminal.
void bfpp_tui_begin_frame(void);
void bfpp_tui_end_frame(void);

// Draw primitives — all write to the back buffer, not directly to terminal.
void bfpp_tui_put(int row, int col, uint8_t ch, int fg, int bg);
void bfpp_tui_puts(int row, int col, const char *str, int fg, int bg);
void bfpp_tui_fill(int row, int col, int w, int h, uint8_t ch, int fg, int bg);
void bfpp_tui_box(int row, int col, int w, int h, int style);

// Input: poll for keypress with timeout in milliseconds.
// Returns character code, or -1 on timeout. Handles escape sequences
// for arrow keys (returns 1000+offset for special keys).
int bfpp_tui_poll_key(int timeout_ms);

// Key constants for special keys.
// Values 1000+ are synthetic codes for multi-byte escape sequences,
// chosen to avoid collision with any valid byte value (0-255).
// Values below 128 are their literal ASCII/control codes.
#define BFPP_KEY_UP     1000    // ESC [ A  or  ESC O A
#define BFPP_KEY_DOWN   1001    // ESC [ B  or  ESC O B
#define BFPP_KEY_RIGHT  1002    // ESC [ C  or  ESC O C
#define BFPP_KEY_LEFT   1003    // ESC [ D  or  ESC O D
#define BFPP_KEY_HOME   1004    // ESC [ H  or  ESC O H
#define BFPP_KEY_END    1005    // ESC [ F  or  ESC O F
#define BFPP_KEY_PGUP   1006    // ESC [ 5 ~
#define BFPP_KEY_PGDN   1007    // ESC [ 6 ~
#define BFPP_KEY_DEL    1008    // ESC [ 3 ~
#define BFPP_KEY_BACKSPACE 127  // ASCII DEL
#define BFPP_KEY_ENTER  13      // ASCII CR
#define BFPP_KEY_TAB    9       // ASCII HT
#define BFPP_KEY_ESC    27      // bare ESC (no following sequence bytes)

// Colors use xterm-256 encoding:
//   -1       = terminal default (emits ESC[39m / ESC[49m)
//   0-7      = standard ANSI colors (black, red, green, yellow, blue, magenta, cyan, white)
//   8-15     = bright/bold variants of standard colors
//   16-231   = 6x6x6 RGB color cube: index = 16 + 36*r + 6*g + b  (r,g,b in 0-5)
//   232-255  = 24-step grayscale ramp (232=dark gray ... 255=white)
#define BFPP_COLOR_DEFAULT (-1)

#endif /* BFPP_RT_H */
