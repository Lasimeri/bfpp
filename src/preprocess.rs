// BF++ Preprocessor — handles `!include "filename"` directives.
//
// Runs as a text-level expansion pass BEFORE lexing. This means included content
// is spliced into the source as raw text — included files can define subroutines,
// contain partial expressions, etc. The lexer sees one flat string.
//
// Include resolution order (first match wins):
//   1. Relative to the directory of the file containing the !include
//   2. Each --include path provided on the command line, in order
//   3. ./stdlib/ relative to the current working directory
//   4. stdlib/ relative to the bfpp executable's directory
//
// Cycle detection: a HashSet of canonical paths prevents infinite recursion.
// Re-including an already-visited file is silently skipped (not an error),
// which allows diamond-shaped include graphs to work correctly.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Read an include file, transparently decompressing `.zst` if the compressed
/// variant exists on disk. When `path.zst` is found, it takes priority over
/// the uncompressed `path`. Requires the `compressed-includes` feature (zstd).
/// Without the feature, only plain-text files are read.
fn read_include_file(path: &Path) -> Result<String, String> {
    #[cfg(feature = "compressed-includes")]
    {
        let mut zst_path = path.as_os_str().to_os_string();
        zst_path.push(".zst");
        let zst_path = PathBuf::from(zst_path);
        if zst_path.exists() {
            use std::io::Read as _;
            let file = std::fs::File::open(&zst_path)
                .map_err(|e| format!("Cannot open {}: {}", zst_path.display(), e))?;
            let mut decoder = zstd::Decoder::new(file)
                .map_err(|e| format!("zstd decode error for {}: {}", zst_path.display(), e))?;
            let mut content = String::new();
            decoder.read_to_string(&mut content)
                .map_err(|e| format!("zstd read error for {}: {}", zst_path.display(), e))?;
            return Ok(content);
        }
    }
    // Fall back to plain text
    std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {}", path.display(), e))
}

// Guard against runaway include chains (mutual recursion through many files)
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

/// Public entry point for preprocessing. Seeds the visited set with the root file
/// and delegates to the recursive `expand` function.
pub fn preprocess(
    source: &str,
    source_path: &Path,
    include_paths: &[PathBuf],
) -> Result<String, PreprocessError> {
    let mut visited = HashSet::new();
    let mut defines = HashMap::new();
    // Canonicalize to resolve symlinks — ensures two paths to the same file are deduplicated.
    // Falls back to the raw path if canonicalize fails (e.g., the path doesn't exist on disk
    // because we're processing an in-memory source with a synthetic path, as in tests).
    let canonical = source_path.canonicalize().unwrap_or_else(|_| source_path.to_path_buf());
    visited.insert(canonical.clone());
    expand(source, source_path, include_paths, &mut visited, &mut defines, 0)
}

// Recursive expansion workhorse. Processes one source text, line by line:
// - Lines starting with `!include "..."` (outside string literals) are replaced
//   with the recursively-expanded contents of the referenced file.
// - All other lines pass through unchanged.
// - String-state tracking prevents !include inside multi-line string literals
//   from being treated as directives.
fn expand(
    source: &str,
    source_path: &Path,
    include_paths: &[PathBuf],
    visited: &mut HashSet<PathBuf>,
    defines: &mut HashMap<String, String>,
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
    // Tracks whether we're currently inside a multi-line string literal.
    // When true, preprocessor directives are treated as string content, not expanded.
    let mut in_string = false;

    for (line_num, line) in source.lines().enumerate() {
        let trimmed = line.trim();

        // Skip all directives inside string literals
        if !in_string {
            // !define NAME VALUE — text substitution macro (like C #define without params)
            if trimmed.starts_with("!define ") {
                let rest = trimmed.strip_prefix("!define ").unwrap().trim();
                let mut parts = rest.splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or("").to_string();
                let value = parts.next().unwrap_or("").trim().to_string();
                if name.is_empty() {
                    return Err(PreprocessError {
                        message: "!define requires a name: !define NAME VALUE".into(),
                        file: source_path.to_path_buf(),
                        line: line_num + 1,
                    });
                }
                defines.insert(name, value);
                result.push('\n');
                continue;
            }

            // !undef NAME — remove a previously defined macro
            if trimmed.starts_with("!undef ") {
                let name = trimmed.strip_prefix("!undef ").unwrap().trim().to_string();
                if name.is_empty() {
                    return Err(PreprocessError {
                        message: "!undef requires a name: !undef NAME".into(),
                        file: source_path.to_path_buf(),
                        line: line_num + 1,
                    });
                }
                defines.remove(&name);
                result.push('\n');
                continue;
            }

            // !include "filename" — expand included file
            if trimmed.starts_with("!include ") {
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
                        result.push('\n');
                        continue;
                    }

                    let included_source = read_include_file(&resolved)
                        .map_err(|msg| PreprocessError {
                            message: msg,
                            file: source_path.to_path_buf(),
                            line: line_num + 1,
                        })?;

                    // Included files see parent defines and can add their own
                    let expanded = expand(&included_source, &resolved, include_paths, visited, defines, depth + 1)?;
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
        }

        // Track string-literal state across lines so that preprocessor directives
        // inside multi-line strings are not expanded. We scan every line for
        // unescaped quote characters and toggle in_string accordingly.
        for (i, ch) in line.chars().enumerate() {
            if ch == '"' {
                let mut backslash_count = 0;
                let bytes = line.as_bytes();
                let mut j = i;
                while j > 0 && bytes[j - 1] == b'\\' {
                    backslash_count += 1;
                    j -= 1;
                }
                if backslash_count % 2 == 0 {
                    in_string = !in_string;
                }
            }
        }

        // Apply macro expansion to non-directive, non-string lines.
        // Single pass: replace all defined names with their values.
        let output_line = if !in_string && !defines.is_empty() {
            expand_defines(line, defines)
        } else {
            line.to_string()
        };

        result.push_str(&output_line);
        result.push('\n');
    }

    Ok(result)
}

/// Expand all defined macros in a single line. Single-pass text replacement.
/// Replaces occurrences of each defined name with its value, longest-name-first
/// to prevent partial matches (e.g., "FOOBAR" shouldn't match "FOO" first).
fn expand_defines(line: &str, defines: &HashMap<String, String>) -> String {
    if defines.is_empty() {
        return line.to_string();
    }

    // Sort names longest-first so longer names match before their prefixes
    let mut names: Vec<&String> = defines.keys().collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));

    let mut result = line.to_string();
    for name in names {
        if let Some(value) = defines.get(name) {
            result = result.replace(name.as_str(), value.as_str());
        }
    }
    result
}

/// Check whether a candidate include path exists — either as a plain file
/// or (when `compressed-includes` is enabled) as a `.zst` compressed file.
fn include_candidate_exists(candidate: &Path) -> bool {
    if candidate.exists() {
        return true;
    }
    #[cfg(feature = "compressed-includes")]
    {
        let mut zst = candidate.as_os_str().to_os_string();
        zst.push(".zst");
        if PathBuf::from(zst).exists() {
            return true;
        }
    }
    false
}

// Resolve an include filename to an actual filesystem path.
// Tries four locations in priority order, returning the first that exists
// (plain or `.zst` compressed when the feature is enabled).
// This order mirrors how C compilers resolve #include "..." — local first, then
// search paths, then system-level locations.
fn resolve_include(filename: &str, source_dir: &Path, include_paths: &[PathBuf]) -> Option<PathBuf> {
    // 1. Relative to the file that contains the !include directive.
    //    Highest priority — local includes should shadow stdlib versions.
    let candidate = source_dir.join(filename);
    if include_candidate_exists(&candidate) {
        return Some(candidate);
    }

    // 2. User-specified --include paths, checked in the order provided.
    for path in include_paths {
        let candidate = path.join(filename);
        if include_candidate_exists(&candidate) {
            return Some(candidate);
        }
    }

    // 3. ./stdlib/ relative to CWD — supports running from the project root
    //    without needing explicit --include flags.
    let candidate = PathBuf::from("stdlib").join(filename);
    if include_candidate_exists(&candidate) {
        return Some(candidate);
    }

    // 4. stdlib/ relative to the bfpp executable — supports installed binaries
    //    that ship with a stdlib directory alongside the binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join("stdlib").join(filename);
            if include_candidate_exists(&candidate) {
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
