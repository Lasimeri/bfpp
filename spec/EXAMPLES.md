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
