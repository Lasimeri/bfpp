// BF++ Optimizer — peephole optimization passes on AST.

use crate::ast::AstNode;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptLevel {
    None,
    Basic, // O1: coalescing, clear loop
    Full,  // O2: all passes
}

pub fn optimize(nodes: Vec<AstNode>, level: OptLevel) -> Vec<AstNode> {
    match level {
        OptLevel::None => nodes,
        OptLevel::Basic => {
            let nodes = pass_clear_loop(nodes);
            pass_error_folding(nodes)
        }
        OptLevel::Full => {
            let nodes = pass_clear_loop(nodes);
            let nodes = pass_scan_loop(nodes);
            let nodes = pass_multiply_move(nodes);
            pass_error_folding(nodes)
        }
    }
}

/// Replace `[-]` and `[+]` loops with Clear node.
fn pass_clear_loop(nodes: Vec<AstNode>) -> Vec<AstNode> {
    nodes.into_iter().map(|node| {
        match node {
            AstNode::Loop(ref body) => {
                if body.len() == 1 {
                    match &body[0] {
                        AstNode::Decrement(1) | AstNode::Increment(1) => {
                            return AstNode::Clear;
                        }
                        _ => {}
                    }
                }
                // Recurse into loop body
                AstNode::Loop(pass_clear_loop(body.to_vec()))
            }
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

/// Replace `[>]` with ScanRight and `[<]` with ScanLeft.
fn pass_scan_loop(nodes: Vec<AstNode>) -> Vec<AstNode> {
    nodes.into_iter().map(|node| {
        match node {
            AstNode::Loop(ref body) => {
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

/// Detect multiplication/move patterns: `[->+++>++<<]`
/// Pattern: loop starts with Decrement(1), has a balanced set of moves and increments,
/// and the net pointer movement is 0.
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

/// Check if a loop body is a multiplication pattern.
/// Returns (offset, factor) pairs if it matches, None otherwise.
fn detect_multiply_pattern(body: &[AstNode]) -> Option<Vec<(isize, usize)>> {
    // First element must be Decrement(1)
    if body.is_empty() || body[0] != AstNode::Decrement(1) {
        return None;
    }

    let mut offset: isize = 0;
    let mut pairs: Vec<(isize, usize)> = Vec::new();

    for node in &body[1..] {
        match node {
            AstNode::MoveRight(n) => offset += *n as isize,
            AstNode::MoveLeft(n) => offset -= *n as isize,
            AstNode::Increment(n) => {
                if offset == 0 {
                    return None; // can't add to self in multiply pattern
                }
                pairs.push((offset, *n));
            }
            _ => return None, // non-move/inc breaks pattern
        }
    }

    // Net pointer movement must be 0 (return to original cell)
    if offset != 0 {
        return None;
    }

    if pairs.is_empty() {
        return None;
    }

    // Merge duplicate offsets
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

/// Remove consecutive duplicate `?` operators (only need one check).
fn pass_error_folding(nodes: Vec<AstNode>) -> Vec<AstNode> {
    let mut result = Vec::new();
    let mut last_was_propagate = false;

    for node in nodes {
        match node {
            AstNode::Propagate => {
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
}
