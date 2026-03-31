// BF++ Preprocessor — handles `!include "filename"` directives.
//
// Runs as a text-level expansion pass before lexing.
// Resolves includes relative to: source dir → --include paths → ./stdlib/ → exe-relative stdlib.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MAX_INCLUDE_DEPTH: usize = 64;

#[derive(Debug)]
pub struct PreprocessError {
    pub message: String,
    pub file: PathBuf,
    pub line: usize,
}

impl std::fmt::Display for PreprocessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file.display(), self.line, self.message)
    }
}

pub fn preprocess(
    source: &str,
    source_path: &Path,
    include_paths: &[PathBuf],
) -> Result<String, PreprocessError> {
    let mut visited = HashSet::new();
    let canonical = source_path.canonicalize().unwrap_or_else(|_| source_path.to_path_buf());
    visited.insert(canonical.clone());
    expand(source, source_path, include_paths, &mut visited, 0)
}

fn expand(
    source: &str,
    source_path: &Path,
    include_paths: &[PathBuf],
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<String, PreprocessError> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(PreprocessError {
            message: format!("Include depth exceeds maximum of {}", MAX_INCLUDE_DEPTH),
            file: source_path.to_path_buf(),
            line: 0,
        });
    }

    let source_dir = source_path.parent().unwrap_or(Path::new("."));
    let mut result = String::new();
    let mut in_string = false;

    for (line_num, line) in source.lines().enumerate() {
        // Track string literal state (simple: toggle on unescaped ")
        // But !include must be at line start (possibly with whitespace), so
        // we just check if the trimmed line starts with !include
        let trimmed = line.trim();

        if !in_string && trimmed.starts_with("!include ") {
            let rest = trimmed.strip_prefix("!include ").unwrap().trim();
            if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
                let filename = &rest[1..rest.len() - 1];
                let resolved = resolve_include(filename, source_dir, include_paths)
                    .ok_or_else(|| PreprocessError {
                        message: format!("Cannot find include file '{}'", filename),
                        file: source_path.to_path_buf(),
                        line: line_num + 1,
                    })?;

                let canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
                if !visited.insert(canonical.clone()) {
                    // Already included — skip (not an error, just dedup)
                    result.push('\n');
                    continue;
                }

                let included_source = std::fs::read_to_string(&resolved)
                    .map_err(|e| PreprocessError {
                        message: format!("Cannot read '{}': {}", resolved.display(), e),
                        file: source_path.to_path_buf(),
                        line: line_num + 1,
                    })?;

                let expanded = expand(&included_source, &resolved, include_paths, visited, depth + 1)?;
                result.push_str(&expanded);
                result.push('\n');
                continue;
            } else {
                return Err(PreprocessError {
                    message: "!include requires a quoted filename: !include \"file.bfpp\"".into(),
                    file: source_path.to_path_buf(),
                    line: line_num + 1,
                });
            }
        }

        // Track string state across lines (for multi-line strings)
        for (i, ch) in line.chars().enumerate() {
            if ch == '"' {
                // Count consecutive backslashes before this quote
                let mut backslash_count = 0;
                let bytes = line.as_bytes();
                let mut j = i;
                while j > 0 && bytes[j - 1] == b'\\' {
                    backslash_count += 1;
                    j -= 1;
                }
                // Even number of backslashes = real quote
                // Odd number = the quote itself is escaped
                if backslash_count % 2 == 0 {
                    in_string = !in_string;
                }
            }
        }

        result.push_str(line);
        result.push('\n');
    }

    Ok(result)
}

fn resolve_include(filename: &str, source_dir: &Path, include_paths: &[PathBuf]) -> Option<PathBuf> {
    // 1. Relative to source file directory
    let candidate = source_dir.join(filename);
    if candidate.exists() {
        return Some(candidate);
    }

    // 2. --include paths
    for path in include_paths {
        let candidate = path.join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 3. ./stdlib/ relative to CWD
    let candidate = PathBuf::from("stdlib").join(filename);
    if candidate.exists() {
        return Some(candidate);
    }

    // 4. Relative to the executable's directory
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join("stdlib").join(filename);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_no_includes() {
        let source = "+++--->><<\n[.>]\n";
        let result = preprocess(source, Path::new("test.bfpp"), &[]).unwrap();
        assert_eq!(result, "+++--->><<\n[.>]\n");
    }

    #[test]
    fn test_include_resolution() {
        // Create temp files
        let dir = std::env::temp_dir().join("bfpp_test_include");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("helper.bfpp"), "!#helper{ + ^ }").unwrap();
        fs::write(dir.join("main.bfpp"), "!include \"helper.bfpp\"\n!#helper\n").unwrap();

        let source = fs::read_to_string(dir.join("main.bfpp")).unwrap();
        let result = preprocess(&source, &dir.join("main.bfpp"), &[]).unwrap();
        assert!(result.contains("!#helper{ + ^ }"));
        assert!(result.contains("!#helper"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cycle_detection() {
        let dir = std::env::temp_dir().join("bfpp_test_cycle");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("a.bfpp"), "!include \"b.bfpp\"\n").unwrap();
        fs::write(dir.join("b.bfpp"), "!include \"a.bfpp\"\n").unwrap();

        let source = fs::read_to_string(dir.join("a.bfpp")).unwrap();
        // Should not error — cycle is detected and skipped
        let result = preprocess(&source, &dir.join("a.bfpp"), &[]);
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_escaped_backslash_before_quote() {
        // "\\" is an escaped backslash — the second " closes the string
        // So !include on the next line should be processed
        let dir = std::env::temp_dir().join("bfpp_test_escape");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("inc.bfpp"), "+++").unwrap();

        let source = "\"\\\\\"\n!include \"inc.bfpp\"\n";
        let result = preprocess(source, &dir.join("main.bfpp"), &[]).unwrap();
        assert!(result.contains("+++"), "Include should trigger after escaped backslash string");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_include_inside_string_not_expanded() {
        // Multi-line string containing !include should NOT be expanded
        let source = "\"hello\n!include \"file.bfpp\"\nworld\"";
        // This should not error about missing file — the !include is inside a string
        let result = preprocess(source, std::path::Path::new("test.bfpp"), &[]);
        assert!(result.is_ok());
    }
}
