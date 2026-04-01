mod ast;
mod lexer;
mod parser;
mod analyzer;
mod codegen;
mod optimizer;
mod preprocess;
mod error_codes;
mod gpu;

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

    /// Number of render threads for framebuffer pipeline
    #[arg(long, default_value = "8")]
    render_threads: usize,

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

    /// Watch input file for changes and recompile automatically
    #[arg(long)]
    watch: bool,
}

fn main() {
    let cli = Cli::parse();

    if !cli.tape_size.is_power_of_two() {
        eprintln!("error: --tape-size must be a power of 2 (got {})", cli.tape_size);
        std::process::exit(1);
    }

    // Try GPU compiler acceleration (optional, --features gpu)
    let gpu_ctx = gpu::GpuCompiler::try_init();

    // Run the compilation pipeline once
    if compile(&cli, &gpu_ctx).is_err() && !cli.watch {
        std::process::exit(1);
    }

    // --watch: poll the input file for changes and recompile on modification
    if cli.watch {
        let mut last_modified = std::fs::metadata(&cli.input)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let current = match std::fs::metadata(&cli.input).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue, // file temporarily unavailable — retry
            };
            if current != last_modified {
                last_modified = current;
                eprintln!("Recompiling {}...", cli.input.display());
                let _ = compile(&cli, &gpu_ctx); // errors are printed but don't exit in watch mode
            }
        }
    }
}

/// Run the full compilation pipeline: read → preprocess → lex → parse → analyze →
/// optimize → codegen → C compile. Returns Err(()) on any stage failure (errors
/// already printed to stderr). Separated from main() to enable --watch recompilation.
fn compile(cli: &Cli, gpu_ctx: &Option<gpu::GpuCompiler>) -> Result<(), ()> {
    // --- Stage 1: Read source file ---
    let raw_source = std::fs::read_to_string(&cli.input).map_err(|e| {
        eprintln!("error: cannot read '{}': {}", cli.input.display(), e);
    })?;

    // --- Stage 2: Preprocess (expand !include directives, !define macros) ---
    let source = preprocess::preprocess(&raw_source, &cli.input, &cli.include_paths).map_err(|e| {
        eprintln!("{}", e);
    })?;

    // --- Stage 2.5: GPU pre-classification (optional, for sources >10KB) ---
    #[cfg(feature = "gpu")]
    if let Some(ref gpu) = gpu_ctx {
        if let Some(_classes) = gpu.classify_chars(source.as_bytes()) {
            eprintln!("bfpp: GPU classified {} bytes", source.len());
            // TODO: feed classification array to lexer for accelerated tokenization
        }
    }
    #[cfg(not(feature = "gpu"))]
    let _ = gpu_ctx; // suppress unused warning

    // --- Stage 3: Lex (tokenize the preprocessed source) ---
    let tokens = lexer::lex(&source).map_err(|e| {
        eprintln!("{}:{}", cli.input.display(), e);
    })?;

    // --- Stage 4: Parse (tokens → AST with coalesced counts) ---
    let program = parser::parse(&tokens).map_err(|e| {
        eprintln!("{}:{}", cli.input.display(), e);
    })?;

    // --- Stage 5: Static analysis (undefined sub calls, nesting errors, etc.) ---
    analyzer::analyze(&program.nodes).map_err(|errors| {
        for e in &errors {
            eprintln!("{}: {}", cli.input.display(), e);
        }
    })?;

    // --- Stage 6: Optimize (peephole passes on the AST) ---
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

    // Framebuffer bounds check
    if let Some((w, h)) = framebuffer {
        let fb_bytes = (w as usize) * (h as usize) * 3;
        if cli.tape_size < fb_bytes + 256 {
            eprintln!("error: tape size {} too small for {}x{} framebuffer (need at least {})",
                cli.tape_size, w, h, fb_bytes + 256);
            return Err(());
        }
    }

    // --- Stage 7: C code generation ---
    let opts = codegen::CodegenOptions {
        tape_size: cli.tape_size,
        stack_size: cli.stack_size,
        call_depth: cli.call_depth,
        framebuffer,
        eof_value: cli.eof,
        render_threads: cli.render_threads,
    };

    let codegen_result = codegen::generate(&optimized_program, &opts);
    let c_source = codegen_result.c_source;

    // Derive output paths from input stem or explicit -o flag.
    let stem = cli.input.file_stem().unwrap().to_string_lossy().to_string();
    let c_path = cli.output.as_ref()
        .map(|o| o.with_extension("c"))
        .unwrap_or_else(|| PathBuf::from(format!("{}.c", stem)));
    let bin_path = cli.output.as_ref()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(&stem));

    if cli.emit_c {
        std::fs::write(&c_path, &c_source).map_err(|e| {
            eprintln!("error: cannot write '{}': {}", c_path.display(), e);
        })?;
        println!("{}", c_path.display());
        return Ok(());
    }

    // --- Stage 8: C compilation ---
    let pid = std::process::id();
    let tmp_dir = PathBuf::from(format!("/tmp/bfpp_{}", pid));
    let parallel = codegen_result.split.is_some();

    // Build the cc command. In parallel mode: link .o files + runtime .c files.
    // In single mode: compile one .c file directly.
    let mut cc_cmd;
    let tmp_c; // single-file temp path (used for cleanup)

    if let Some(ref split) = codegen_result.split {
        // ── Parallel mode: compile subroutines in parallel, then link ──
        let _ = std::fs::create_dir_all(&tmp_dir);
        tmp_c = tmp_dir.join("_unused.c");

        // Write shared header
        std::fs::write(tmp_dir.join("bfpp_gen.h"), &split.header).map_err(|e| {
            eprintln!("error: cannot write header: {}", e);
        })?;

        // Write sub .c files + main .c file
        let mut c_files: Vec<PathBuf> = Vec::new();
        for (i, (name, src)) in split.subs.iter().enumerate() {
            let path = tmp_dir.join(format!("sub_{}_{}.c", i, name));
            std::fs::write(&path, src).map_err(|e| {
                eprintln!("error: cannot write sub file: {}", e);
            })?;
            c_files.push(path);
        }
        let main_path = tmp_dir.join("main.c");
        std::fs::write(&main_path, &split.main_source).map_err(|e| {
            eprintln!("error: cannot write main file: {}", e);
        })?;
        c_files.push(main_path);

        // Find runtime include dir (needed for compile step)
        let rt_inc = ["runtime", "."]
            .iter()
            .map(PathBuf::from)
            .chain(std::env::current_exe().ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime"))))
            .find(|d| d.join("bfpp_fb_pipeline.h").exists() || d.join("bfpp_rt.h").exists())
            .unwrap_or_default();
        let rt_inc_str = format!("-I{}", rt_inc.display());

        // Compile each .c → .o in parallel
        let cc_name = cli.cc.clone();
        let inc_dir = format!("-I{}", tmp_dir.display());
        let rt_inc_flag = rt_inc_str.clone();

        let handles: Vec<_> = c_files.iter().map(|c_file| {
            let c_file = c_file.clone();
            let cc = cc_name.clone();
            let inc = inc_dir.clone();
            let rt_inc = rt_inc_flag.clone();
            std::thread::spawn(move || {
                let obj = c_file.with_extension("o");
                let status = Command::new(&cc)
                    .args([
                        c_file.to_str().unwrap(),
                        "-c", "-o", obj.to_str().unwrap(),
                        "-O2", "-Wall",
                        "-Wno-unused-variable", "-Wno-unused-function",
                        &inc, &rt_inc, "-msse4.1",
                    ])
                    .status();
                (obj, status)
            })
        }).collect();

        let mut obj_files: Vec<PathBuf> = Vec::new();
        let mut compile_failed = false;
        for h in handles {
            let (obj, status) = h.join().unwrap();
            match status {
                Ok(s) if s.success() => obj_files.push(obj),
                _ => { compile_failed = true; }
            }
        }

        if compile_failed {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            eprintln!("error: parallel C compilation failed");
            return Err(());
        }

        // Link: cc *.o -o output + library flags + runtime .c files
        cc_cmd = Command::new(&cli.cc);
        for obj in &obj_files {
            cc_cmd.arg(obj.to_str().unwrap());
        }
        cc_cmd.args(["-o", bin_path.to_str().unwrap()]);

        eprintln!("bfpp: parallel compile ({} TUs on {} threads)",
                  split.subs.len() + 1,
                  split.subs.len() + 1);
    } else {
        // ── Single-file mode (existing behavior) ──
        tmp_c = PathBuf::from(format!("/tmp/bfpp_{}.c", pid));
        std::fs::write(&tmp_c, &c_source).map_err(|e| {
            eprintln!("error: cannot write temp file: {}", e);
        })?;

        cc_cmd = Command::new(&cli.cc);
        cc_cmd.args([
            tmp_c.to_str().unwrap(),
            "-o", bin_path.to_str().unwrap(),
            "-O2", "-Wall",
            "-Wno-unused-variable", "-Wno-unused-function",
        ]);
    }

    // ── Shared flags: library linking + runtime .c files ──
    // These apply to BOTH parallel (link step) and single-file (compile+link step).

    // AVX2 + FMA for x86_64 SIMD paths in runtime (guarded by #ifdef __AVX2__)
    #[cfg(target_arch = "x86_64")]
    cc_cmd.args(["-mavx2", "-mfma"]);

    // -lSDL2 + pipeline runtime
    if codegen_result.uses_fb_pipeline {
        cc_cmd.args(["-lSDL2", "-pthread", "-msse4.1"]);
        let runtime_paths = [
            PathBuf::from("runtime"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime")))
                .unwrap_or_default(),
        ];
        for rt_dir in &runtime_paths {
            if rt_dir.join("bfpp_fb_pipeline.c").exists() {
                cc_cmd.arg(format!("-I{}", rt_dir.display()));
                cc_cmd.arg(rt_dir.join("bfpp_fb_pipeline.c").to_str().unwrap());
                // Terminal fallback for headless/SSH rendering
                let term_path = rt_dir.join("bfpp_fb_terminal.c");
                if term_path.exists() {
                    cc_cmd.arg(term_path.to_str().unwrap());
                }
                break;
            }
        }
    }

    // -ldl for FFI
    if codegen_result.uses_ffi {
        cc_cmd.args(["-ldl"]);
    }

    // 3D rendering runtime
    if codegen_result.uses_3d {
        cc_cmd.args(["-lGL", "-lGLEW", "-lm"]);
        if !codegen_result.uses_fb_pipeline {
            cc_cmd.args(["-lSDL2", "-pthread", "-msse4.1"]);
        }
        let runtime_paths = [
            PathBuf::from("runtime"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime")))
                .unwrap_or_default(),
        ];
        let rt_3d_files = [
            "bfpp_rt_3d.c",
            "bfpp_rt_3d_math.c",
            "bfpp_rt_3d_meshgen.c",
            "bfpp_rt_3d_software.c",
        ];
        for rt_dir in &runtime_paths {
            if rt_dir.join("bfpp_rt_3d.c").exists() {
                cc_cmd.arg(format!("-I{}", rt_dir.display()));
                for f in &rt_3d_files {
                    let path = rt_dir.join(f);
                    if path.exists() {
                        cc_cmd.arg(path.to_str().unwrap());
                    }
                }
                break;
            }
        }
    }

    // Multi-GPU + Scene Oracle runtime
    if codegen_result.uses_multigpu {
        cc_cmd.arg("-lEGL");
        // Link libnuma if available (for NUMA-aware staging buffers on EPYC)
        // Check existence before linking to avoid hard dependency
        if std::path::Path::new("/usr/lib/libnuma.so").exists()
            || std::path::Path::new("/usr/lib64/libnuma.so").exists()
            || std::path::Path::new("/usr/lib/x86_64-linux-gnu/libnuma.so").exists()
        {
            cc_cmd.arg("-lnuma");
        }
        let runtime_paths = [
            PathBuf::from("runtime"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime")))
                .unwrap_or_default(),
        ];
        let mgpu_files = [
            "bfpp_rt_3d_multigpu.c",
            "bfpp_rt_3d_oracle.c",
        ];
        for rt_dir in &runtime_paths {
            if rt_dir.join("bfpp_rt_3d_multigpu.c").exists() {
                cc_cmd.arg(format!("-I{}", rt_dir.display()));
                for f in &mgpu_files {
                    let path = rt_dir.join(f);
                    if path.exists() {
                        cc_cmd.arg(path.to_str().unwrap());
                    }
                }
                break;
            }
        }
    }

    // OpenCL GPU compute runtime (loaded via dlopen — no link-time dependency)
    if codegen_result.uses_opencl {
        cc_cmd.arg("-ldl"); // for dlopen/dlsym
        let runtime_paths = [
            PathBuf::from("runtime"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime")))
                .unwrap_or_default(),
        ];
        for rt_dir in &runtime_paths {
            if rt_dir.join("bfpp_rt_opencl.c").exists() {
                cc_cmd.arg(format!("-I{}", rt_dir.display()));
                cc_cmd.arg(rt_dir.join("bfpp_rt_opencl.c").to_str().unwrap());
                break;
            }
        }
    }

    // Threading runtime
    if codegen_result.uses_threading {
        if !codegen_result.uses_fb_pipeline {
            cc_cmd.arg("-pthread");
        }
        let runtime_paths = [
            PathBuf::from("runtime"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("runtime")))
                .unwrap_or_default(),
        ];
        for rt_dir in &runtime_paths {
            if rt_dir.join("bfpp_rt_parallel.c").exists() {
                cc_cmd.arg(format!("-I{}", rt_dir.display()));
                cc_cmd.arg(rt_dir.join("bfpp_rt_parallel.c").to_str().unwrap());
                break;
            }
        }
    }

    // TUI runtime
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
            if parallel { let _ = std::fs::remove_dir_all(&tmp_dir); }
            else { let _ = std::fs::remove_file(&tmp_c); }
            return Err(());
        }
    };

    // Cleanup temp files
    if parallel {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    } else {
        let _ = std::fs::remove_file(&tmp_c);
    }

    if !status.success() {
        eprintln!("error: C compilation failed");
        return Err(());
    }

    Ok(())
}
