// BF++ Semantic Analyzer — validates AST before codegen.

use crate::ast::AstNode;
use std::collections::HashSet;

#[derive(Debug)]
pub struct AnalysisError {
    pub message: String,
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Analysis error: {}", self.message)
    }
}

pub fn analyze(nodes: &[AstNode]) -> Result<(), Vec<AnalysisError>> {
    let mut errors = Vec::new();
    let mut defined_subs = HashSet::new();
    let mut called_subs = HashSet::new();

    // Collect all subroutine definitions and calls
    collect_subs(nodes, &mut defined_subs, &mut called_subs);

    // Check for calls to undefined subroutines
    for name in &called_subs {
        if !defined_subs.contains(name) {
            errors.push(AnalysisError {
                message: format!("Call to undefined subroutine '#{}'", name),
            });
        }
    }

    // Check for duplicate subroutine definitions
    let mut seen = HashSet::new();
    check_duplicate_defs(nodes, &mut seen, &mut errors);

    // Check for return (^) outside subroutine body
    check_return_context(nodes, false);

    // Check for empty FFI names
    check_ffi_names(nodes, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_subs(nodes: &[AstNode], defs: &mut HashSet<String>, calls: &mut HashSet<String>) {
    for node in nodes {
        match node {
            AstNode::SubDef(name, body) => {
                defs.insert(name.clone());
                collect_subs(body, defs, calls);
            }
            AstNode::SubCall(name) => {
                calls.insert(name.clone());
            }
            AstNode::Loop(body) => collect_subs(body, defs, calls),
            AstNode::ResultBlock(r, k) => {
                collect_subs(r, defs, calls);
                collect_subs(k, defs, calls);
            }
            AstNode::Deref(inner) => {
                collect_subs(&[*inner.clone()], defs, calls);
            }
            AstNode::FfiCall(_, _) => {
                // FFI calls don't define or call BF++ subroutines
            }
            _ => {}
        }
    }
}

fn check_duplicate_defs(nodes: &[AstNode], seen: &mut HashSet<String>, errors: &mut Vec<AnalysisError>) {
    for node in nodes {
        if let AstNode::SubDef(name, body) = node {
            if !seen.insert(name.clone()) {
                errors.push(AnalysisError {
                    message: format!("Duplicate subroutine definition '#{}'", name),
                });
            }
            check_duplicate_defs(body, seen, errors);
        }
    }
}

fn check_return_context(nodes: &[AstNode], in_sub: bool) {
    for node in nodes {
        match node {
            AstNode::Return => {
                if !in_sub {
                    // Top-level ^ transpiles to `return 0;` from main.
                    // Valid but unusual — emit as a diagnostic warning.
                    eprintln!("warning: top-level '^' will return from main");
                }
            }
            AstNode::SubDef(_, body) => {
                check_return_context(body, true);
            }
            AstNode::Loop(body) => {
                check_return_context(body, in_sub);
            }
            AstNode::ResultBlock(r, k) => {
                check_return_context(r, in_sub);
                check_return_context(k, in_sub);
            }
            _ => {}
        }
    }
}

fn check_ffi_names(nodes: &[AstNode], errors: &mut Vec<AnalysisError>) {
    for node in nodes {
        match node {
            AstNode::FfiCall(lib, func) => {
                if lib.is_empty() {
                    errors.push(AnalysisError {
                        message: "FFI call with empty library name".into(),
                    });
                }
                if func.is_empty() {
                    errors.push(AnalysisError {
                        message: "FFI call with empty function name".into(),
                    });
                }
            }
            AstNode::Loop(body) | AstNode::SubDef(_, body) => {
                check_ffi_names(body, errors);
            }
            AstNode::ResultBlock(r, k) => {
                check_ffi_names(r, errors);
                check_ffi_names(k, errors);
            }
            AstNode::Deref(inner) => {
                check_ffi_names(&[*inner.clone()], errors);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_program() {
        let nodes = vec![
            AstNode::SubDef("pr".into(), vec![AstNode::Output, AstNode::Return]),
            AstNode::SubCall("pr".into()),
        ];
        assert!(analyze(&nodes).is_ok());
    }

    #[test]
    fn test_undefined_sub() {
        let nodes = vec![AstNode::SubCall("undefined".into())];
        let result = analyze(&nodes);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors[0].message.contains("undefined"));
    }

    #[test]
    fn test_duplicate_sub() {
        let nodes = vec![
            AstNode::SubDef("dup".into(), vec![AstNode::Return]),
            AstNode::SubDef("dup".into(), vec![AstNode::Return]),
        ];
        let result = analyze(&nodes);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors[0].message.contains("Duplicate"));
    }
}
