// BF++ Semantic Analyzer — validates AST invariants before codegen.
//
// Runs after parsing, before optimization/codegen. Catches errors that are
// syntactically valid but semantically wrong:
//
// Validation passes (in order):
//   1. collect_subs        — walk the full AST, collect all SubDef names and
//                            SubCall names into sets. Then check that every
//                            called name has a corresponding definition.
//   2. check_duplicate_defs — walk SubDefs and flag any name defined more than
//                            once. Uses a separate pass from collect_subs because
//                            collect_subs uses a HashSet (deduplicates on insert),
//                            so it can't detect duplicates.
//   3. check_return_context — warn (not error) if `^` (Return) appears outside
//                            a subroutine body. Top-level `^` compiles to
//                            `return 0;` from main, which is valid C but almost
//                            certainly not what the programmer intended.
//   4. check_ffi_names      — reject FFI calls with empty library or function
//                            names. The lexer allows empty strings in quotes;
//                            this catches them before codegen emits a broken
//                            dlopen/dlsym call.
//
// Note: check_return_context uses eprintln (direct stderr warning) instead of
// pushing to the error vec because a top-level `^` is a valid program — it's a
// diagnostic hint, not a hard error. The analyzer returns Ok(()) even if the
// warning fires.

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

// Main entry point: run all validation passes and collect errors.
// Returns Ok(()) if no hard errors; warnings go to stderr independently.
pub fn analyze(nodes: &[AstNode]) -> Result<(), Vec<AnalysisError>> {
    let mut errors = Vec::new();
    let mut defined_subs = HashSet::new();
    let mut called_subs = HashSet::new();

    // Pass 1: collect all subroutine definitions and call sites
    collect_subs(nodes, &mut defined_subs, &mut called_subs);

    // Check for calls to undefined subroutines.
    // Names starting with "__" are compiler intrinsics — they're handled by
    // codegen as inline C, not as BF++ subroutine definitions.
    for name in &called_subs {
        if !defined_subs.contains(name) && !name.starts_with("__") {
            errors.push(AnalysisError {
                message: format!("Call to undefined subroutine '#{}'", name),
            });
        }
    }

    // Pass 2: check for duplicate subroutine definitions
    let mut seen = HashSet::new();
    check_duplicate_defs(nodes, &mut seen, &mut errors);

    // Pass 3: warn about top-level return (not a hard error)
    check_return_context(nodes, false);

    // Pass 4: validate FFI library/function names aren't empty
    check_ffi_names(nodes, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// Recursively walk the AST and insert all SubDef names into `defs` and all
// SubCall names into `calls`. Recurses into Loop bodies, ResultBlock branches,
// Deref wrappers, and nested SubDef bodies (subroutines can define inner subs).
fn collect_subs(nodes: &[AstNode], defs: &mut HashSet<String>, calls: &mut HashSet<String>) {
    for node in nodes {
        match node {
            AstNode::SubDef(name, body) => {
                defs.insert(name.clone());
                // Recurse into body — subroutines can contain nested defs/calls
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
                // Deref wraps a single node — unbox and check it
                collect_subs(&[*inner.clone()], defs, calls);
            }
            AstNode::FfiCall(_, _) => {
                // FFI calls don't define or call BF++ subroutines
            }
            _ => {}
        }
    }
}

// Detect duplicate subroutine definitions. Uses a separate `seen` set because
// the collect_subs pass uses HashSet::insert which silently deduplicates.
// Here, a failed insert (name already in `seen`) means it's a duplicate → error.
// Recurses into SubDef bodies to catch nested duplicates.
fn check_duplicate_defs(nodes: &[AstNode], seen: &mut HashSet<String>, errors: &mut Vec<AnalysisError>) {
    for node in nodes {
        if let AstNode::SubDef(name, body) = node {
            if !seen.insert(name.clone()) {
                errors.push(AnalysisError {
                    message: format!("Duplicate subroutine definition '#{}'", name),
                });
            }
            // Check body for nested duplicate defs
            check_duplicate_defs(body, seen, errors);
        }
    }
}

// Warn about `^` (Return) used outside a subroutine body.
//
// Uses eprintln instead of pushing to the error vec because top-level `^` is
// technically valid — it transpiles to `return 0;` from main(). It's almost
// always a mistake, but it's not a semantic error that should block compilation.
// The warning goes to stderr so it's visible but non-fatal.
//
// `in_sub` tracks whether we're currently inside a SubDef body. Entering a
// SubDef sets it to true; Loop and ResultBlock propagate the current value
// (a return inside a loop inside a sub is still "in a sub").
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
                // Entering a subroutine body — return is expected here
                check_return_context(body, true);
            }
            AstNode::Loop(body) => {
                // Propagate current context — loop doesn't change return validity
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

// Validate FFI call names: both library and function names must be non-empty.
// Empty strings are syntactically valid (the lexer accepts `\ffi "" ""`) but
// would produce broken dlopen("") / dlsym(handle, "") calls in codegen.
// Recurses into all block-containing nodes to catch FFI calls inside loops,
// subroutines, result/catch blocks, and deref wrappers.
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

    #[test]
    fn test_intrinsic_not_flagged_as_undefined() {
        // Calls to __* names should NOT trigger "undefined sub" errors
        let nodes = vec![AstNode::SubCall("__term_size".into())];
        assert!(analyze(&nodes).is_ok());
    }

    #[test]
    fn test_intrinsic_still_allows_normal_undefined() {
        // Non-__ names should still be flagged
        let nodes = vec![AstNode::SubCall("nonexistent".into())];
        assert!(analyze(&nodes).is_err());
    }

    #[test]
    fn test_empty_ffi_lib_name() {
        let nodes = vec![AstNode::FfiCall("".into(), "func".into())];
        let result = analyze(&nodes);
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].message.contains("empty library"));
    }

    #[test]
    fn test_empty_ffi_func_name() {
        let nodes = vec![AstNode::FfiCall("lib".into(), "".into())];
        let result = analyze(&nodes);
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].message.contains("empty function"));
    }
}
