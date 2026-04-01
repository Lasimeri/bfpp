#!/usr/bin/env bash
# BF++ Integration Test Runner
# Usage: ./test_runner.sh [path-to-bfpp-binary]

set -euo pipefail

BFPP="${1:-../../target/release/bfpp}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
STDLIB_DIR="$SCRIPT_DIR/../../stdlib"
PASS=0
FAIL=0
ERRORS=""

run_test() {
    local name="$1"
    local source="$2"
    local expected="$3"
    local extra_args="${4:-}"
    local tmpbin="/tmp/bfpp_test_$$_${name}"

    if ! "$BFPP" "$source" -o "$tmpbin" --include "$STDLIB_DIR" $extra_args 2>/dev/null; then
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL: ${name} — compilation failed"
        return
    fi

    local actual_file="/tmp/bfpp_test_$$_${name}_out"
    local expected_file="/tmp/bfpp_test_$$_${name}_exp"
    "$tmpbin" > "$actual_file" 2>&1 || true
    rm -f "$tmpbin"

    # Normalize: strip trailing whitespace/newlines for comparison.
    # Avoids false failures from trailing newline differences between
    # program output and expected files.
    perl -pe 'chomp if eof' "$actual_file" > "${actual_file}.norm" 2>/dev/null
    perl -pe 'chomp if eof' "$expected" > "${expected_file}.norm" 2>/dev/null

    if cmp -s "${actual_file}.norm" "${expected_file}.norm"; then
        PASS=$((PASS + 1))
        echo "  PASS: ${name}"
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL: ${name}"
        ERRORS="${ERRORS}\n    expected: $(head -c 60 "$expected")"
        ERRORS="${ERRORS}\n    actual:   $(head -c 60 "$actual_file")"
    fi
    rm -f "$actual_file" "${actual_file}.norm" "${expected_file}.norm"
}

echo "BF++ Integration Tests"
echo "======================"
echo "Using: $BFPP"
echo ""

# === Original tests ===

# Classic BF hello world
run_test "hello_classic" \
    "$SCRIPT_DIR/../../examples/hello.bfpp" \
    "$SCRIPT_DIR/expected_hello.txt"

# BF++ string literal + subroutine hello
run_test "hello_bfpp" \
    "$SCRIPT_DIR/../../examples/hello_bfpp.bfpp" \
    "$SCRIPT_DIR/expected_hello_bfpp.txt"

# Error handling
run_test "error_handling" \
    "$SCRIPT_DIR/../../examples/error_handling.bfpp" \
    "$SCRIPT_DIR/expected_errors.txt"

# === New feature tests ===

# T operator (tape address)
run_test "tape_addr" \
    "$SCRIPT_DIR/test_tape_addr.bfpp" \
    "$SCRIPT_DIR/expected_tape_addr.txt"

# Include system
run_test "include" \
    "$SCRIPT_DIR/test_include.bfpp" \
    "$SCRIPT_DIR/expected_include.txt"

# Stdlib math (multiply)
run_test "stdlib_math" \
    "$SCRIPT_DIR/test_stdlib_math.bfpp" \
    "$SCRIPT_DIR/expected_stdlib_math.txt"

# Stdlib I/O (print_string)
run_test "stdlib_io" \
    "$SCRIPT_DIR/test_stdlib_io.bfpp" \
    "$SCRIPT_DIR/expected_stdlib_io.txt"

# Stdlib string (load test)
run_test "stdlib_string" \
    "$SCRIPT_DIR/test_stdlib_string.bfpp" \
    "$SCRIPT_DIR/expected_stdlib_string.txt"

# FFI (call libc abs)
run_test "ffi" \
    "$SCRIPT_DIR/test_ffi.bfpp" \
    "$SCRIPT_DIR/expected_ffi.txt"

# Numeric literals (#N, #0xHH)
run_test "numeric_lit" \
    "$SCRIPT_DIR/test_numeric_lit.bfpp" \
    "$SCRIPT_DIR/expected_numeric_lit.txt"

# Cell width directives (%1, %2, %4, %8)
run_test "cell_width" \
    "$SCRIPT_DIR/test_cell_width.bfpp" \
    "$SCRIPT_DIR/expected_cell_width.txt"

# Block comments (/* */ with nesting)
run_test "block_comments" \
    "$SCRIPT_DIR/test_block_comments.bfpp" \
    "$SCRIPT_DIR/expected_block_comments.txt"

# R{...}K{...} error propagation with ?
run_test "error_propagation" \
    "$SCRIPT_DIR/test_error_propagation.bfpp" \
    "$SCRIPT_DIR/expected_error_propagation.txt"

# Subroutine def/call, return, call depth
run_test "subroutines" \
    "$SCRIPT_DIR/test_subroutines.bfpp" \
    "$SCRIPT_DIR/expected_subroutines.txt"

# Stack ops ($, ~, T)
run_test "stack_ops" \
    "$SCRIPT_DIR/test_stack_ops.bfpp" \
    "$SCRIPT_DIR/expected_stack_ops.txt"

# Compiler intrinsics (__getpid, __sleep, __time_ms, __getenv, __exit)
run_test "intrinsics" \
    "$SCRIPT_DIR/test_intrinsics.bfpp" \
    "$SCRIPT_DIR/expected_intrinsics.txt"

# Bitwise operators (|, &, x, s, r, n)
run_test "bitwise" \
    "$SCRIPT_DIR/test_bitwise.bfpp" \
    "$SCRIPT_DIR/expected_bitwise.txt"

# Optimizer synthetic nodes (clear, increment coalescing, multiply-move, scan)
run_test "optimizer" \
    "$SCRIPT_DIR/test_optimizer.bfpp" \
    "$SCRIPT_DIR/expected_optimizer.txt"

# === v0.4.0 feature tests ===

# Preprocessor macros (!define/!undef) and if/else (?{...}:{...})
run_test "macros_ifelse" \
    "$SCRIPT_DIR/test_macros_ifelse.bfpp" \
    "$SCRIPT_DIR/expected_macros_ifelse.txt"

# Self-hosting intrinsics (__mul, __div, __strcmp, __strlen, __call)
run_test "selfhost_intrinsics" \
    "$SCRIPT_DIR/test_selfhost_intrinsics.bfpp" \
    "$SCRIPT_DIR/expected_selfhost_intrinsics.txt"

# Advanced optimizer passes (constant fold, DCE, inline, conditional eval)
run_test "optimizer_advanced" \
    "$SCRIPT_DIR/test_optimizer_advanced.bfpp" \
    "$SCRIPT_DIR/expected_optimizer_advanced.txt"

# Comprehensive multi-feature test
run_test "comprehensive" \
    "$SCRIPT_DIR/test_comprehensive.bfpp" \
    "$SCRIPT_DIR/expected_comprehensive.txt"

# Classic BF tests (if any exist)
for bf_file in "$SCRIPT_DIR/classic_bf/"*.bfpp; do
    [ -f "$bf_file" ] || continue
    base=$(basename "$bf_file" .bfpp)
    expected_file="$SCRIPT_DIR/classic_bf/expected_${base}.txt"
    if [ -f "$expected_file" ]; then
        run_test "classic_${base}" "$bf_file" "$expected_file"
    fi
done

echo ""
echo "Results: ${PASS} passed, ${FAIL} failed"
if [ $FAIL -gt 0 ]; then
    echo -e "\nFailures:${ERRORS}"
    exit 1
fi
