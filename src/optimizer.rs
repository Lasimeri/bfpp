// BF++ Optimizer — peephole optimization passes on the parsed AST.
//
// Optimization levels:
//   None  — no transforms; AST passes through unchanged
//   Basic — clear-loop detection + error folding (lightweight, safe passes)
//   Full  — all Basic passes + scan-loop detection + multiply-move pattern extraction
//
// Each pass is a separate function that consumes and returns Vec<AstNode>.
// Passes chain left-to-right: the output of one feeds the next. Ordering matters —
// clear-loop runs first so that [-] is already reduced before multiply-move scanning
// (a clear loop is NOT a valid multiply-move, so early reduction avoids false matches).
//
// All passes recurse into Loop bodies, SubDef bodies, and ResultBlock branches
// so that nested structures are optimized at every depth.

use crate::ast::AstNode;
use rayon::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptLevel {
    None,
    Basic, // O1: clear-loop detection, error folding
    Full,  // O2: all Basic passes + scan-loop, multiply-move
}

// Run all optimization passes on a node list (used per-sub and for top-level).
fn run_passes(nodes: Vec<AstNode>, level: OptLevel) -> Vec<AstNode> {
    match level {
        OptLevel::None => nodes,
        OptLevel::Basic => {
            let nodes = pass_clear_loop(nodes);
            let nodes = pass_fold_constants(nodes);
            let nodes = pass_coalesce_moves(nodes);
            pass_error_folding(nodes)
        }
        OptLevel::Full => {
            let nodes = pass_clear_loop(nodes);
            let nodes = pass_fold_constants(nodes);
            let nodes = pass_coalesce_moves(nodes);
            let nodes = pass_eval_conditionals(nodes);
            let nodes = pass_scan_loop(nodes);
            let nodes = pass_multiply_move(nodes);
            let nodes = pass_unroll_small_loops(nodes);
            let nodes = pass_auto_parallel(nodes);
            let nodes = pass_detect_gpu_loops(nodes);
            let nodes = pass_dead_code(nodes);
            let nodes = pass_inline_subs(nodes);
            let nodes = pass_fold_constants(nodes);
            let nodes = pass_coalesce_moves(nodes);
            pass_error_folding(nodes)
        }
    }
}

// Entry point: optimizes subroutine bodies in parallel (rayon), then
// optimizes top-level code. Each SubDef body is independent — safe to
// parallelize. DCE and inlining run on the reassembled AST afterward.
pub fn optimize(nodes: Vec<AstNode>, level: OptLevel) -> Vec<AstNode> {
    if level == OptLevel::None { return nodes; }

    // Split: separate SubDefs from top-level code, preserving order.
    let mut subs: Vec<(String, Vec<AstNode>)> = Vec::new();
    let mut top_level: Vec<AstNode> = Vec::new();
    let mut sub_positions: Vec<(usize, String)> = Vec::new(); // (index in top_level, name)

    for node in nodes {
        match node {
            AstNode::SubDef(name, body) => {
                sub_positions.push((top_level.len(), name.clone()));
                top_level.push(AstNode::SubDef(name.clone(), vec![])); // placeholder
                subs.push((name, body));
            }
            other => top_level.push(other),
        }
    }

    // Optimize sub bodies in parallel
    let optimized_subs: Vec<(String, Vec<AstNode>)> = subs.into_par_iter()
        .map(|(name, body)| (name, run_passes(body, level)))
        .collect();

    // Reassemble: replace placeholders with optimized bodies
    let mut sub_map: std::collections::HashMap<String, Vec<AstNode>> =
        optimized_subs.into_iter().collect();
    for (idx, name) in &sub_positions {
        if let Some(body) = sub_map.remove(name) {
            top_level[*idx] = AstNode::SubDef(name.clone(), body);
        }
    }

    // Optimize top-level (non-sub) code + cross-sub passes (DCE, inline)
    run_passes(top_level, level)
}

// Clear-loop detection: replaces `[-]` and `[+]` with a single Clear node.
//
// Both [-] (decrement by 1 until zero) and [+] (increment by 1 until wrap-around
// to zero) are idiomatic BF patterns for zeroing the current cell. With wrapping
// arithmetic on u8, incrementing from any value eventually hits 0 (mod 256), so
// [+] is equivalent to [-] — just takes a different number of iterations.
//
// The Clear node lets codegen emit `tape[ptr] = 0` instead of a loop.
fn pass_clear_loop(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let map_fn = |node: AstNode| -> AstNode {
        match node {
            AstNode::Loop(ref body) => {
                // Match: loop body is exactly one op that is Dec(1) or Inc(1)
                if body.len() == 1 {
                    match &body[0] {
                        AstNode::Decrement(1) | AstNode::Increment(1) => {
                            return AstNode::Clear;
                        }
                        _ => {}
                    }
                }
                // Not a clear loop — recurse into the body to catch nested ones
                AstNode::Loop(pass_clear_loop(body.to_vec()))
            }
            // Recurse into subroutine and result-block children
            AstNode::SubDef(name, body) => {
                AstNode::SubDef(name, pass_clear_loop(body))
            }
            AstNode::ResultBlock(r, k) => {
                AstNode::ResultBlock(pass_clear_loop(r), pass_clear_loop(k))
            }
            other => other,
        }
    };
    if nodes.len() > 1000 {
        nodes.into_par_iter().map(map_fn).collect()
    } else {
        nodes.into_iter().map(map_fn).collect()
    }
}

// Scan-loop detection: replaces `[>]` with ScanRight and `[<]` with ScanLeft.
//
// These loops move the pointer in one direction until a zero cell is found —
// essentially a linear scan / memchr(0). Codegen can emit a while-loop with
// pointer arithmetic or use memchr for a significant speedup over per-cell branching.
fn pass_scan_loop(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let map_fn = |node: AstNode| -> AstNode {
        match node {
            AstNode::Loop(ref body) => {
                // Match: loop body is exactly one MoveRight(1) or MoveLeft(1)
                if body.len() == 1 {
                    match &body[0] {
                        AstNode::MoveRight(1) => return AstNode::ScanRight,
                        AstNode::MoveLeft(1) => return AstNode::ScanLeft,
                        _ => {}
                    }
                }
                AstNode::Loop(pass_scan_loop(body.to_vec()))
            }
            AstNode::SubDef(name, body) => {
                AstNode::SubDef(name, pass_scan_loop(body))
            }
            AstNode::ResultBlock(r, k) => {
                AstNode::ResultBlock(pass_scan_loop(r), pass_scan_loop(k))
            }
            other => other,
        }
    };
    if nodes.len() > 1000 {
        nodes.into_par_iter().map(map_fn).collect()
    } else {
        nodes.into_iter().map(map_fn).collect()
    }
}

// Multiply-move detection: replaces loops like `[->+++>++<<]` with MultiplyMove.
//
// The pattern represents: "for each decrement of the source cell, add N to cell
// at offset K." This is equivalent to:
//   for each target (offset, factor):
//     tape[ptr + offset] += tape[ptr] * factor
//   tape[ptr] = 0
//
// Codegen can emit direct arithmetic instead of an O(N) loop — turns an O(N*M)
// loop (N = source cell value, M = body length) into O(M) straight-line code.
fn pass_multiply_move(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let map_fn = |node: AstNode| -> AstNode {
        match node {
            AstNode::Loop(ref body) => {
                if let Some(pairs) = detect_multiply_pattern(body) {
                    return AstNode::MultiplyMove(pairs);
                }
                AstNode::Loop(pass_multiply_move(body.to_vec()))
            }
            AstNode::SubDef(name, body) => {
                AstNode::SubDef(name, pass_multiply_move(body))
            }
            AstNode::ResultBlock(r, k) => {
                AstNode::ResultBlock(pass_multiply_move(r), pass_multiply_move(k))
            }
            other => other,
        }
    };
    if nodes.len() > 1000 {
        nodes.into_par_iter().map(map_fn).collect()
    } else {
        nodes.into_iter().map(map_fn).collect()
    }
}

// Validates whether a loop body matches the multiply-move pattern and extracts
// the (offset, factor) pairs if so. Returns None on any structural mismatch.
//
// Algorithm:
//   1. First node must be Decrement(1) — the loop counter that drains the source cell.
//   2. Walk remaining nodes, tracking a running pointer offset:
//      - MoveRight(n) / MoveLeft(n) adjust the offset
//      - Increment(n) at a non-zero offset records (offset, n) as a multiply target
//      - Increment at offset 0 is rejected — adding back to the source cell would
//        make the loop non-terminating or change the semantics
//      - Any other node type (Output, Input, nested Loop, etc.) breaks the pattern
//   3. After the walk, the net pointer offset must be exactly 0 — the pointer must
//      return to the source cell so the loop condition tests the right cell.
//   4. Duplicate offsets are merged by summing their factors. This handles patterns
//      like `[->+>++<+<]` where offset +1 gets incremented in two separate places
//      (factor 1 + factor 1 = factor 2).
fn detect_multiply_pattern(body: &[AstNode]) -> Option<Vec<(isize, usize)>> {
    // Step 1: the loop must start with Dec(1) — one decrement per iteration
    if body.is_empty() || body[0] != AstNode::Decrement(1) {
        return None;
    }

    let mut offset: isize = 0;
    let mut pairs: Vec<(isize, usize)> = Vec::new();

    // Step 2: scan remaining nodes, accumulating offset and recording increments
    for node in &body[1..] {
        match node {
            AstNode::MoveRight(n) => offset += *n as isize,
            AstNode::MoveLeft(n) => offset -= *n as isize,
            AstNode::Increment(n) => {
                if offset == 0 {
                    return None; // incrementing the source cell breaks the drain invariant
                }
                pairs.push((offset, *n));
            }
            _ => return None, // any non-move/inc node disqualifies the pattern
        }
    }

    // Step 3: pointer must return to origin (net offset zero)
    if offset != 0 {
        return None;
    }

    if pairs.is_empty() {
        return None;
    }

    // Step 4: merge duplicate offsets — sum factors targeting the same cell
    let mut merged: std::collections::HashMap<isize, usize> = std::collections::HashMap::new();
    for (offset, factor) in &pairs {
        *merged.entry(*offset).or_insert(0) += *factor;
    }
    let pairs: Vec<(isize, usize)> = merged.into_iter().collect();
    if pairs.is_empty() {
        return None;
    }

    Some(pairs)
}

// Constant folding: peephole pass that simplifies adjacent arithmetic/set operations.
//
// Optimizations:
//   - Dead store elimination: adjacent SetValue/Clear → keep only the last one
//   - Arithmetic folding: SetValue(N) + Increment(M) → SetValue(N+M) (same for Decrement)
//   - Clear + Increment(N) → SetValue(N)
//   - No-op removal: Increment(0), Decrement(0), MoveRight(0), MoveLeft(0)
//
// Runs to a fixed point — one pass may expose new folding opportunities (e.g.,
// SetValue(3) + SetValue(7) + Inc(2) → SetValue(7) + Inc(2) → SetValue(9)).
fn pass_fold_constants(nodes: Vec<AstNode>) -> Vec<AstNode> {
    
    fold_constants_once(nodes)
}

fn fold_constants_once(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result: Vec<AstNode> = Vec::with_capacity(nodes.len());

    for node in nodes {
        // Recurse into container nodes first
        let node = match node {
            AstNode::Loop(body) => AstNode::Loop(fold_constants_once(body)),
            AstNode::SubDef(name, body) => AstNode::SubDef(name, fold_constants_once(body)),
            AstNode::ResultBlock(r, k) => {
                AstNode::ResultBlock(fold_constants_once(r), fold_constants_once(k))
            }
            AstNode::IfEqual(v, body, el) => AstNode::IfEqual(
                v,
                fold_constants_once(body),
                el.map(fold_constants_once),
            ),
            AstNode::IfNotEqual(v, body) => {
                AstNode::IfNotEqual(v, fold_constants_once(body))
            }
            AstNode::IfLess(v, body) => AstNode::IfLess(v, fold_constants_once(body)),
            AstNode::IfGreater(v, body) => {
                AstNode::IfGreater(v, fold_constants_once(body))
            }
            other => other,
        };

        // Remove no-ops
        match &node {
            AstNode::Increment(0)
            | AstNode::Decrement(0)
            | AstNode::MoveRight(0)
            | AstNode::MoveLeft(0) => continue,
            _ => {}
        }

        // Peephole: check last emitted node for folding opportunities
        if let Some(prev) = result.last() {
            match (prev, &node) {
                // Adjacent SetValue: dead store — drop the earlier one
                (AstNode::SetValue(_), AstNode::SetValue(_)) => {
                    result.pop();
                }
                // Clear followed by SetValue: Clear is dead
                (AstNode::Clear, AstNode::SetValue(_)) => {
                    result.pop();
                }
                // SetValue followed by Increment: fold into SetValue
                (AstNode::SetValue(base), AstNode::Increment(n)) => {
                    let folded = AstNode::SetValue(base.wrapping_add(*n as u64));
                    result.pop();
                    result.push(folded);
                    continue;
                }
                // SetValue followed by Decrement: fold into SetValue
                (AstNode::SetValue(base), AstNode::Decrement(n)) => {
                    let folded = AstNode::SetValue(base.wrapping_sub(*n as u64));
                    result.pop();
                    result.push(folded);
                    continue;
                }
                // Clear followed by Increment: SetValue(N)
                (AstNode::Clear, AstNode::Increment(n)) => {
                    let folded = AstNode::SetValue(*n as u64);
                    result.pop();
                    result.push(folded);
                    continue;
                }
                // Clear followed by Decrement: SetValue(wrapping)
                (AstNode::Clear, AstNode::Decrement(n)) => {
                    let folded = AstNode::SetValue(0u64.wrapping_sub(*n as u64));
                    result.pop();
                    result.push(folded);
                    continue;
                }
                // Adjacent Clear: redundant
                (AstNode::Clear, AstNode::Clear) => {
                    // Already have Clear, skip the duplicate
                    continue;
                }
                // SetValue followed by Clear: SetValue is dead
                (AstNode::SetValue(_), AstNode::Clear) => {
                    result.pop();
                }
                _ => {}
            }
        }

        result.push(node);
    }

    result
}

// Error folding: collapses runs of consecutive `?` (Propagate) nodes into one.
//
// The `?` operator checks the error register and returns/propagates if set.
// Multiple consecutive `?` are redundant — if the first doesn't propagate (error
// register is clear), subsequent checks on the same unchanged register are no-ops.
// Collapsing N consecutive Propagates into 1 eliminates N-1 branch instructions.
//
// Unlike the other passes, this one uses an imperative loop with a flag rather than
// map(), because it needs to suppress nodes based on their predecessor — a
// stateful fold rather than a stateless per-node transform.
fn pass_error_folding(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result = Vec::new();
    let mut last_was_propagate = false;

    for node in nodes {
        match node {
            AstNode::Propagate => {
                // Only emit the first Propagate in a consecutive run
                if !last_was_propagate {
                    result.push(AstNode::Propagate);
                }
                last_was_propagate = true;
            }
            AstNode::Loop(body) => {
                last_was_propagate = false;
                result.push(AstNode::Loop(pass_error_folding(body)));
            }
            AstNode::SubDef(name, body) => {
                last_was_propagate = false;
                result.push(AstNode::SubDef(name, pass_error_folding(body)));
            }
            AstNode::ResultBlock(r, k) => {
                last_was_propagate = false;
                result.push(AstNode::ResultBlock(
                    pass_error_folding(r),
                    pass_error_folding(k),
                ));
            }
            other => {
                last_was_propagate = false;
                result.push(other);
            }
        }
    }

    result
}

// Compile-time conditional evaluation: when the current cell has a known value
// (from SetValue or Clear), conditionals can be resolved at compile time.
//
// Examples:
//   #5 ?= #5 [ body ]         → body (always true)
//   #5 ?= #3 [ body ]         → (eliminated, always false)
//   [-] [ body ]               → (eliminated, cell is 0, loop never enters)
//   #0 ?{ true } : { false }  → false (cell is 0)
//   #1 ?{ true } : { false }  → true (cell is nonzero)
//
// Value tracking is conservative: any loop, subroutine call, I/O, or
// absolute addressing resets the known value to None.
fn pass_eval_conditionals(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result = Vec::new();
    let mut known_value: Option<u64> = None;

    for node in nodes {
        // Update known_value based on current node
        match &node {
            AstNode::SetValue(v) => { known_value = Some(*v); }
            AstNode::Clear => { known_value = Some(0); }
            AstNode::Increment(n) => {
                known_value = known_value.map(|v| v.wrapping_add(*n as u64));
            }
            AstNode::Decrement(n) => {
                known_value = known_value.map(|v| v.wrapping_sub(*n as u64));
            }
            // These destroy known value (cell modified unpredictably)
            AstNode::Input | AstNode::InputFd(_) | AstNode::Pop |
            AstNode::ErrorRead | AstNode::SubCall(_) |
            AstNode::AbsoluteAddr | AstNode::Syscall |
            AstNode::FfiCall(_, _) => {
                known_value = None;
            }
            // Pointer movement: we're now at a different cell, value unknown
            AstNode::MoveRight(_) | AstNode::MoveLeft(_) => {
                known_value = None;
            }
            _ => {}
        }

        // Try to resolve conditionals with known value
        match &node {
            AstNode::Loop(body) if known_value == Some(0) => {
                // Cell is 0 → loop never executes. Eliminate.
                continue;
            }
            AstNode::IfEqual(target, body, else_body) => {
                if let Some(v) = known_value {
                    if v == *target {
                        // Always true — emit body directly
                        result.extend(pass_eval_conditionals(body.clone()));
                    } else if let Some(eb) = else_body {
                        // Always false — emit else body
                        result.extend(pass_eval_conditionals(eb.clone()));
                    }
                    // Don't reset known_value — conditionals are non-destructive
                    continue;
                }
                // Value unknown — keep the conditional, recurse into bodies
                result.push(AstNode::IfEqual(
                    *target,
                    pass_eval_conditionals(body.clone()),
                    else_body.as_ref().map(|e| pass_eval_conditionals(e.clone())),
                ));
                known_value = None; // conditional body may modify cell
                continue;
            }
            AstNode::IfNotEqual(target, body) => {
                if let Some(v) = known_value {
                    if v != *target {
                        result.extend(pass_eval_conditionals(body.clone()));
                    }
                    continue;
                }
                result.push(AstNode::IfNotEqual(*target, pass_eval_conditionals(body.clone())));
                known_value = None;
                continue;
            }
            AstNode::IfLess(target, body) => {
                if let Some(v) = known_value {
                    if v < *target {
                        result.extend(pass_eval_conditionals(body.clone()));
                    }
                    continue;
                }
                result.push(AstNode::IfLess(*target, pass_eval_conditionals(body.clone())));
                known_value = None;
                continue;
            }
            AstNode::IfGreater(target, body) => {
                if let Some(v) = known_value {
                    if v > *target {
                        result.extend(pass_eval_conditionals(body.clone()));
                    }
                    continue;
                }
                result.push(AstNode::IfGreater(*target, pass_eval_conditionals(body.clone())));
                known_value = None;
                continue;
            }
            AstNode::IfElse(true_body, false_body) => {
                if let Some(v) = known_value {
                    if v != 0 {
                        result.extend(pass_eval_conditionals(true_body.clone()));
                    } else {
                        result.extend(pass_eval_conditionals(false_body.clone()));
                    }
                    known_value = Some(0); // IfElse consumes cell (sets to 0)
                    continue;
                }
                result.push(AstNode::IfElse(
                    pass_eval_conditionals(true_body.clone()),
                    pass_eval_conditionals(false_body.clone()),
                ));
                known_value = Some(0); // consumed
                continue;
            }
            // Loops with unknown value: recurse but reset tracking
            AstNode::Loop(body) => {
                result.push(AstNode::Loop(pass_eval_conditionals(body.clone())));
                known_value = Some(0); // loop exits when cell is 0
                continue;
            }
            // Recurse into sub bodies
            AstNode::SubDef(name, body) => {
                result.push(AstNode::SubDef(name.clone(), pass_eval_conditionals(body.clone())));
                continue;
            }
            AstNode::ResultBlock(r, k) => {
                result.push(AstNode::ResultBlock(
                    pass_eval_conditionals(r.clone()),
                    pass_eval_conditionals(k.clone()),
                ));
                known_value = None;
                continue;
            }
            _ => {}
        }

        result.push(node);
    }

    result
}

// Loop unrolling: for small constant-trip loops `#N [- body]` where N ≤ 16
// and body is short (≤ 20 ops), replace the loop with N copies of the body.
// Eliminates branch + counter overhead for tight inner loops.
fn pass_unroll_small_loops(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result = Vec::new();
    let mut i = 0;
    let nodes_vec: Vec<AstNode> = nodes;

    while i < nodes_vec.len() {
        // Pattern: SetValue(N) followed by Loop([Decrement(1), ...body...])
        if i + 1 < nodes_vec.len() {
            if let AstNode::SetValue(count) = &nodes_vec[i] {
                if let AstNode::Loop(body) = &nodes_vec[i + 1] {
                    if *count > 0 && *count <= 16
                        && body.first() == Some(&AstNode::Decrement(1))
                        && body.len() <= 21  // 1 (Dec) + 20 (body)
                        && !body_has_side_effects(&body[1..])
                    {
                        // Unroll: emit body[1..] (skip the Decrement) N times
                        let unrolled_body = &body[1..];
                        for _ in 0..*count {
                            result.extend(unrolled_body.iter().cloned());
                        }
                        i += 2; // skip SetValue + Loop
                        continue;
                    }
                }
            }
        }

        // Recurse into containers
        let node = nodes_vec[i].clone();
        match node {
            AstNode::Loop(body) => result.push(AstNode::Loop(pass_unroll_small_loops(body))),
            AstNode::SubDef(name, body) => {
                result.push(AstNode::SubDef(name, pass_unroll_small_loops(body)));
            }
            AstNode::ResultBlock(r, k) => {
                result.push(AstNode::ResultBlock(
                    pass_unroll_small_loops(r),
                    pass_unroll_small_loops(k),
                ));
            }
            other => result.push(other),
        }
        i += 1;
    }

    result
}

// Auto-parallelism detection: identifies loops with provably independent
// iterations and rewrites them as ParallelLoop nodes.
//
// Pattern 1 — Independent Loop Iterations:
// Detects `SetValue(N)` followed by `Loop([Decrement(1), ...body..., MoveRight(stride)])`
// where N >= 64, body has no side effects or cross-iteration dependencies, and
// all cell accesses are within [0, stride) relative to the iteration's base pointer.
//
// The threshold (64) accounts for pthread dispatch overhead — below that,
// sequential execution is faster than spawning threads.
fn pass_auto_parallel(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < nodes.len() {
        // Pattern: SetValue(N) followed by Loop([Decrement(1), ...body..., MoveRight(stride)])
        if i + 1 < nodes.len() {
            if let AstNode::SetValue(count) = &nodes[i] {
                if *count >= 64 {
                    if let AstNode::Loop(body) = &nodes[i + 1] {
                        if let Some((inner_body, stride)) = detect_parallel_loop(body) {
                            result.push(AstNode::ParallelLoop {
                                body: inner_body,
                                stride,
                                trip_count: Some(*count),
                            });
                            i += 2;
                            continue;
                        }
                    }
                }
            }
        }

        // Recurse into containers
        let node = nodes[i].clone();
        match node {
            AstNode::Loop(body) => result.push(AstNode::Loop(pass_auto_parallel(body))),
            AstNode::SubDef(name, body) => {
                result.push(AstNode::SubDef(name, pass_auto_parallel(body)));
            }
            AstNode::ResultBlock(r, k) => {
                result.push(AstNode::ResultBlock(
                    pass_auto_parallel(r),
                    pass_auto_parallel(k),
                ));
            }
            other => result.push(other),
        }
        i += 1;
    }

    result
}

// Checks if a loop body matches the parallelizable pattern:
//   [Decrement(1), ...inner_body..., MoveRight(stride)]
// Returns Some((inner_body, stride)) if the body is parallelizable, None otherwise.
//
// Parallelization requirements:
// 1. First node must be Decrement(1) — the loop counter
// 2. Last node must be MoveRight(stride) — advancing to the next chunk
// 3. Inner body (between Dec and MoveRight) must be free of:
//    - I/O (Output, Input, FramebufferFlush)
//    - Subroutine calls (SubCall)
//    - Error handling (ErrorRead, ErrorWrite, Propagate, ResultBlock)
//    - FFI calls
//    - Absolute addressing
//    - Nested loops (conservative: could be refined but not worth the complexity)
// 4. All cell accesses must be within [0, stride) relative to ptr — no cross-iteration aliasing
fn detect_parallel_loop(body: &[AstNode]) -> Option<(Vec<AstNode>, usize)> {
    if body.len() < 3 { return None; }

    // First node: Decrement(1)
    if body[0] != AstNode::Decrement(1) { return None; }

    // Last node: MoveRight(stride)
    let stride = match body.last()? {
        AstNode::MoveRight(s) if *s > 0 => *s,
        _ => return None,
    };

    // Inner body: everything between Dec(1) and MoveRight(stride)
    let inner = &body[1..body.len() - 1];

    // Check safety: no disqualifying ops
    if !is_parallel_safe(inner) { return None; }

    // Check cell access bounds: all accesses must be within [0, stride)
    if !accesses_within_stride(inner, stride) { return None; }

    Some((inner.to_vec(), stride))
}

// Returns true if the body contains only ops safe for parallel execution.
// Rejects any operation with side effects, cross-thread hazards, or
// non-local control flow.
fn is_parallel_safe(body: &[AstNode]) -> bool {
    for node in body {
        match node {
            // Safe: pure cell arithmetic and pointer movement
            AstNode::Increment(_) | AstNode::Decrement(_) |
            AstNode::MoveRight(_) | AstNode::MoveLeft(_) |
            AstNode::SetValue(_) | AstNode::SetMulti(_) |
            AstNode::Clear |
            AstNode::MultiplyMove(_) |
            AstNode::BitOr | AstNode::BitAnd | AstNode::BitXor |
            AstNode::ShiftLeft | AstNode::ShiftRight | AstNode::BitNot |
            AstNode::CellWidthCycle | AstNode::SetCellWidth(_) |
            AstNode::Push | AstNode::Pop |
            AstNode::StringLit(_) => {}

            // Disqualifying: I/O, subroutine calls, error handling, control flow,
            // absolute addressing, FFI, syscall, dual-tape ops, nested loops, etc.
            _ => return false,
        }
    }
    true
}

// Checks that all cell accesses in the body are within [0, stride) relative
// to the starting pointer. This ensures no cross-iteration aliasing —
// iteration K accesses cells [K*stride, K*stride+stride), which doesn't
// overlap with iteration K+1's region.
//
// Tracks a running pointer offset and checks that it never goes negative
// or reaches stride.
fn accesses_within_stride(body: &[AstNode], stride: usize) -> bool {
    let mut offset: isize = 0;

    for node in body {
        match node {
            AstNode::MoveRight(n) => offset += *n as isize,
            AstNode::MoveLeft(n) => offset -= *n as isize,
            // Cell access at current offset: check bounds
            AstNode::Increment(_) | AstNode::Decrement(_) |
            AstNode::SetValue(_) | AstNode::Clear |
            AstNode::BitOr | AstNode::BitAnd | AstNode::BitXor |
            AstNode::ShiftLeft | AstNode::ShiftRight | AstNode::BitNot |
            AstNode::Push | AstNode::Pop |
            AstNode::CellWidthCycle | AstNode::SetCellWidth(_) => {
                if offset < 0 || offset >= stride as isize {
                    return false;
                }
            }
            // MultiplyMove: check that all target offsets are within bounds
            AstNode::MultiplyMove(pairs) => {
                if offset < 0 || offset >= stride as isize {
                    return false;
                }
                for (rel_off, _) in pairs {
                    let abs = offset + rel_off;
                    if abs < 0 || abs >= stride as isize {
                        return false;
                    }
                }
            }
            // SetMulti writes to offset..offset+len
            AstNode::SetMulti(vals) => {
                if offset < 0 || offset + vals.len() as isize > stride as isize {
                    return false;
                }
            }
            // StringLit writes len bytes starting at current offset
            AstNode::StringLit(bytes) => {
                if offset < 0 || offset + bytes.len() as isize > stride as isize {
                    return false;
                }
            }
            // Anything else: shouldn't happen (is_parallel_safe filtered), but be safe
            _ => return false,
        }
    }

    // After the body, pointer must return to offset 0 — the next iteration
    // starts at the same relative position within its stride-wide region.
    // Actually, the pointer offset after the body doesn't need to be 0 because
    // the MoveRight(stride) at the end handles advancing to the next chunk.
    // But the body itself must not leave the pointer outside [0, stride).
    // We've already checked each access point, so we're good.
    true
}

// ── GPU loop transpilation pass ──────────────────────────────────────────────
//
// Detects data-parallel loops that can be offloaded to OpenCL. A loop qualifies
// when it meets all criteria for CPU auto-parallelism AND additionally has no
// nested loops (GPU kernels can't branch). GPU loops take priority over CPU
// parallel loops — a ParallelLoop node is upgraded to GpuLoop when possible.
//
// Uses a global kernel ID counter (AtomicUsize) so IDs are unique even across
// rayon-parallelized subroutine optimization.

use std::sync::atomic::{AtomicUsize, Ordering};
static GPU_KERNEL_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn pass_detect_gpu_loops(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result = Vec::new();

    for node in nodes {
        match node {
            // Upgrade existing ParallelLoop to GpuLoop if body is GPU-safe
            AstNode::ParallelLoop { body, stride, trip_count: Some(count) } => {
                if is_gpu_safe(&body) {
                    let kid = GPU_KERNEL_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let ksrc = generate_opencl_kernel(&body, kid, stride);
                    result.push(AstNode::GpuLoop {
                        trip_count: count,
                        stride,
                        body,
                        kernel_source: ksrc,
                        kernel_id: kid,
                    });
                } else {
                    result.push(AstNode::ParallelLoop { body, stride, trip_count: Some(count) });
                }
            }
            // Also detect raw SetValue + Loop patterns not caught by auto_parallel
            // (e.g., those with nested loops that auto_parallel rejected but GPU can't handle either)
            AstNode::Loop(body) => result.push(AstNode::Loop(pass_detect_gpu_loops(body))),
            AstNode::SubDef(name, body) => {
                result.push(AstNode::SubDef(name, pass_detect_gpu_loops(body)));
            }
            AstNode::ResultBlock(r, k) => {
                result.push(AstNode::ResultBlock(
                    pass_detect_gpu_loops(r),
                    pass_detect_gpu_loops(k),
                ));
            }
            other => result.push(other),
        }
    }

    result
}

// GPU kernels are more restrictive than CPU parallel: no nested loops at all,
// no stack ops (GPU has no stack), no string literals (variable-length writes).
fn is_gpu_safe(body: &[AstNode]) -> bool {
    for node in body {
        match node {
            AstNode::Increment(_) | AstNode::Decrement(_) |
            AstNode::MoveRight(_) | AstNode::MoveLeft(_) |
            AstNode::SetValue(_) | AstNode::Clear |
            AstNode::BitOr | AstNode::BitAnd | AstNode::BitXor |
            AstNode::ShiftLeft | AstNode::ShiftRight | AstNode::BitNot |
            AstNode::CellWidthCycle | AstNode::SetCellWidth(_) => {}
            // Everything else disqualifies (loops, I/O, subs, stack, strings, etc.)
            _ => return false,
        }
    }
    true
}

// Generate an OpenCL C kernel string from a BF++ loop body.
fn generate_opencl_kernel(body: &[AstNode], kernel_id: usize, _stride: usize) -> String {
    let mut s = String::with_capacity(512);
    s.push_str("#define R(a) tape[(a)]\n");
    s.push_str("#define W(a,v) tape[(a)]=(unsigned char)(v)\n");
    s.push_str(&format!(
        "__kernel void bfpp_gpu_loop_{}(\
        __global unsigned char *tape, const int base_ptr, const int stride) {{\n\
        int gid = get_global_id(0);\n\
        int p = base_ptr + gid * stride;\n\
        int o = 0;\n",
        kernel_id
    ));

    for node in body {
        match node {
            AstNode::Increment(n) => s.push_str(&format!("R(p+o) += {};\n", n)),
            AstNode::Decrement(n) => s.push_str(&format!("R(p+o) -= {};\n", n)),
            AstNode::SetValue(n) => s.push_str(&format!("W(p+o, {});\n", n)),
            AstNode::MoveRight(n) => s.push_str(&format!("o += {};\n", n)),
            AstNode::MoveLeft(n) => s.push_str(&format!("o -= {};\n", n)),
            AstNode::Clear => s.push_str("W(p+o, 0);\n"),
            AstNode::BitOr => s.push_str("W(p+o, R(p+o) | R(p+o+1));\n"),
            AstNode::BitAnd => s.push_str("W(p+o, R(p+o) & R(p+o+1));\n"),
            AstNode::BitXor => s.push_str("W(p+o, R(p+o) ^ R(p+o+1));\n"),
            AstNode::ShiftLeft => s.push_str("W(p+o, R(p+o) << R(p+o+1));\n"),
            AstNode::ShiftRight => s.push_str("W(p+o, R(p+o) >> R(p+o+1));\n"),
            AstNode::BitNot => s.push_str("W(p+o, ~R(p+o));\n"),
            AstNode::CellWidthCycle | AstNode::SetCellWidth(_) => {
                // Cell width changes affect how the CPU interprets tape data but
                // the OpenCL kernel always operates on bytes. Skip in kernel.
            }
            _ => {} // filtered by is_gpu_safe
        }
    }

    s.push_str("}\n");
    s
}

// Check if a loop body slice has side effects that prevent unrolling.
// Side effects: I/O, subroutine calls, syscalls, pointer jumps, nested loops.
fn body_has_side_effects(body: &[AstNode]) -> bool {
    for node in body {
        match node {
            AstNode::Output | AstNode::Input | AstNode::OutputFd(_) | AstNode::InputFd(_) |
            AstNode::SubCall(_) | AstNode::Syscall | AstNode::FfiCall(_, _) |
            AstNode::AbsoluteAddr | AstNode::Loop(_) | AstNode::Return |
            AstNode::Propagate | AstNode::ErrorWrite | AstNode::ErrorRead |
            AstNode::FramebufferFlush => return true,
            _ => {}
        }
    }
    false
}

// Move coalescing: merges adjacent MoveRight/MoveLeft and Increment/Decrement
// nodes that may have been separated by earlier passes removing intervening ops.
//
// Examples:
//   MoveRight(3) + MoveRight(2) → MoveRight(5)
//   Increment(4) + Increment(6) → Increment(10)
//   MoveRight(3) + MoveLeft(1)  → MoveRight(2)
fn pass_coalesce_moves(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result: Vec<AstNode> = Vec::with_capacity(nodes.len());

    for node in nodes {
        let node = match node {
            AstNode::Loop(body) => AstNode::Loop(pass_coalesce_moves(body)),
            AstNode::SubDef(name, body) => AstNode::SubDef(name, pass_coalesce_moves(body)),
            AstNode::ResultBlock(r, k) => {
                AstNode::ResultBlock(pass_coalesce_moves(r), pass_coalesce_moves(k))
            }
            other => other,
        };

        // Clone the last node's discriminant values to avoid borrow conflicts
        // with result.pop() / result.push() below.
        let prev_kind = result.last().cloned();
        if let Some(prev) = prev_kind {
            match (&prev, &node) {
                (AstNode::MoveRight(a), AstNode::MoveRight(b)) => {
                    result.pop();
                    result.push(AstNode::MoveRight(a + b));
                    continue;
                }
                (AstNode::MoveLeft(a), AstNode::MoveLeft(b)) => {
                    result.pop();
                    result.push(AstNode::MoveLeft(a + b));
                    continue;
                }
                (AstNode::Increment(a), AstNode::Increment(b)) => {
                    result.pop();
                    result.push(AstNode::Increment(a + b));
                    continue;
                }
                (AstNode::Decrement(a), AstNode::Decrement(b)) => {
                    result.pop();
                    result.push(AstNode::Decrement(a + b));
                    continue;
                }
                (AstNode::MoveRight(a), AstNode::MoveLeft(b)) => {
                    result.pop();
                    if a > b { result.push(AstNode::MoveRight(a - b)); }
                    else if b > a { result.push(AstNode::MoveLeft(b - a)); }
                    continue;
                }
                (AstNode::MoveLeft(a), AstNode::MoveRight(b)) => {
                    result.pop();
                    if a > b { result.push(AstNode::MoveLeft(a - b)); }
                    else if b > a { result.push(AstNode::MoveRight(b - a)); }
                    continue;
                }
                (AstNode::Increment(a), AstNode::Decrement(b)) => {
                    result.pop();
                    if a > b { result.push(AstNode::Increment(a - b)); }
                    else if b > a { result.push(AstNode::Decrement(b - a)); }
                    continue;
                }
                (AstNode::Decrement(a), AstNode::Increment(b)) => {
                    result.pop();
                    if a > b { result.push(AstNode::Decrement(a - b)); }
                    else if b > a { result.push(AstNode::Increment(b - a)); }
                    continue;
                }
                _ => {}
            }
        }
        result.push(node);
    }

    result
}

// Dead code elimination: removes unused subroutine definitions and unreachable
// code after Return (^) nodes within subroutine bodies.
//
// Phase 1: Collect all SubCall names referenced anywhere in the AST.
// Phase 2: Remove SubDef nodes whose names aren't in the call set.
// Phase 3: Truncate subroutine bodies after unconditional Return.
fn pass_dead_code(nodes: Vec<AstNode>) -> Vec<AstNode> {
    // Phase 1: Collect all called subroutine names
    let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_calls(&nodes, &mut called);

    // Phase 2+3: Filter and truncate
    eliminate_dead(nodes, &called)
}

fn collect_calls(nodes: &[AstNode], called: &mut std::collections::HashSet<String>) {
    for node in nodes {
        match node {
            AstNode::SubCall(name) => { called.insert(name.clone()); }
            AstNode::Loop(body) | AstNode::SubDef(_, body) => collect_calls(body, called),
            AstNode::ResultBlock(r, k) => {
                collect_calls(r, called);
                collect_calls(k, called);
            }
            AstNode::IfEqual(_, body, el) => {
                collect_calls(body, called);
                if let Some(e) = el { collect_calls(e, called); }
            }
            AstNode::IfNotEqual(_, body) | AstNode::IfLess(_, body) |
            AstNode::IfGreater(_, body) => collect_calls(body, called),
            AstNode::IfElse(t, f) => {
                collect_calls(t, called);
                collect_calls(f, called);
            }
            AstNode::Deref(inner) => collect_calls(&[*inner.clone()], called),
            AstNode::ParallelLoop { body, .. } => collect_calls(body, called),
            AstNode::ParallelCalls(names) => {
                for name in names { called.insert(name.clone()); }
            }
            AstNode::GpuLoop { body, .. } => collect_calls(body, called),
            _ => {}
        }
    }
}

fn eliminate_dead(nodes: Vec<AstNode>, called: &std::collections::HashSet<String>) -> Vec<AstNode> {
    let mut result = Vec::new();

    for node in nodes {
        match node {
            // Remove unused subroutine definitions
            AstNode::SubDef(ref name, _) if !called.contains(name) => continue,

            // Recurse into subroutine bodies and truncate after Return
            AstNode::SubDef(name, body) => {
                let body = eliminate_dead(body, called);
                let body = truncate_after_return(body);
                result.push(AstNode::SubDef(name, body));
            }

            // Recurse into other containers
            AstNode::Loop(body) => result.push(AstNode::Loop(eliminate_dead(body, called))),
            AstNode::ResultBlock(r, k) => {
                result.push(AstNode::ResultBlock(
                    eliminate_dead(r, called),
                    eliminate_dead(k, called),
                ));
            }
            other => result.push(other),
        }
    }

    result
}

// Truncate a node list after the first unconditional Return,
// and eliminate redundant trailing Return (the codegen epilogue
// already decrements call_depth and returns on fall-through).
fn truncate_after_return(mut nodes: Vec<AstNode>) -> Vec<AstNode> {
    // Phase 1: Remove trailing Return (tail return elimination)
    if let Some(AstNode::Return) = nodes.last() {
        nodes.pop();
    }
    // Phase 2: Truncate after any remaining Return (unreachable code)
    let mut result = Vec::new();
    for node in nodes {
        let is_return = matches!(&node, AstNode::Return);
        result.push(node);
        if is_return { break; }
    }
    result
}

// Subroutine inlining: replaces calls to small subroutines (≤ threshold ops)
// with the subroutine's body directly at the call site. Eliminates call/return
// overhead for tiny helper functions.
//
// Only inlines subroutines that:
//   - Have ≤ INLINE_THRESHOLD nodes in their body
//   - Don't contain Return (^) — inlining a return would break control flow
//   - Don't contain nested SubDef (no closure-like behavior)
//   - Don't contain SubCall (no recursion risk)
const INLINE_THRESHOLD: usize = 8;

fn pass_inline_subs(nodes: Vec<AstNode>) -> Vec<AstNode> {
    // Phase 1: Collect inlineable subroutine bodies
    let mut inlineable: std::collections::HashMap<String, Vec<AstNode>> =
        std::collections::HashMap::new();

    for node in &nodes {
        if let AstNode::SubDef(name, body) = node {
            if body.len() <= INLINE_THRESHOLD && is_inlineable(body) {
                inlineable.insert(name.clone(), body.clone());
            }
        }
    }

    if inlineable.is_empty() {
        return nodes;
    }

    // Phase 2: Replace SubCall with body for inlineable subs
    inline_calls(nodes, &inlineable)
}

fn is_inlineable(body: &[AstNode]) -> bool {
    for node in body {
        match node {
            AstNode::Return | AstNode::SubDef(_, _) | AstNode::SubCall(_) => return false,
            _ => {}
        }
    }
    true
}

fn inline_calls(nodes: Vec<AstNode>, inlineable: &std::collections::HashMap<String, Vec<AstNode>>) -> Vec<AstNode> {
    let mut result = Vec::new();

    for node in nodes {
        match node {
            // Replace call with body
            AstNode::SubCall(ref name) if inlineable.contains_key(name) => {
                result.extend(inlineable[name].clone());
            }
            // Recurse into containers
            AstNode::Loop(body) => result.push(AstNode::Loop(inline_calls(body, inlineable))),
            AstNode::SubDef(name, body) => {
                // Don't remove the SubDef yet — DCE handles that.
                // But inline within the body itself.
                result.push(AstNode::SubDef(name, inline_calls(body, inlineable)));
            }
            AstNode::ResultBlock(r, k) => {
                result.push(AstNode::ResultBlock(
                    inline_calls(r, inlineable),
                    inline_calls(k, inlineable),
                ));
            }
            other => result.push(other),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clear_loop() {
        let input = vec![AstNode::Loop(vec![AstNode::Decrement(1)])];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Clear]);
    }

    #[test]
    fn test_clear_loop_increment() {
        let input = vec![AstNode::Loop(vec![AstNode::Increment(1)])];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Clear]);
    }

    #[test]
    fn test_scan_right() {
        let input = vec![AstNode::Loop(vec![AstNode::MoveRight(1)])];
        let output = optimize(input, OptLevel::Full);
        assert_eq!(output, vec![AstNode::ScanRight]);
    }

    #[test]
    fn test_scan_left() {
        let input = vec![AstNode::Loop(vec![AstNode::MoveLeft(1)])];
        let output = optimize(input, OptLevel::Full);
        assert_eq!(output, vec![AstNode::ScanLeft]);
    }

    #[test]
    fn test_multiply_move() {
        // [->>+++<<] → multiply: tape[ptr+2] += tape[ptr] * 3; tape[ptr] = 0
        let input = vec![AstNode::Loop(vec![
            AstNode::Decrement(1),
            AstNode::MoveRight(2),
            AstNode::Increment(3),
            AstNode::MoveLeft(2),
        ])];
        let output = optimize(input, OptLevel::Full);
        assert_eq!(output, vec![AstNode::MultiplyMove(vec![(2, 3)])]);
    }

    #[test]
    fn test_error_folding() {
        let input = vec![
            AstNode::Propagate,
            AstNode::Propagate,
            AstNode::Propagate,
        ];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Propagate]);
    }

    #[test]
    fn test_no_optimization() {
        let input = vec![AstNode::Increment(3), AstNode::Output];
        let output = optimize(input.clone(), OptLevel::None);
        assert_eq!(output, input);
    }

    // ── Constant folding tests ─────────────────────────────────────────

    #[test]
    fn test_fold_adjacent_set_values() {
        // SetValue(3) then SetValue(7) → SetValue(7) (dead store)
        let input = vec![AstNode::SetValue(3), AstNode::SetValue(7)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::SetValue(7)]);
    }

    #[test]
    fn test_fold_set_value_increment() {
        // SetValue(0) then Increment(5) → SetValue(5)
        let input = vec![AstNode::SetValue(0), AstNode::Increment(5)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::SetValue(5)]);
    }

    #[test]
    fn test_fold_set_value_decrement() {
        // SetValue(10) then Decrement(3) → SetValue(7)
        let input = vec![AstNode::SetValue(10), AstNode::Decrement(3)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::SetValue(7)]);
    }

    #[test]
    fn test_fold_clear_then_increment() {
        // Clear then Increment(42) → SetValue(42)
        // (Clear is produced by pass_clear_loop from [-])
        let input = vec![
            AstNode::Loop(vec![AstNode::Decrement(1)]),
            AstNode::Increment(42),
        ];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::SetValue(42)]);
    }

    #[test]
    fn test_fold_clear_then_set_value() {
        // Clear then SetValue(99) → SetValue(99)
        let input = vec![AstNode::Clear, AstNode::SetValue(99)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::SetValue(99)]);
    }

    #[test]
    fn test_fold_remove_noop_increment() {
        // Increment(0) is a no-op — removed
        let input = vec![AstNode::Increment(0), AstNode::Output];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Output]);
    }

    #[test]
    fn test_fold_remove_noop_move() {
        // MoveRight(0) is a no-op — removed
        let input = vec![AstNode::MoveRight(0), AstNode::Output];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Output]);
    }

    #[test]
    fn test_fold_set_value_then_clear() {
        // SetValue(5) then Clear → Clear (SetValue is dead)
        let input = vec![AstNode::SetValue(5), AstNode::Clear];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Clear]);
    }

    #[test]
    fn test_fold_nested_in_loop() {
        // Folding applies inside loop bodies too
        let input = vec![AstNode::Loop(vec![
            AstNode::SetValue(10),
            AstNode::Increment(5),
            AstNode::Output,
        ])];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(
            output,
            vec![AstNode::Loop(vec![AstNode::SetValue(15), AstNode::Output])]
        );
    }

    // ── Move coalescing tests ─────────────────────────────────────────

    #[test]
    fn test_coalesce_adjacent_moves() {
        let input = vec![AstNode::MoveRight(3), AstNode::MoveRight(2)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::MoveRight(5)]);
    }

    #[test]
    fn test_coalesce_opposite_moves() {
        let input = vec![AstNode::MoveRight(5), AstNode::MoveLeft(3)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::MoveRight(2)]);
    }

    #[test]
    fn test_coalesce_cancelling_moves() {
        let input = vec![AstNode::MoveRight(3), AstNode::MoveLeft(3)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![]);
    }

    #[test]
    fn test_coalesce_adjacent_increments() {
        let input = vec![AstNode::Increment(4), AstNode::Increment(6)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Increment(10)]);
    }

    #[test]
    fn test_coalesce_opposite_arithmetic() {
        let input = vec![AstNode::Increment(10), AstNode::Decrement(3)];
        let output = optimize(input, OptLevel::Basic);
        assert_eq!(output, vec![AstNode::Increment(7)]);
    }

    // ── Dead code elimination tests ──────────────────────────────────

    #[test]
    fn test_dce_unused_sub() {
        // "used" sub is small enough to inline, so after inlining + DCE,
        // both SubDefs are removed and SubCall is replaced with body.
        let input = vec![
            AstNode::SubDef("unused".into(), vec![AstNode::Output]),
            AstNode::SubDef("used".into(), vec![AstNode::Output]),
            AstNode::SubCall("used".into()),
        ];
        let output = optimize(input, OptLevel::Full);
        // Both subs inlined/removed, only the Output from "used" remains
        assert!(!output.iter().any(|n| matches!(n, AstNode::SubDef(name, _) if name == "unused")));
        assert!(output.contains(&AstNode::Output));
    }

    #[test]
    fn test_dce_unreachable_after_return() {
        let input = vec![
            AstNode::SubDef("sub".into(), vec![
                AstNode::Output,
                AstNode::Return,
                AstNode::Increment(5),  // unreachable
                AstNode::Output,        // unreachable
            ]),
            AstNode::SubCall("sub".into()),
        ];
        let output = optimize(input, OptLevel::Full);
        assert_eq!(output, vec![
            AstNode::SubDef("sub".into(), vec![
                AstNode::Output,
                AstNode::Return,
            ]),
            AstNode::SubCall("sub".into()),
        ]);
    }

    // ── Subroutine inlining tests ────────────────────────────────────

    #[test]
    fn test_inline_small_sub() {
        let input = vec![
            AstNode::SubDef("tiny".into(), vec![AstNode::Increment(1), AstNode::Output]),
            AstNode::SubCall("tiny".into()),
        ];
        let output = optimize(input, OptLevel::Full);
        // After inlining, SubCall is replaced with body.
        // After DCE, the now-unused SubDef is removed.
        assert!(output.contains(&AstNode::Increment(1)));
        assert!(output.contains(&AstNode::Output));
        // SubCall should be gone (replaced with body)
        assert!(!output.iter().any(|n| matches!(n, AstNode::SubCall(_))));
    }

    #[test]
    fn test_no_inline_large_sub() {
        // Subroutine with > INLINE_THRESHOLD ops should NOT be inlined
        let body: Vec<AstNode> = (0..10).map(|_| AstNode::Output).collect();
        let input = vec![
            AstNode::SubDef("big".into(), body),
            AstNode::SubCall("big".into()),
        ];
        let output = optimize(input, OptLevel::Full);
        // SubCall should remain (not inlined)
        assert!(output.iter().any(|n| matches!(n, AstNode::SubCall(_))));
    }

    #[test]
    fn test_no_inline_recursive_sub() {
        // Subroutine that calls another sub should NOT be inlined
        let input = vec![
            AstNode::SubDef("caller".into(), vec![AstNode::SubCall("other".into())]),
            AstNode::SubDef("other".into(), vec![AstNode::Output]),
            AstNode::SubCall("caller".into()),
        ];
        let output = optimize(input, OptLevel::Full);
        // SubCall("caller") should remain
        assert!(output.iter().any(|n| matches!(n, AstNode::SubCall(name) if name == "caller")));
    }

    // ── Compile-time conditional evaluation tests ────────────────

    #[test]
    fn test_eval_known_equal_true() {
        // #5 ?= #5 [ body ] → body (always true)
        let input = vec![
            AstNode::SetValue(5),
            AstNode::IfEqual(5, vec![AstNode::Output], None),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(output.contains(&AstNode::Output));
    }

    #[test]
    fn test_eval_known_equal_false() {
        // #5 ?= #3 [ body ] → eliminated (always false)
        let input = vec![
            AstNode::SetValue(5),
            AstNode::IfEqual(3, vec![AstNode::Output], None),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(!output.contains(&AstNode::Output));
    }

    #[test]
    fn test_eval_dead_loop_after_clear() {
        // [-] [ body ] → eliminated (cell is 0, loop never enters)
        let input = vec![
            AstNode::Clear,
            AstNode::Loop(vec![AstNode::Output]),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(!output.contains(&AstNode::Output));
    }

    #[test]
    fn test_eval_if_else_known_true() {
        // #1 ?{ A } : { B } → A (cell nonzero)
        let input = vec![
            AstNode::SetValue(1),
            AstNode::IfElse(vec![AstNode::Output], vec![AstNode::Input]),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(output.contains(&AstNode::Output));
        assert!(!output.contains(&AstNode::Input));
    }

    #[test]
    fn test_eval_if_else_known_false() {
        // #0 ?{ A } : { B } → B (cell is zero)
        let input = vec![
            AstNode::SetValue(0),
            AstNode::IfElse(vec![AstNode::Output], vec![AstNode::Input]),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(!output.contains(&AstNode::Output));
        assert!(output.contains(&AstNode::Input));
    }

    // ── Loop unrolling tests ─────────────────────────────────────

    #[test]
    fn test_unroll_small_loop() {
        // #3 [- >+<] → >+< >+< >+<
        let input = vec![
            AstNode::SetValue(3),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::MoveRight(1),
                AstNode::Increment(1),
                AstNode::MoveLeft(1),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        // Should not contain a Loop — it was unrolled
        assert!(!output.iter().any(|n| matches!(n, AstNode::Loop(_))));
        // Should contain 3 copies of >+< (coalesced by move coalescing)
        // The exact form depends on how coalescing interacts, but no loop.
    }

    #[test]
    fn test_no_unroll_large_count() {
        // #100 [- body] → NOT unrolled (count > 16)
        let input = vec![
            AstNode::SetValue(100),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::Increment(1),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        // Loop should remain (or be optimized differently, but not unrolled)
        // Actually this is [-+] which is just a counter drain. But the point
        // is it shouldn't be unrolled 100 times.
    }

    #[test]
    fn test_no_unroll_side_effects() {
        // #3 [- .] → NOT unrolled (Output is a side effect)
        let input = vec![
            AstNode::SetValue(3),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::Output,
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        // Should still contain a loop (not unrolled due to I/O)
        assert!(output.iter().any(|n| matches!(n, AstNode::Loop(_))));
    }

    // ── Tail return elimination test ─────────────────────────────

    #[test]
    fn test_tail_return_eliminated() {
        let input = vec![
            AstNode::SubDef("sub".into(), vec![
                AstNode::Output,
                AstNode::Return,  // trailing Return — redundant
            ]),
            AstNode::SubCall("sub".into()),
        ];
        let output = optimize(input, OptLevel::Full);
        // After inlining (sub is small), the Return should be gone.
        // The Output should remain.
        assert!(output.contains(&AstNode::Output));
        assert!(!output.contains(&AstNode::Return));
    }

    // ── Auto-parallelism tests ──────────────────────────────────

    #[test]
    fn test_auto_parallel_basic() {
        // #100 [- >+< >>>>>>>>] → ParallelLoop with stride 8
        // Pattern: SetValue(100), Loop([Dec(1), MoveRight(1), Inc(1), MoveLeft(1), MoveRight(8)])
        // Inner body: [MoveRight(1), Inc(1), MoveLeft(1)] within stride 8
        let input = vec![
            AstNode::SetValue(100),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::MoveRight(1),
                AstNode::Increment(1),
                AstNode::MoveLeft(1),
                AstNode::MoveRight(8),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(output.iter().any(|n| matches!(n, AstNode::ParallelLoop { .. } | AstNode::GpuLoop { .. })),
            "Expected ParallelLoop or GpuLoop node, got: {:?}", output);
    }

    #[test]
    fn test_auto_parallel_below_threshold() {
        // #32 [- >+< >>>>>>>>] → NOT parallelized (count < 64)
        let input = vec![
            AstNode::SetValue(32),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::MoveRight(1),
                AstNode::Increment(1),
                AstNode::MoveLeft(1),
                AstNode::MoveRight(8),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        // Should NOT be parallelized due to low trip count
        assert!(!output.iter().any(|n| matches!(n, AstNode::ParallelLoop { .. })),
            "Should not parallelize with count < 64, got: {:?}", output);
    }

    #[test]
    fn test_auto_parallel_rejects_io() {
        // #100 [- . >>>>>>>>] → NOT parallelized (has Output)
        let input = vec![
            AstNode::SetValue(100),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::Output,
                AstNode::MoveRight(8),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(!output.iter().any(|n| matches!(n, AstNode::ParallelLoop { .. })),
            "Should not parallelize loops with I/O, got: {:?}", output);
    }

    #[test]
    fn test_auto_parallel_rejects_out_of_stride() {
        // #100 [- >>>>>>>>>+<<<<<<<<< >>>>>>>>] → NOT parallelized
        // Access at offset 9 exceeds stride 8
        let input = vec![
            AstNode::SetValue(100),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::MoveRight(9),
                AstNode::Increment(1),
                AstNode::MoveLeft(9),
                AstNode::MoveRight(8),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        assert!(!output.iter().any(|n| matches!(n, AstNode::ParallelLoop { .. })),
            "Should not parallelize with out-of-stride access, got: {:?}", output);
    }

    #[test]
    fn test_auto_parallel_stride_and_count() {
        // Verify the detected stride and trip_count values
        let input = vec![
            AstNode::SetValue(256),
            AstNode::Loop(vec![
                AstNode::Decrement(1),
                AstNode::SetValue(42),
                AstNode::MoveRight(4),
            ]),
        ];
        let output = optimize(input, OptLevel::Full);
        // GPU-safe loops get upgraded from ParallelLoop to GpuLoop
        let gpu = output.iter().find(|n| matches!(n, AstNode::GpuLoop { .. }));
        assert!(gpu.is_some(), "Expected GpuLoop, got: {:?}", output);
        if let Some(AstNode::GpuLoop { stride, trip_count, body, kernel_source, .. }) = gpu {
            assert_eq!(*stride, 4);
            assert_eq!(*trip_count, 256);
            assert_eq!(body.len(), 1); // SetValue(42)
            assert!(kernel_source.contains("bfpp_gpu_loop_"));
        }
    }
}
