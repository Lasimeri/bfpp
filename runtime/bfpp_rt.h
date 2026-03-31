#ifndef BFPP_RT_H
#define BFPP_RT_H

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

// Key constants for special keys
#define BFPP_KEY_UP     1000
#define BFPP_KEY_DOWN   1001
#define BFPP_KEY_RIGHT  1002
#define BFPP_KEY_LEFT   1003
#define BFPP_KEY_HOME   1004
#define BFPP_KEY_END    1005
#define BFPP_KEY_PGUP   1006
#define BFPP_KEY_PGDN   1007
#define BFPP_KEY_DEL    1008
#define BFPP_KEY_BACKSPACE 127
#define BFPP_KEY_ENTER  13
#define BFPP_KEY_TAB    9
#define BFPP_KEY_ESC    27

// Colors: -1 = default, 0-7 standard, 8-15 bright, 16-231 RGB cube, 232-255 grayscale
#define BFPP_COLOR_DEFAULT (-1)

#endif /* BFPP_RT_H */
