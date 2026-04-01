// BF++ Compiler GPU Acceleration via OpenCL
//
// Optional module (enabled with --features gpu / cargo build --features gpu).
// Accelerates lexing and optimizer pattern matching for large source files.
//
// When OpenCL is unavailable or source is small (<10KB), falls back to CPU.
// The GPU path produces identical results to the CPU path.

#[cfg(feature = "gpu")]
use opencl3::command_queue::{CommandQueue, CL_QUEUE_PROFILING_ENABLE};
#[cfg(feature = "gpu")]
use opencl3::context::Context;
#[cfg(feature = "gpu")]
use opencl3::device::{get_all_devices, Device, CL_DEVICE_TYPE_GPU};
#[cfg(feature = "gpu")]
use opencl3::kernel::{ExecuteKernel, Kernel};
#[cfg(feature = "gpu")]
use opencl3::memory::{Buffer, CL_MEM_READ_ONLY, CL_MEM_READ_WRITE};
#[cfg(feature = "gpu")]
use opencl3::program::Program;
#[cfg(feature = "gpu")]
use opencl3::types::{cl_uchar, CL_BLOCKING};

#[allow(dead_code)]
/// Minimum source size (bytes) to justify GPU dispatch overhead.
const GPU_THRESHOLD: usize = 10240;

/// OpenCL kernel for parallel character classification.
/// Each work-item classifies one source byte into a token type code.
#[cfg(feature = "gpu")]
const CLASSIFY_KERNEL: &str = r#"
__kernel void classify(__global const uchar *source,
                       __global uchar *classes,
                       int len) {
    int gid = get_global_id(0);
    if (gid >= len) return;

    uchar ch = source[gid];
    uchar cl = 0; // 0 = ignore

    switch (ch) {
        case '>': cl = 1; break;
        case '<': cl = 2; break;
        case '+': cl = 3; break;
        case '-': cl = 4; break;
        case '.': cl = 5; break;
        case ',': cl = 6; break;
        case '[': cl = 7; break;
        case ']': cl = 8; break;
        case '$': cl = 9; break;
        case '~': cl = 10; break;
        case '^': cl = 11; break;
        case 'E': cl = 12; break;
        case 'e': cl = 13; break;
        case '@': cl = 14; break;
        case '*': cl = 15; break;
        case 'T': cl = 16; break;
        case 'F': cl = 17; break;
        case '|': cl = 18; break;
        case '&': cl = 19; break;
        case 'x': cl = 20; break;
        case 's': cl = 21; break;
        case 'r': cl = 22; break;
        case 'n': cl = 23; break;
        case '?': cl = 24; break;
        case '#': cl = 25; break;
        case '%': cl = 26; break;
        case '!': cl = 27; break;
        case '"': cl = 28; break;
        case ';': cl = 29; break; // comment start
        case '\n': cl = 30; break;
        case '{': cl = 31; break;
        case '}': cl = 32; break;
        case ':': cl = 33; break;
        default: cl = 0; break;
    }

    classes[gid] = cl;
}
"#;

/// OpenCL kernel for parallel pattern detection in classified token arrays.
/// Detects clear-loop ([-]) and scan-loop ([>], [<]) patterns.
#[cfg(feature = "gpu")]
const PATTERN_KERNEL: &str = r#"
__kernel void detect_patterns(__global const uchar *classes,
                              __global uchar *patterns,
                              int len) {
    int gid = get_global_id(0);
    if (gid + 2 >= len) return;

    // Detect [-] pattern: LOOP_START(7), DEC(4), LOOP_END(8)
    if (classes[gid] == 7 && classes[gid+1] == 4 && classes[gid+2] == 8) {
        patterns[gid] = 1; // clear-loop
        return;
    }
    // Detect [+] pattern: LOOP_START(7), INC(3), LOOP_END(8)
    if (classes[gid] == 7 && classes[gid+1] == 3 && classes[gid+2] == 8) {
        patterns[gid] = 1; // also clear-loop
        return;
    }
    // Detect [>] pattern: LOOP_START(7), MOVE_R(1), LOOP_END(8)
    if (classes[gid] == 7 && classes[gid+1] == 1 && classes[gid+2] == 8) {
        patterns[gid] = 2; // scan-right
        return;
    }
    // Detect [<] pattern: LOOP_START(7), MOVE_L(2), LOOP_END(8)
    if (classes[gid] == 7 && classes[gid+1] == 2 && classes[gid+2] == 8) {
        patterns[gid] = 3; // scan-left
        return;
    }
    patterns[gid] = 0;
}
"#;

/// GPU compiler context. Initialized once, reused across compilation stages.
#[cfg(feature = "gpu")]
pub struct GpuCompiler {
    context: Context,
    queue: CommandQueue,
    classify_kernel: Kernel,
    pattern_kernel: Kernel,
    device_name: String,
}

#[cfg(feature = "gpu")]
impl GpuCompiler {
    /// Try to initialize GPU compiler acceleration.
    /// Returns None if no GPU available or OpenCL fails.
    pub fn try_init() -> Option<Self> {
        let devices = get_all_devices(CL_DEVICE_TYPE_GPU).ok()?;
        if devices.is_empty() { return None; }

        let device = Device::new(devices[0]);
        let device_name = device.name().unwrap_or_default();
        let context = Context::from_device(&device).ok()?;
        let queue = CommandQueue::create_default_with_properties(
            &context, CL_QUEUE_PROFILING_ENABLE, 0
        ).ok()?;

        // Compile kernels
        let classify_prog = Program::create_and_build_from_source(&context, CLASSIFY_KERNEL, "")
            .ok()?;
        let classify_kernel = Kernel::create(&classify_prog, "classify").ok()?;

        let pattern_prog = Program::create_and_build_from_source(&context, PATTERN_KERNEL, "")
            .ok()?;
        let pattern_kernel = Kernel::create(&pattern_prog, "detect_patterns").ok()?;

        eprintln!("bfpp: GPU compiler acceleration enabled ({})", device_name);

        Some(Self {
            context,
            queue,
            classify_kernel,
            pattern_kernel,
            device_name,
        })
    }

    /// GPU-accelerated character classification.
    /// Returns a Vec<u8> where each byte is the token class of the corresponding source byte.
    pub fn classify_chars(&self, source: &[u8]) -> Option<Vec<u8>> {
        if source.len() < GPU_THRESHOLD { return None; }

        let len = source.len();
        let mut src_buf = unsafe {
            Buffer::<cl_uchar>::create(&self.context, CL_MEM_READ_ONLY, len, std::ptr::null_mut())
        }.ok()?;

        let mut cls_buf = unsafe {
            Buffer::<cl_uchar>::create(&self.context, CL_MEM_READ_WRITE, len, std::ptr::null_mut())
        }.ok()?;

        // Upload source
        let _write_evt = unsafe {
            self.queue.enqueue_write_buffer(&mut src_buf, CL_BLOCKING, 0, source, &[])
        }.ok()?;

        // Dispatch classification kernel
        let len_i32 = len as i32;
        let global_size = len;
        let _exec_evt = unsafe {
            ExecuteKernel::new(&self.classify_kernel)
                .set_arg(&src_buf)
                .set_arg(&cls_buf)
                .set_arg(&len_i32)
                .set_global_work_size(global_size)
                .enqueue_nd_range(&self.queue)
        }.ok()?;

        // Read back results
        let mut result = vec![0u8; len];
        let _read_evt = unsafe {
            self.queue.enqueue_read_buffer(&cls_buf, CL_BLOCKING, 0, &mut result, &[])
        }.ok()?;

        Some(result)
    }

    /// GPU-accelerated pattern detection on classified token arrays.
    pub fn detect_patterns(&self, classes: &[u8]) -> Option<Vec<u8>> {
        if classes.len() < GPU_THRESHOLD { return None; }

        let len = classes.len();
        let mut cls_buf = unsafe {
            Buffer::<cl_uchar>::create(&self.context, CL_MEM_READ_ONLY, len, std::ptr::null_mut())
        }.ok()?;

        let mut pat_buf = unsafe {
            Buffer::<cl_uchar>::create(&self.context, CL_MEM_READ_WRITE, len, std::ptr::null_mut())
        }.ok()?;

        let _write_evt = unsafe {
            self.queue.enqueue_write_buffer(&mut cls_buf, CL_BLOCKING, 0, classes, &[])
        }.ok()?;

        let len_i32 = len as i32;
        let _exec_evt = unsafe {
            ExecuteKernel::new(&self.pattern_kernel)
                .set_arg(&cls_buf)
                .set_arg(&pat_buf)
                .set_arg(&len_i32)
                .set_global_work_size(len)
                .enqueue_nd_range(&self.queue)
        }.ok()?;

        let mut result = vec![0u8; len];
        let _read_evt = unsafe {
            self.queue.enqueue_read_buffer(&pat_buf, CL_BLOCKING, 0, &mut result, &[])
        }.ok()?;

        Some(result)
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

/// CPU fallback for character classification (used when GPU unavailable).
#[allow(dead_code)]
pub fn classify_chars_cpu(source: &[u8]) -> Vec<u8> {
    source.iter().map(|&ch| match ch {
        b'>' => 1, b'<' => 2, b'+' => 3, b'-' => 4,
        b'.' => 5, b',' => 6, b'[' => 7, b']' => 8,
        b'$' => 9, b'~' => 10, b'^' => 11, b'E' => 12,
        b'e' => 13, b'@' => 14, b'*' => 15, b'T' => 16,
        b'F' => 17, b'|' => 18, b'&' => 19, b'x' => 20,
        b's' => 21, b'r' => 22, b'n' => 23, b'?' => 24,
        b'#' => 25, b'%' => 26, b'!' => 27, b'"' => 28,
        b';' => 29, b'\n' => 30, b'{' => 31, b'}' => 32,
        b':' => 33,
        _ => 0,
    }).collect()
}

/// Stub for non-GPU builds. Always returns None.
#[cfg(not(feature = "gpu"))]
pub struct GpuCompiler;

#[cfg(not(feature = "gpu"))]
impl GpuCompiler {
    pub fn try_init() -> Option<Self> { None }
}
