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

    local actual
    actual=$("$tmpbin" 2>&1) || true
    rm -f "$tmpbin"

    local expected_content
    expected_content=$(cat "$expected")

    if [ "$actual" = "$expected_content" ]; then
        PASS=$((PASS + 1))
        echo "  PASS: ${name}"
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  FAIL: ${name}"
        ERRORS="${ERRORS}\n    expected: $(echo "$expected_content" | head -1)"
        ERRORS="${ERRORS}\n    actual:   $(echo "$actual" | head -1)"
    fi
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
