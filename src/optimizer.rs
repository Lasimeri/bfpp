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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptLevel {
    None,
    Basic, // O1: clear-loop detection, error folding
    Full,  // O2: all Basic passes + scan-loop, multiply-move
}

// Entry point: applies the selected optimization passes in sequence.
// Pass ordering for Full:
//   1. clear-loop  — reduces [-] and [+] to Clear (must run before multiply-move)
//   2. scan-loop   — reduces [>] and [<] to ScanRight/ScanLeft
//   3. multiply-move — detects balanced decrement-move-increment loops
//   4. error-folding — collapses consecutive ? (Propagate) nodes
pub fn optimize(nodes: Vec<AstNode>, level: OptLevel) -> Vec<AstNode> {
    match level {
        OptLevel::None => nodes,
        OptLevel::Basic => {
            let nodes = pass_clear_loop(nodes);
            let nodes = pass_fold_constants(nodes);
            pass_error_folding(nodes)
        }
        OptLevel::Full => {
            let nodes = pass_clear_loop(nodes);
            let nodes = pass_fold_constants(nodes);
            let nodes = pass_scan_loop(nodes);
            let nodes = pass_multiply_move(nodes);
            pass_error_folding(nodes)
        }
    }
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
    nodes.into_iter().map(|node| {
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
    }).collect()
}

// Scan-loop detection: replaces `[>]` with ScanRight and `[<]` with ScanLeft.
//
// These loops move the pointer in one direction until a zero cell is found —
// essentially a linear scan / memchr(0). Codegen can emit a while-loop with
// pointer arithmetic or use memchr for a significant speedup over per-cell branching.
fn pass_scan_loop(nodes: Vec<AstNode>) -> Vec<AstNode> {
    nodes.into_iter().map(|node| {
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
    }).collect()
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
    nodes.into_iter().map(|node| {
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
    }).collect()
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
}
