#ifndef BFPP_FB_TERMINAL_H
#define BFPP_FB_TERMINAL_H

/*
 * bfpp_fb_terminal.h — Terminal framebuffer backend for headless/SSH.
 *
 * True-color ANSI rendering with delta encoding and adaptive frame rate.
 * Target: 256KB/s SSH connections.
 *
 * Override bandwidth: BFPP_TERMINAL_BW=128 (KB/s)
 * Force terminal mode: BFPP_TERMINAL_FB=1
 */

#include <stdint.h>

/* Detect if terminal backend should be used (no display server). */
int bfpp_fb_terminal_detect(void);

/* Initialize terminal backend. */
void bfpp_fb_terminal_init(int width, int height, uint8_t *tape, int fb_offset);

/* Present a frame (delta-encoded, bandwidth-adaptive). */
void bfpp_fb_terminal_present(void);

/* Cleanup: restore terminal state. */
void bfpp_fb_terminal_cleanup(void);

/* Check if quit was requested via terminal input. */
int bfpp_fb_terminal_should_quit(void);

#endif /* BFPP_FB_TERMINAL_H */
