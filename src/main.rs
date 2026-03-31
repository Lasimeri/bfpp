mod ast;
mod lexer;
mod parser;
mod analyzer;
mod codegen;
mod optimizer;
mod preprocess;
mod error_codes;

use clap::Parser;
use std::path::PathBuf;
use std::process::Command;

// CLI argument definitions.
//
// Compilation pipeline: read source → preprocess (!include expansion) → lex →
// parse → static analysis → optimize → C codegen → invoke CC to produce binary.
//
// Defaults: 64K tape, 4K data stack, 256 call depth, O1 optimization, cc compiler,
// EOF value 0. Framebuffer is opt-in (requires WxH dimensions and links SDL2).
#[derive(Parser, Debug)]
#[command(name = "bfpp", version, about = "BF++ transpiler — Brainfuck extended")]
struct Cli {
    /// Input BF++ source file
    input: PathBuf,

    /// Output binary name
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Emit C source instead of compiling
    #[arg(long)]
    emit_c: bool,

    // 64K default — enough for most BF programs. Must be power-of-2 so the
    // runtime can use bitmask wrapping (ptr & (tape_size - 1)) instead of modulo.
    /// Tape size in bytes
    #[arg(long, default_value = "65536")]
    tape_size: usize,

    /// Data stack size (entries)
    #[arg(long, default_value = "4096")]
    stack_size: usize,

    /// Max call stack depth
    #[arg(long, default_value = "256")]
    call_depth: usize,

    /// Enable framebuffer (WxH format, e.g., 80x60)
    #[arg(long)]
    framebuffer: Option<String>,

    /// Disable all optimizations
    #[arg(long)]
    no_optimize: bool,

    // Maps 0 → None, 1 → Basic, 2+ → Full. Overridden to None by --no-optimize.
    /// Optimization level
    #[arg(short = 'O', default_value = "1")]
    opt_level: u8,

    /// Additional include paths for stdlib
    #[arg(long = "include")]
    include_paths: Vec<PathBuf>,

    /// C compiler to use
    #[arg(long, default_value = "cc")]
    cc: String,

    // Classic BF uses 0 for EOF; some implementations use 255 (-1 as unsigned).
    /// EOF value for input operations (0 = default, 255 = classic BF)
    #[arg(long, default_value = "0")]
    eof: u8,
}

fn main() {
    let cli = Cli::parse();

    // Tape size must be a power of 2 so codegen can emit bitmask wrapping
    // (ptr & (size - 1)) instead of expensive modulo. This is a hard requirement
    // of the generated C runtime, not just a performance preference.
    if !cli.tape_size.is_power_of_two() {
        eprintln!("error: --tape-size must be a power of 2 (got {})", cli.tape_size);
        std::process::exit(1);
    }

    // --- Stage 1: Read source file ---
    let raw_source = match std::fs::read_to_string(&cli.input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{}': {}", cli.input.display(), e);
            std::process::exit(1);
        }
    };

    // --- Stage 2: Preprocess (expand !include directives, resolve paths) ---
    let source = match preprocess::preprocess(&raw_source, &cli.input, &cli.include_paths) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    // --- Stage 3: Lex (tokenize the preprocessed source) ---
    let tokens = match lexer::lex(&source) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}:{}", cli.input.display(), e);
            std::process::exit(1);
        }
    };

    // --- Stage 4: Parse (tokens → AST with coalesced counts) ---
    let program = match parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}:{}", cli.input.display(), e);
            std::process::exit(1);
        }
    };

    // --- Stage 5: Static analysis (undefined sub calls, nesting errors, etc.) ---
    if let Err(errors) = analyzer::analyze(&program.nodes) {
        for e in &errors {
            eprintln!("{}: {}", cli.input.display(), e);
        }
        std::process::exit(1);
    }

    // --- Stage 6: Optimize (peephole passes on the AST) ---
    // --no-optimize forces None regardless of -O flag.
    let opt_level = if cli.no_optimize {
        optimizer::OptLevel::None
    } else {
        match cli.opt_level {
            0 => optimizer::OptLevel::None,
            1 => optimizer::OptLevel::Basic,
            _ => optimizer::OptLevel::Full,
        }
    };

    let optimized_nodes = optimizer::optimize(program.nodes, opt_level);
    let optimized_program = ast::Program { nodes: optimized_nodes };

    // Parse framebuffer dimensions from "WxH" string format
    let framebuffer = cli.framebuffer.as_ref().map(|s| {
        let parts: Vec<&str> = s.split('x').collect();
        if parts.len() != 2 {
            eprintln!("error: --framebuffer must be WxH format (e.g., 80x60)");
            std::process::exit(1);
        }
        let w: u32 = parts[0].parse().unwrap_or_else(|_| {
            eprintln!("error: invalid framebuffer width");
            std::process::exit(1);
        });
        let h: u32 = parts[1].parse().unwrap_or_else(|_| {
            eprintln!("error: invalid framebuffer height");
            std::process::exit(1);
        });
        (w, h)
    });

    // Framebuffer bounds check: the framebuffer is mapped into the tape as W*H*3
    // bytes (RGB pixels). The tape must have room for the framebuffer PLUS at least
    // 256 bytes of working space for the program itself.
    if let Some((w, h)) = framebuffer {
        let fb_bytes = (w as usize) * (h as usize) * 3;
        if cli.tape_size < fb_bytes + 256 {
            eprintln!("error: tape size {} too small for {}x{} framebuffer (need at least {})",
                cli.tape_size, w, h, fb_bytes + 256);
            std::process::exit(1);
        }
    }

    // --- Stage 7: C code generation ---
    let opts = codegen::CodegenOptions {
        tape_size: cli.tape_size,
        stack_size: cli.stack_size,
        call_depth: cli.call_depth,
        framebuffer,
        eof_value: cli.eof,
    };

    let codegen_result = codegen::generate(&optimized_program, &opts);
    let c_source = codegen_result.c_source;

    // Derive output paths from input stem or explicit -o flag.
    // C path: used for --emit-c output or as the temp file base.
    // Bin path: final binary name.
    let stem = cli.input.file_stem().unwrap().to_string_lossy().to_string();
    let c_path = cli.output.as_ref()
        .map(|o| o.with_extension("c"))
        .unwrap_or_else(|| PathBuf::from(format!("{}.c", stem)));
    let bin_path = cli.output.as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(&stem));

    if cli.emit_c {
        // --emit-c: write the generated C source to disk and exit without compiling
        if let Err(e) = std::fs::write(&c_path, &c_source) {
            eprintln!("error: cannot write '{}': {}", c_path.display(), e);
            std::process::exit(1);
        }
        println!("{}", c_path.display());
        return;
    }

    // --- Stage 8: C compilation ---
    // Write to a PID-namespaced temp file in /tmp to avoid clobbering parallel builds
    // and to keep the working directory clean. Cleaned up after compilation regardless
    // of success or failure.
    let tmp_c = PathBuf::from(format!("/tmp/bfpp_{}.c", std::process::id()));
    if let Err(e) = std::fs::write(&tmp_c, &c_source) {
        eprintln!("error: cannot write temp file: {}", e);
        std::process::exit(1);
    }

    // Invoke the C compiler. Flags:
    //   -O2    — optimize the generated C (complements BF++ AST-level optimizations)
    //   -Wall  — catch codegen bugs early via compiler warnings
    //   -Wno-unused-variable / -Wno-unused-function — the codegen emits helper
    //     functions and variables unconditionally; not all programs use them
    let mut cc_cmd = Command::new(&cli.cc);
    cc_cmd.args([
        tmp_c.to_str().unwrap(),
        "-o", bin_path.to_str().unwrap(),
        "-O2",
        "-Wall",
        "-Wno-unused-variable",
        "-Wno-unused-function",
    ]);

    // -lSDL2: only needed when framebuffer mode is active (renders tape region
    // as an SDL2 window)
    if framebuffer.is_some() {
        cc_cmd.args(["-lSDL2"]);
    }

    // -ldl: needed for dlopen/dlsym when the program uses FFI calls (\ffi)
    if codegen_result.uses_ffi {
        cc_cmd.args(["-ldl"]);
    }

    // TUI runtime: compile bfpp_rt.c alongside the generated C and add its
    // include path. Searches CWD/runtime first, then alongside the executable.
    if codegen_result.uses_tui_runtime {
        let runtime_paths = [
            PathBuf::from("runtime"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime")))
                .unwrap_or_default(),
        ];
        for rt_dir in &runtime_paths {
            if rt_dir.join("bfpp_rt.c").exists() {
                cc_cmd.arg(format!("-I{}", rt_dir.display()));
                cc_cmd.arg(rt_dir.join("bfpp_rt.c").to_str().unwrap());
                break;
            }
        }
    }

    let status = match cc_cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot run '{}': {}", cli.cc, e);
            let _ = std::fs::remove_file(&tmp_c); // clean up temp file on CC launch failure
            std::process::exit(1);
        }
    };

    // Always remove the temp file — the binary (if produced) is the only artifact
    let _ = std::fs::remove_file(&tmp_c);

    if !status.success() {
        eprintln!("error: C compilation failed");
        std::process::exit(1);
    }
}
