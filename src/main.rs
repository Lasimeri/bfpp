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

    /// Optimization level
    #[arg(short = 'O', default_value = "1")]
    opt_level: u8,

    /// Additional include paths for stdlib
    #[arg(long = "include")]
    include_paths: Vec<PathBuf>,

    /// C compiler to use
    #[arg(long, default_value = "cc")]
    cc: String,

    /// EOF value for input operations (0 = default, 255 = classic BF)
    #[arg(long, default_value = "0")]
    eof: u8,
}

fn main() {
    let cli = Cli::parse();

    if !cli.tape_size.is_power_of_two() {
        eprintln!("error: --tape-size must be a power of 2 (got {})", cli.tape_size);
        std::process::exit(1);
    }

    // Read source
    let raw_source = match std::fs::read_to_string(&cli.input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{}': {}", cli.input.display(), e);
            std::process::exit(1);
        }
    };

    // Preprocess (expand !include directives)
    let source = match preprocess::preprocess(&raw_source, &cli.input, &cli.include_paths) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    // Lex
    let tokens = match lexer::lex(&source) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}:{}", cli.input.display(), e);
            std::process::exit(1);
        }
    };

    // Parse
    let program = match parser::parse(&tokens) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}:{}", cli.input.display(), e);
            std::process::exit(1);
        }
    };

    // Analyze
    if let Err(errors) = analyzer::analyze(&program.nodes) {
        for e in &errors {
            eprintln!("{}: {}", cli.input.display(), e);
        }
        std::process::exit(1);
    }

    // Optimize
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

    // Parse framebuffer dimensions
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

    if let Some((w, h)) = framebuffer {
        let fb_bytes = (w as usize) * (h as usize) * 3;
        if cli.tape_size < fb_bytes + 256 {
            eprintln!("error: tape size {} too small for {}x{} framebuffer (need at least {})",
                cli.tape_size, w, h, fb_bytes + 256);
            std::process::exit(1);
        }
    }

    // Codegen
    let opts = codegen::CodegenOptions {
        tape_size: cli.tape_size,
        stack_size: cli.stack_size,
        call_depth: cli.call_depth,
        framebuffer,
        eof_value: cli.eof,
    };

    let codegen_result = codegen::generate(&optimized_program, &opts);
    let c_source = codegen_result.c_source;

    // Determine output path
    let stem = cli.input.file_stem().unwrap().to_string_lossy().to_string();
    let c_path = cli.output.as_ref()
        .map(|o| o.with_extension("c"))
        .unwrap_or_else(|| PathBuf::from(format!("{}.c", stem)));
    let bin_path = cli.output.as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(&stem));

    if cli.emit_c {
        // Write C source and exit
        if let Err(e) = std::fs::write(&c_path, &c_source) {
            eprintln!("error: cannot write '{}': {}", c_path.display(), e);
            std::process::exit(1);
        }
        println!("{}", c_path.display());
        return;
    }

    // Write temporary C file
    let tmp_c = PathBuf::from(format!("/tmp/bfpp_{}.c", std::process::id()));
    if let Err(e) = std::fs::write(&tmp_c, &c_source) {
        eprintln!("error: cannot write temp file: {}", e);
        std::process::exit(1);
    }

    // Compile with cc
    let mut cc_cmd = Command::new(&cli.cc);
    cc_cmd.args([
        tmp_c.to_str().unwrap(),
        "-o", bin_path.to_str().unwrap(),
        "-O2",
        "-Wall",
        "-Wno-unused-variable",
        "-Wno-unused-function",
    ]);

    if framebuffer.is_some() {
        cc_cmd.args(["-lSDL2"]);
    }

    if codegen_result.uses_ffi {
        cc_cmd.args(["-ldl"]);
    }

    let status = match cc_cmd.status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot run '{}': {}", cli.cc, e);
            let _ = std::fs::remove_file(&tmp_c);
            std::process::exit(1);
        }
    };

    let _ = std::fs::remove_file(&tmp_c);

    if !status.success() {
        eprintln!("error: C compilation failed");
        std::process::exit(1);
    }
}
