#!/usr/bin/env bash
# BF++ Benchmark Runner
# Compiles each benchmark at O0, O1, O2 and measures execution time.
# Usage: ./bench_runner.sh [path-to-bfpp-binary]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BFPP="${1:-$SCRIPT_DIR/../target/release/bfpp}"
TMPDIR="/tmp/bfpp_bench_$$"
mkdir -p "$TMPDIR"

# Check for hyperfine
USE_HYPERFINE=false
if command -v hyperfine &>/dev/null; then
    USE_HYPERFINE=true
fi

echo "BF++ Benchmark Suite"
echo "===================="
echo "Compiler: $BFPP"
echo "Timer: $(if $USE_HYPERFINE; then echo 'hyperfine'; else echo 'time (builtin)'; fi)"
echo ""

# Benchmark programs (name : source : description : timeout)
declare -A BENCHMARKS
BENCHMARKS=(
    ["mandelbrot"]="mandelbrot.bfpp:Mandelbrot ASCII renderer:30"
    ["fibonacci"]="fibonacci.bfpp:Fibonacci sequence:10"
    ["prime_sieve"]="prime_sieve.bfpp:Prime sieve to 30:10"
    ["hello_loop"]="hello_loop.bfpp:Hello world x1000:30"
)

run_benchmark() {
    local name="$1"
    local source="$2"
    local timeout="$3"
    local opt_level="$4"
    local bin="$TMPDIR/${name}_O${opt_level}"

    # Compile
    if ! "$BFPP" "$SCRIPT_DIR/$source" -o "$bin" -O "$opt_level" --include "$SCRIPT_DIR/../stdlib" 2>/dev/null; then
        echo "  SKIP: ${name} O${opt_level} — compilation failed"
        return
    fi

    # Run and time
    if $USE_HYPERFINE; then
        hyperfine --warmup 1 --runs 3 --time-unit millisecond \
            --export-json "$TMPDIR/${name}_O${opt_level}.json" \
            "$bin" 2>/dev/null | tail -1
    else
        local start end elapsed
        start=$(date +%s%N)
        timeout "$timeout" "$bin" > /dev/null 2>&1 || true
        end=$(date +%s%N)
        elapsed=$(( (end - start) / 1000000 ))
        printf "  O%d: %d ms\n" "$opt_level" "$elapsed"
    fi
}

# Run all benchmarks
for name in mandelbrot fibonacci prime_sieve hello_loop; do
    IFS=':' read -r source desc timeout <<< "${BENCHMARKS[$name]}"
    echo "--- $name: $desc ---"

    for opt in 0 1 2; do
        run_benchmark "$name" "$source" "$timeout" "$opt"
    done
    echo ""
done

# Summary table
echo "=== Summary ==="
if $USE_HYPERFINE && ls "$TMPDIR"/*.json &>/dev/null; then
    printf "%-15s %10s %10s %10s\n" "Benchmark" "O0 (ms)" "O1 (ms)" "O2 (ms)"
    printf "%-15s %10s %10s %10s\n" "----------" "-------" "-------" "-------"
    for name in mandelbrot fibonacci prime_sieve hello_loop; do
        o0="N/A"; o1="N/A"; o2="N/A"
        for opt in 0 1 2; do
            json="$TMPDIR/${name}_O${opt}.json"
            if [ -f "$json" ]; then
                ms=$(python3 -c "import json; d=json.load(open('$json')); print(f\"{d['results'][0]['mean']*1000:.1f}\")" 2>/dev/null || echo "N/A")
                case $opt in
                    0) o0="$ms" ;;
                    1) o1="$ms" ;;
                    2) o2="$ms" ;;
                esac
            fi
        done
        printf "%-15s %10s %10s %10s\n" "$name" "$o0" "$o1" "$o2"
    done
fi

# Cleanup
rm -rf "$TMPDIR"
echo ""
echo "Done."
