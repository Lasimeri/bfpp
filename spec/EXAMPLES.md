# BF++ Examples

**Version**: 0.1.0

Annotated examples demonstrating BF++ language features.

---

## 1. Hello World (Classic BF — fully compatible)

```bfpp
; Classic BF hello world — works unmodified in BF++
++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>
---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.
```

---

## 2. Hello World (BF++ with string literals)

```bfpp
"Hello, World!\n"     ; write string to tape, ptr advances past it
<<<<<<<<<<<<<<        ; move back to start of string
[.>]                  ; print each byte until null
```

---

## 3. Hello World (BF++ with stdlib)

```bfpp
"Hello, World!\0"     ; write null-terminated string
<<<<<<<<<<<<<<        ; back to start
!#.>                  ; call print_string subroutine
```

---

## 4. Subroutine Definition and Call

```bfpp
; Define a subroutine that prints a string at ptr
!#pr{
  [.>]                ; output bytes until zero
  ^                   ; return
}

; Use it
"BF++ works!\n\0"
<<<<<<<<<<<<<
!#pr
```

---

## 5. Stack Operations

```bfpp
; Push values, pop in reverse
+++++ $               ; push 5
+++++ +++++ $         ; push 10 (cell was 5, +5 more = 10... no, cell is still at ptr)
[-]                   ; clear cell
+++++ +++++ $         ; push 10
[-]                   ; clear cell
~ .                   ; pop 10, print as byte
~ .                   ; pop 5, print as byte
```

---

## 6. Bitwise Operations

```bfpp
; Compute 0xAA & 0x0F = 0x0A
[-] +++++++++++ > [-] ++++++++++++++++  ; cell[0] = 0xAA (170), cell[1] = 0x0F (15)
; (simplified — actual values need correct counts)
< &                   ; cell[0] &= cell[1]
.                     ; output result
```

---

## 7. Cell Width Cycling

```bfpp
; Default: 8-bit cell
++++++++++            ; cell = 10
%                     ; now 16-bit
>+++++++++++++++++    ; next byte contributes to 16-bit value
<                     ; back to 16-bit cell start
.                     ; output (16-bit value, low byte)
```

---

## 8. Absolute Address and Dereference

```bfpp
; Set cell[0] = 5, cell[5] = 65 ('A')
+++++ > > > > >       ; move to cell 5
[-] +++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++  ; cell[5] = 65
<<<<<<                ; back to cell 0
; cell[0] = 5
@                     ; ptr = tape[0] = 5, now pointing at cell 5
.                     ; prints 'A'
```

---

## 9. Error Handling — Propagation with `?`

```bfpp
; Subroutine that may fail
!#risky{
  ++++++              ; set cell to 6 (ERR_INVALID_ARG)
  e                   ; write to error register
  ^                   ; return with error set
}

; Subroutine that chains calls
!#chain{
  !#risky ?           ; call risky, propagate error if any
  ; this line only reached if no error
  "OK\0" !#.>
  ^
}

; Top-level error handling
R{
  !#chain
}K{
  E                   ; load error code into cell
  ; error code 6 is now in current cell
  "Error occurred!\n\0"
  !#.>
}
```

---

## 10. Syscall — File Write

```bfpp
; Write "Hello" to a file using raw syscalls (Linux x86_64)
; Note: stdlib !#fo and !#fw are preferred

@128 %64              ; go to syscall region, 64-bit mode

; syscall 2 = open
++                    ; tape[0x8000] = 2 (sys_open)
> "output.txt\0"      ; filename at next cells
\                     ; execute syscall
?                     ; propagate error if open failed
; fd now in tape[0x8000]

; Save fd, prepare write syscall
$                     ; push fd to stack
[-] +                 ; tape[0x8000] = 1 (sys_write)
> ~                   ; pop fd into arg1
> "Hello\0"           ; data to write at arg2 (pointer)
> +++++ >             ; arg3 = 5 (byte count)
<<<<<
\                     ; execute write syscall
?                     ; propagate error
```

---

## 11. Fd-Extended I/O

```bfpp
; Write byte to stderr (fd 2)
+++++ +++++ +++++ +++++ +++++ +++++ +++++  ; cell = 35 = '#'
.{2}                  ; write '#' to fd 2 (stderr)

; Read byte from fd 3 (previously opened file)
,{3}                  ; read byte from fd 3 into current cell
```

---

## 12. TCP Echo Server (using stdlib)

```bfpp
; Listen on port 8080, echo back whatever is received

; Set up port: 8080 = 0x1F90
[-] > [-]
++++++++++++++++++++++++++++++++ ; high byte approximation
> [-]
; (exact byte setup omitted for brevity)

!#tl ?                ; tcp_listen, propagate error

; Accept loop
[
  !#ta ?              ; accept connection, get client fd
  $                   ; save client fd

  ; Read from client
  ~                   ; restore fd
  > [-] +++++++++++++++++++++++++++++++ > ; count = 32, buffer at ptr+2
  !#tr ?              ; tcp_recv

  ; Echo back
  ; (re-setup fd, count, data pointer)
  !#ts ?              ; tcp_send

  +                   ; keep looping (non-zero cell)
]
```

---

## 13. Result/Catch Block

```bfpp
R{
  "test.txt\0"
  !#fo                ; try to open file
}K{
  E                   ; get error code
  ; check if error is 2 (file not found)
  --
  [
    ; error was not 2, re-propagate
    ++ e ?
  ]
  ; error was 2 — create the file instead
  "test.txt\0"
  < + <               ; set flags to 1 (write/create)
  >> !#fo ?           ; open for writing
}
```

---

## 14. Recursive Factorial

```bfpp
; Factorial subroutine — tape[ptr] = n, returns n! in tape[ptr]
; Uses stack for recursive state

!#fac{
  ; if n <= 1, return 1
  $                   ; save n
  -                   ; n-1
  [                   ; if n-1 != 0 (i.e., n > 1)
    ; recursive case
    !#fac             ; factorial(n-1), result in cell
    ~ >               ; pop original n into next cell
    < !#m*            ; multiply: cell[ptr] = (n-1)! * n
    ^                 ; return
  ]
  ; base case: n <= 1
  ~                   ; restore n (but we return 1)
  [-] +               ; cell = 1
  ^                   ; return 1
}

; Compute 5!
+++++ !#fac           ; tape[ptr] = 120
```

---

## 15. Comment Style

```bfpp
; This is a single-line comment
; Everything after ; is ignored until newline

+++ ; increment by 3
[-] ; clear cell

; BF++ uses ; for comments because
; # is reserved for subroutine names
; and // would conflict with potential future operators
```

---

## 16. Numeric Literals (`#N`)

```bfpp
; Set cell to known value directly — no increment chains needed
#72 .                   ; print 'H' (ASCII 72)
#101 .                  ; print 'e'
#108 . .                ; print 'l' twice
#111 .                  ; print 'o'
#10 .                   ; print newline

; Hex format
#0xFF                   ; cell = 255
#0x9000                 ; cell = 36864 (heap metadata address)

; Large values (requires wider cell)
%4 #40960              ; 32-bit cell, value = 0xA000 (framebuffer offset)

; Clear cell to zero
#0                      ; equivalent to [-] but generates a direct bfpp_set
```

---

## 17. Direct Cell Width (`%N`)

```bfpp
; Set cell width without cycling through intermediate widths
%8                      ; 64-bit cell (for syscall args)
#41                     ; SYS_socket = 41

; Set up a 16-bit value
%2 #0x1000              ; 16-bit cell, value = 4096

; Restore to byte width
%1                      ; back to 8-bit

; Compare with cycling (%):
; % cycles: 8 -> 16 -> 32 -> 64 -> 8
; %4 jumps directly to 32-bit
```

---

## 18. Block Comments

```bfpp
; Line comments use ;
/* Block comments use C-style delimiters */

+ /* this is a block comment spanning
     multiple lines */ -

/* Block comments support nesting:
   /* inner comment */
   still in outer comment
*/

; Useful for commenting out code blocks:
/* [-] +++ . */         ; this code is disabled
```

---

## 19. Compiler Intrinsics

```bfpp
; Intrinsics are subroutine calls with __ prefix that emit inline C.
; They bridge BF++ to system APIs that can't be expressed in pure BF++.

!include "io.bfpp"

; --- Read environment variable ---
"HOME\0"
<<<<<                   ; back to start of "HOME"
!#__getenv              ; overwrites "HOME" with the value (e.g., "/home/user")
!#.>                    ; print the path
#10 . [-]               ; newline

; --- Get process ID ---
[-]
!#__getpid              ; tape[ptr] = pid
!#.+                    ; print as decimal
#10 .                   ; newline

; --- Monotonic timestamp ---
[-]
!#__time_ms             ; tape[ptr] = milliseconds since boot
!#.+
#10 .

; --- Sleep ---
[-]
#50                     ; 50 milliseconds
!#__sleep               ; pause execution

; --- Exit cleanly ---
[-]
#0
!#__exit                ; exit(0)
```

---

## 20. TUI Runtime (Double-Buffered Terminal UI)

```bfpp
; The TUI runtime provides a double-buffered terminal UI.
; __tui_* intrinsics require the C runtime library (bfpp_rt.h).

; Initialize TUI (raw mode, alternate screen, hide cursor)
!#__tui_init

; Get terminal size
!#__tui_size            ; tape[ptr]=cols, tape[ptr+1]=rows

; Main render loop
#1                      ; loop flag
[
    !#__tui_begin       ; start frame

    ; Draw a character: row=5, col=10, char='X', fg=2 (green), bg=-1 (default)
    #5 > #10 > #88 > #2 > #255 <<<<
    ;                              ^ -1 as unsigned byte = 255 for default color
    !#__tui_put

    ; Fill a rectangle: row=0, col=0, w=20, h=3, char=' ', fg=7, bg=4
    #0 > #0 > #20 > #3 > #32 > #7 > #4 <<<<<<
    !#__tui_fill

    ; Draw a box: row=10, col=5, w=30, h=10, style=0
    #10 > #5 > #30 > #10 > #0 <<<<
    !#__tui_box

    !#__tui_end         ; end frame (diff and render)

    ; Poll for keypress (100ms timeout)
    #100
    !#__tui_key         ; tape[ptr] = keycode or -1

    ; Check for 'q' to quit (ASCII 113)
    ; (simplified — real code would compare the value)
]

; Cleanup (restore terminal)
!#__tui_cleanup
```

---

## 21. Intrinsics Demo (Complete Program)

```bfpp
; Full intrinsics demo — compile with: bfpp examples/intrinsics_demo.bfpp -o demo

!include "io.bfpp"

; --- __getenv: read environment variable ---
"HOME\0"
<<<<<                   ; back to start of "HOME"
!#__getenv              ; reads HOME, writes value at ptr
!#.>                    ; print the result path
#10 . [-]               ; newline

; --- __getpid: get process ID ---
[-]
!#__getpid              ; tape[ptr] = pid
!#.+                    ; print as decimal
#10 .                   ; newline

; --- __time_ms: monotonic timestamp ---
[-]
!#__time_ms             ; tape[ptr] = timestamp in ms
!#.+                    ; print timestamp
#10 .

; --- __sleep: pause execution ---
[-]
#50                     ; 50 milliseconds
!#__sleep               ; pause

; --- __time_ms again to show elapsed time ---
[-]
!#__time_ms
!#.+
#10 .

; --- __exit: clean exit with code 0 ---
[-]
#0
!#__exit
```

---

## 22. Framebuffer Graphics

```bfpp
; Draw a red pixel and clear to blue on a 320x200 framebuffer.
; Compile with: bfpp prog.bfpp --framebuffer 320x200 --tape-size 262144 -o demo

!include "graphics.bfpp"

; Clear screen to blue
%4                          ; 32-bit cells for FB addresses
#40960 >                    ; P+0 = fb_offset (0xA000)
#320 >                      ; P+1 = width
#200 >                      ; P+2 = height
#0 >                        ; P+3 = r
#0 >                        ; P+4 = g
#255                        ; P+5 = b
<<<<<                       ; back to P+0
!#gc                        ; clear_fb
F                           ; flush to screen

; Draw a red pixel at (160, 100) — center of screen
; (ptr is now inside FB, so navigate to a clean area first)
; ... set up params at known clean tape position ...
```
