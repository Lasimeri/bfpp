/*
 * bfpp_rt_opencl.c — GPU compute offloading for BF++ via OpenCL.
 *
 * Architecture:
 *   OpenCL loaded at runtime via dlopen — no compile-time dependency.
 *   Up to 8 GPUs with per-device command queues.
 *   Async dispatch with event-based completion tracking.
 *   Tape regions are uploaded to GPU buffers, processed, read back.
 *
 * Linking: no -lOpenCL needed. dlopen("libOpenCL.so") at runtime.
 *   Programs compile and run without OpenCL installed (intrinsics = no-ops).
 */

#include "bfpp_rt_opencl.h"
#include "bfpp_rt_opencl_kernels.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdatomic.h>
#include <dlfcn.h>

/* ── OpenCL type definitions (avoid requiring CL headers) ────── */

typedef int32_t   cl_int;
typedef uint32_t  cl_uint;
typedef uint64_t  cl_ulong;
typedef void *    cl_platform_id;
typedef void *    cl_device_id;
typedef void *    cl_context;
typedef void *    cl_command_queue;
typedef void *    cl_program;
typedef void *    cl_kernel;
typedef void *    cl_mem;
typedef void *    cl_event;
typedef uint64_t  cl_mem_flags;
typedef uint64_t  cl_command_queue_properties;

#define CL_SUCCESS                0
#define CL_MEM_READ_WRITE        (1 << 0)
#define CL_MEM_COPY_HOST_PTR     (1 << 5)
#define CL_TRUE                  1
#define CL_FALSE                 0
#define CL_DEVICE_TYPE_GPU       (1 << 2)
#define CL_DEVICE_TYPE_ALL       0xFFFFFFFF
#define CL_COMPLETE              0
#define CL_EVENT_COMMAND_EXECUTION_STATUS 0x11D3
#define CL_QUEUE_PROFILING_ENABLE (1 << 1)

/* ── dlopen function pointers ────────────────────────────────── */

typedef cl_int (*fn_clGetPlatformIDs)(cl_uint, cl_platform_id *, cl_uint *);
typedef cl_int (*fn_clGetDeviceIDs)(cl_platform_id, cl_ulong, cl_uint, cl_device_id *, cl_uint *);
typedef cl_context (*fn_clCreateContext)(void *, cl_uint, const cl_device_id *, void *, void *, cl_int *);
typedef cl_command_queue (*fn_clCreateCommandQueue)(cl_context, cl_device_id, cl_command_queue_properties, cl_int *);
typedef cl_program (*fn_clCreateProgramWithSource)(cl_context, cl_uint, const char **, const size_t *, cl_int *);
typedef cl_int (*fn_clBuildProgram)(cl_program, cl_uint, const cl_device_id *, const char *, void *, void *);
typedef cl_kernel (*fn_clCreateKernel)(cl_program, const char *, cl_int *);
typedef cl_int (*fn_clSetKernelArg)(cl_kernel, cl_uint, size_t, const void *);
typedef cl_int (*fn_clEnqueueNDRangeKernel)(cl_command_queue, cl_kernel, cl_uint, const size_t *, const size_t *, const size_t *, cl_uint, const cl_event *, cl_event *);
typedef cl_int (*fn_clEnqueueWriteBuffer)(cl_command_queue, cl_mem, cl_int, size_t, size_t, const void *, cl_uint, const cl_event *, cl_event *);
typedef cl_int (*fn_clEnqueueReadBuffer)(cl_command_queue, cl_mem, cl_int, size_t, size_t, void *, cl_uint, const cl_event *, cl_event *);
typedef cl_mem (*fn_clCreateBuffer)(cl_context, cl_mem_flags, size_t, void *, cl_int *);
typedef cl_int (*fn_clReleaseMemObject)(cl_mem);
typedef cl_int (*fn_clReleaseKernel)(cl_kernel);
typedef cl_int (*fn_clReleaseProgram)(cl_program);
typedef cl_int (*fn_clReleaseCommandQueue)(cl_command_queue);
typedef cl_int (*fn_clReleaseContext)(cl_context);
typedef cl_int (*fn_clWaitForEvents)(cl_uint, const cl_event *);
typedef cl_int (*fn_clGetEventInfo)(cl_event, cl_uint, size_t, void *, size_t *);
typedef cl_int (*fn_clReleaseEvent)(cl_event);
typedef cl_int (*fn_clFlush)(cl_command_queue);
typedef cl_int (*fn_clFinish)(cl_command_queue);
typedef cl_int (*fn_clGetProgramBuildInfo)(cl_program, cl_device_id, cl_uint, size_t, void *, size_t *);

/* ── Function pointer table ──────────────────────────────────── */

static struct {
    void *lib;
    fn_clGetPlatformIDs       GetPlatformIDs;
    fn_clGetDeviceIDs         GetDeviceIDs;
    fn_clCreateContext         CreateContext;
    fn_clCreateCommandQueue    CreateCommandQueue;
    fn_clCreateProgramWithSource CreateProgramWithSource;
    fn_clBuildProgram          BuildProgram;
    fn_clCreateKernel          CreateKernel;
    fn_clSetKernelArg          SetKernelArg;
    fn_clEnqueueNDRangeKernel  EnqueueNDRangeKernel;
    fn_clEnqueueWriteBuffer    EnqueueWriteBuffer;
    fn_clEnqueueReadBuffer     EnqueueReadBuffer;
    fn_clCreateBuffer          CreateBuffer;
    fn_clReleaseMemObject      ReleaseMemObject;
    fn_clReleaseKernel         ReleaseKernel;
    fn_clReleaseProgram        ReleaseProgram;
    fn_clReleaseCommandQueue   ReleaseCommandQueue;
    fn_clReleaseContext        ReleaseContext;
    fn_clWaitForEvents         WaitForEvents;
    fn_clGetEventInfo          GetEventInfo;
    fn_clReleaseEvent          ReleaseEvent;
    fn_clFlush                 Flush;
    fn_clFinish                Finish;
    fn_clGetProgramBuildInfo   GetProgramBuildInfo;
} cl;

/* ── Per-device state ────────────────────────────────────────── */

#define BFPP_CL_MAX_DEVICES 8

typedef struct {
    cl_device_id   device;
    cl_context     context;
    cl_command_queue queue;
    cl_mem         tape_buf;      /* GPU-side copy of tape region */
    int            tape_buf_size;
    int            pending_ops;
} bfpp_cl_device_t;

/* ── Async operation tracking ────────────────────────────────── */

#define BFPP_CL_MAX_OPS 256

typedef struct {
    cl_event    event;
    int         device_idx;
    int         tape_offset;
    int         size;
    int         active;
} bfpp_cl_op_t;

/* ── Global state ────────────────────────────────────────────── */

static struct {
    int               initialized;
    int               device_count;
    bfpp_cl_device_t  devices[BFPP_CL_MAX_DEVICES];

    /* Compiled kernels (shared across devices via same source) */
    cl_program        programs[BFPP_CL_MAX_DEVICES];
    cl_kernel         k_memset[BFPP_CL_MAX_DEVICES];
    cl_kernel         k_memcpy[BFPP_CL_MAX_DEVICES];
    cl_kernel         k_reduce[BFPP_CL_MAX_DEVICES];
    cl_kernel         k_blur[BFPP_CL_MAX_DEVICES];
    cl_kernel         k_rasterize[BFPP_CL_MAX_DEVICES];
    cl_kernel         k_transform[BFPP_CL_MAX_DEVICES];

    /* Async operation pool */
    bfpp_cl_op_t      ops[BFPP_CL_MAX_OPS];
    atomic_int        next_op;

    /* Load balancing */
    int               robin;
} ocl;

/* ── Error register ──────────────────────────────────────────── */

extern int bfpp_err;
#define BFPP_ERR_GENERIC 1

/* ── Tape helpers ────────────────────────────────────────────── */

static inline uint32_t tape_u32(const uint8_t *tape, int addr) {
    return (uint32_t)tape[addr] | ((uint32_t)tape[addr+1]<<8)
         | ((uint32_t)tape[addr+2]<<16) | ((uint32_t)tape[addr+3]<<24);
}

static inline void tape_set_u32(uint8_t *tape, int addr, uint32_t val) {
    tape[addr] = val & 0xFF;
    tape[addr+1] = (val >> 8) & 0xFF;
    tape[addr+2] = (val >> 16) & 0xFF;
    tape[addr+3] = (val >> 24) & 0xFF;
}

/* ── Comparator for qsort (uint32 LE) ────────────────────────── */

static int cmp_u32_le(const void *a, const void *b) {
    const uint8_t *pa = (const uint8_t *)a, *pb = (const uint8_t *)b;
    uint32_t va = pa[0] | ((uint32_t)pa[1]<<8) | ((uint32_t)pa[2]<<16) | ((uint32_t)pa[3]<<24);
    uint32_t vb = pb[0] | ((uint32_t)pb[1]<<8) | ((uint32_t)pb[2]<<16) | ((uint32_t)pb[3]<<24);
    return (va > vb) - (va < vb);
}

/* ── dlopen loader ───────────────────────────────────────────── */

static int load_opencl(void) {
    cl.lib = dlopen("libOpenCL.so", RTLD_LAZY);
    if (!cl.lib) cl.lib = dlopen("libOpenCL.so.1", RTLD_LAZY);
    if (!cl.lib) return 0;

    #define LOAD(name) cl.name = (fn_cl##name)dlsym(cl.lib, "cl" #name); if (!cl.name) return 0;
    LOAD(GetPlatformIDs)
    LOAD(GetDeviceIDs)
    LOAD(CreateContext)
    LOAD(CreateCommandQueue)
    LOAD(CreateProgramWithSource)
    LOAD(BuildProgram)
    LOAD(CreateKernel)
    LOAD(SetKernelArg)
    LOAD(EnqueueNDRangeKernel)
    LOAD(EnqueueWriteBuffer)
    LOAD(EnqueueReadBuffer)
    LOAD(CreateBuffer)
    LOAD(ReleaseMemObject)
    LOAD(ReleaseKernel)
    LOAD(ReleaseProgram)
    LOAD(ReleaseCommandQueue)
    LOAD(ReleaseContext)
    LOAD(WaitForEvents)
    LOAD(GetEventInfo)
    LOAD(ReleaseEvent)
    LOAD(Flush)
    LOAD(Finish)
    LOAD(GetProgramBuildInfo)
    #undef LOAD

    return 1;
}

/* ── Kernel compilation ──────────────────────────────────────── */

static cl_kernel compile_kernel(int dev_idx, const char *source, const char *name) {
    cl_int err;
    bfpp_cl_device_t *dev = &ocl.devices[dev_idx];
    size_t len = strlen(source);

    cl_program prog = cl.CreateProgramWithSource(dev->context, 1, &source, &len, &err);
    if (err != CL_SUCCESS) return NULL;

    err = cl.BuildProgram(prog, 1, &dev->device, "-cl-fast-relaxed-math", NULL, NULL);
    if (err != CL_SUCCESS) {
        char log[4096];
        cl.GetProgramBuildInfo(prog, dev->device, 0x1183 /*CL_PROGRAM_BUILD_LOG*/,
                               sizeof(log), log, NULL);
        fprintf(stderr, "bfpp_opencl: build error for '%s': %s\n", name, log);
        cl.ReleaseProgram(prog);
        return NULL;
    }

    cl_kernel k = cl.CreateKernel(prog, name, &err);
    cl.ReleaseProgram(prog);
    return (err == CL_SUCCESS) ? k : NULL;
}

static void compile_all_kernels(int dev_idx) {
    ocl.k_memset[dev_idx]    = compile_kernel(dev_idx, BFPP_CL_MEMSET, "bfpp_memset");
    ocl.k_memcpy[dev_idx]    = compile_kernel(dev_idx, BFPP_CL_MEMCPY, "bfpp_memcpy");
    ocl.k_reduce[dev_idx]    = compile_kernel(dev_idx, BFPP_CL_REDUCE, "bfpp_reduce");
    ocl.k_blur[dev_idx]      = compile_kernel(dev_idx, BFPP_CL_BLUR, "bfpp_blur");
    ocl.k_transform[dev_idx] = compile_kernel(dev_idx, BFPP_CL_TRANSFORM, "bfpp_transform");
    /* Rasterizer kernel compiled on-demand (needs scene-specific params) */
}

/* ── Load balancing ──────────────────────────────────────────── */

static int pick_device(void) {
    if (ocl.device_count == 0) return -1;
    int idx = ocl.robin;
    ocl.robin = (ocl.robin + 1) % ocl.device_count;
    return idx;
}

/* ── Operation tracking ──────────────────────────────────────── */

static int alloc_op(int dev_idx, cl_event evt, int tape_offset, int size) {
    int idx = atomic_fetch_add(&ocl.next_op, 1) % BFPP_CL_MAX_OPS;
    ocl.ops[idx].event = evt;
    ocl.ops[idx].device_idx = dev_idx;
    ocl.ops[idx].tape_offset = tape_offset;
    ocl.ops[idx].size = size;
    ocl.ops[idx].active = 1;
    return idx;
}

/* ── Public API ──────────────────────────────────────────────── */

int bfpp_opencl_init(void) {
    memset(&ocl, 0, sizeof(ocl));

    if (!load_opencl()) {
        fprintf(stderr, "bfpp_opencl: libOpenCL.so not found — GPU compute disabled\n");
        return 0;
    }

    /* Enumerate platforms and devices */
    cl_platform_id platforms[8];
    cl_uint num_platforms;
    if (cl.GetPlatformIDs(8, platforms, &num_platforms) != CL_SUCCESS || num_platforms == 0) {
        fprintf(stderr, "bfpp_opencl: no OpenCL platforms found\n");
        return 0;
    }

    ocl.device_count = 0;
    for (cl_uint p = 0; p < num_platforms && ocl.device_count < BFPP_CL_MAX_DEVICES; p++) {
        cl_device_id devs[BFPP_CL_MAX_DEVICES];
        cl_uint num_devs;
        if (cl.GetDeviceIDs(platforms[p], CL_DEVICE_TYPE_GPU,
                            BFPP_CL_MAX_DEVICES - ocl.device_count,
                            devs, &num_devs) != CL_SUCCESS)
            continue;

        for (cl_uint d = 0; d < num_devs && ocl.device_count < BFPP_CL_MAX_DEVICES; d++) {
            cl_int err;
            bfpp_cl_device_t *dev = &ocl.devices[ocl.device_count];
            dev->device = devs[d];

            dev->context = cl.CreateContext(NULL, 1, &dev->device, NULL, NULL, &err);
            if (err != CL_SUCCESS) continue;

            dev->queue = cl.CreateCommandQueue(dev->context, dev->device, 0, &err);
            if (err != CL_SUCCESS) { cl.ReleaseContext(dev->context); continue; }

            /* Compile kernels for this device */
            compile_all_kernels(ocl.device_count);

            ocl.device_count++;
        }
    }

    if (ocl.device_count > 0) {
        ocl.initialized = 1;
        fprintf(stderr, "bfpp_opencl: %d GPU compute device(s) initialized\n", ocl.device_count);
    }

    return ocl.device_count;
}

void bfpp_opencl_cleanup(void) {
    if (!ocl.initialized) return;

    for (int i = 0; i < ocl.device_count; i++) {
        bfpp_cl_device_t *dev = &ocl.devices[i];
        if (dev->tape_buf) cl.ReleaseMemObject(dev->tape_buf);
        if (ocl.k_memset[i]) cl.ReleaseKernel(ocl.k_memset[i]);
        if (ocl.k_memcpy[i]) cl.ReleaseKernel(ocl.k_memcpy[i]);
        if (ocl.k_reduce[i]) cl.ReleaseKernel(ocl.k_reduce[i]);
        if (ocl.k_blur[i]) cl.ReleaseKernel(ocl.k_blur[i]);
        if (ocl.k_transform[i]) cl.ReleaseKernel(ocl.k_transform[i]);
        if (ocl.k_rasterize[i]) cl.ReleaseKernel(ocl.k_rasterize[i]);
        if (dev->queue) cl.ReleaseCommandQueue(dev->queue);
        if (dev->context) cl.ReleaseContext(dev->context);
    }

    if (cl.lib) dlclose(cl.lib);
    memset(&ocl, 0, sizeof(ocl));
}

int bfpp_opencl_device_count(void) { return ocl.device_count; }
int bfpp_opencl_available(void) { return ocl.initialized; }

/* ── Ensure GPU buffer is large enough ───────────────────────── */

static cl_mem ensure_buf(int dev_idx, int size) {
    bfpp_cl_device_t *dev = &ocl.devices[dev_idx];
    if (dev->tape_buf && dev->tape_buf_size >= size)
        return dev->tape_buf;
    if (dev->tape_buf) cl.ReleaseMemObject(dev->tape_buf);
    cl_int err;
    dev->tape_buf = cl.CreateBuffer(dev->context, CL_MEM_READ_WRITE, size, NULL, &err);
    dev->tape_buf_size = (err == CL_SUCCESS) ? size : 0;
    return dev->tape_buf;
}

/* ── Async memset ────────────────────────────────────────────── */

int bfpp_opencl_memset(uint8_t *tape, int offset, uint8_t value, int size) {
    if (!ocl.initialized || size < 65536) return -1;  /* below threshold */
    int di = pick_device();
    if (di < 0) return -1;
    bfpp_cl_device_t *dev = &ocl.devices[di];

    cl_mem buf = ensure_buf(di, offset + size);
    if (!buf) return -1;

    cl.SetKernelArg(ocl.k_memset[di], 0, sizeof(cl_mem), &buf);
    cl.SetKernelArg(ocl.k_memset[di], 1, sizeof(int), &offset);
    cl.SetKernelArg(ocl.k_memset[di], 2, sizeof(uint8_t), &value);
    cl.SetKernelArg(ocl.k_memset[di], 3, sizeof(int), &size);

    size_t global = (size_t)size;
    cl_event evt;
    cl.EnqueueNDRangeKernel(dev->queue, ocl.k_memset[di], 1, NULL, &global, NULL, 0, NULL, &evt);

    /* Read back */
    cl_event read_evt;
    cl.EnqueueReadBuffer(dev->queue, buf, CL_FALSE, offset, size, tape + offset, 1, &evt, &read_evt);
    cl.ReleaseEvent(evt);
    cl.Flush(dev->queue);

    return alloc_op(di, read_evt, offset, size);
}

/* ── Async blur ──────────────────────────────────────────────── */

int bfpp_opencl_blur(uint8_t *tape, int fb_offset, int width, int height, int radius) {
    if (!ocl.initialized) return -1;
    int di = pick_device();
    if (di < 0) return -1;
    bfpp_cl_device_t *dev = &ocl.devices[di];

    int fb_size = width * height * 3;
    cl_mem buf = ensure_buf(di, fb_offset + fb_size);
    if (!buf) return -1;

    /* Upload FB region */
    cl_event upload_evt;
    cl.EnqueueWriteBuffer(dev->queue, buf, CL_FALSE, fb_offset, fb_size,
                          tape + fb_offset, 0, NULL, &upload_evt);

    cl.SetKernelArg(ocl.k_blur[di], 0, sizeof(cl_mem), &buf);
    cl.SetKernelArg(ocl.k_blur[di], 1, sizeof(int), &fb_offset);
    cl.SetKernelArg(ocl.k_blur[di], 2, sizeof(int), &width);
    cl.SetKernelArg(ocl.k_blur[di], 3, sizeof(int), &height);
    cl.SetKernelArg(ocl.k_blur[di], 4, sizeof(int), &radius);

    size_t global[2] = { (size_t)width, (size_t)height };
    cl_event kern_evt;
    cl.EnqueueNDRangeKernel(dev->queue, ocl.k_blur[di], 2, NULL, global, NULL,
                            1, &upload_evt, &kern_evt);
    cl.ReleaseEvent(upload_evt);

    /* Read back */
    cl_event read_evt;
    cl.EnqueueReadBuffer(dev->queue, buf, CL_FALSE, fb_offset, fb_size,
                          tape + fb_offset, 1, &kern_evt, &read_evt);
    cl.ReleaseEvent(kern_evt);
    cl.Flush(dev->queue);

    return alloc_op(di, read_evt, fb_offset, fb_size);
}

/* ── Completion ──────────────────────────────────────────────── */

int bfpp_opencl_poll(int handle) {
    if (handle < 0 || handle >= BFPP_CL_MAX_OPS || !ocl.ops[handle].active) return 1;
    cl_int status;
    cl.GetEventInfo(ocl.ops[handle].event, CL_EVENT_COMMAND_EXECUTION_STATUS,
                    sizeof(status), &status, NULL);
    if (status == CL_COMPLETE) {
        cl.ReleaseEvent(ocl.ops[handle].event);
        ocl.ops[handle].active = 0;
        return 1;
    }
    return 0;
}

void bfpp_opencl_wait(int handle) {
    if (handle < 0 || handle >= BFPP_CL_MAX_OPS || !ocl.ops[handle].active) return;
    cl.WaitForEvents(1, &ocl.ops[handle].event);
    cl.ReleaseEvent(ocl.ops[handle].event);
    ocl.ops[handle].active = 0;
}

/* ── Async memcpy (non-overlapping) ───────────────────────────── */

int bfpp_opencl_memcpy(uint8_t *tape, int dst, int src, int size) {
    if (!ocl.initialized || size < 65536) return -1;
    int di = pick_device();
    if (di < 0) return -1;
    bfpp_cl_device_t *dev = &ocl.devices[di];

    int max_addr = (dst + size > src + size) ? dst + size : src + size;
    cl_mem buf = ensure_buf(di, max_addr);
    if (!buf) return -1;

    /* Upload source region */
    cl_event up_evt;
    cl.EnqueueWriteBuffer(dev->queue, buf, CL_FALSE, src, size, tape + src, 0, NULL, &up_evt);

    cl.SetKernelArg(ocl.k_memcpy[di], 0, sizeof(cl_mem), &buf);
    cl.SetKernelArg(ocl.k_memcpy[di], 1, sizeof(int), &dst);
    cl.SetKernelArg(ocl.k_memcpy[di], 2, sizeof(int), &src);
    cl.SetKernelArg(ocl.k_memcpy[di], 3, sizeof(int), &size);

    size_t global = (size_t)size;
    cl_event kern_evt;
    cl.EnqueueNDRangeKernel(dev->queue, ocl.k_memcpy[di], 1, NULL, &global, NULL, 1, &up_evt, &kern_evt);
    cl.ReleaseEvent(up_evt);

    cl_event read_evt;
    cl.EnqueueReadBuffer(dev->queue, buf, CL_FALSE, dst, size, tape + dst, 1, &kern_evt, &read_evt);
    cl.ReleaseEvent(kern_evt);
    cl.Flush(dev->queue);

    return alloc_op(di, read_evt, dst, size);
}

/* ── Async reduce (sum/min/max on 32-bit elements) ───────────── */

int bfpp_opencl_reduce(uint8_t *tape, int offset, int count, int op) {
    if (!ocl.initialized || count < 256) return -1;
    int di = pick_device();
    if (di < 0) return -1;
    bfpp_cl_device_t *dev = &ocl.devices[di];

    int data_size = count * 4;
    cl_mem buf = ensure_buf(di, offset + data_size);
    if (!buf) return -1;

    /* Upload data */
    cl_event up_evt;
    cl.EnqueueWriteBuffer(dev->queue, buf, CL_FALSE, offset, data_size, tape + offset, 0, NULL, &up_evt);

    /* Partial results buffer (one per work-group) */
    size_t local_size = 256;
    size_t global = ((count + local_size - 1) / local_size) * local_size;
    int num_groups = (int)(global / local_size);

    cl_int err;
    cl_mem partial = cl.CreateBuffer(dev->context, CL_MEM_READ_WRITE, num_groups * 4, NULL, &err);
    if (err != CL_SUCCESS) { cl.ReleaseEvent(up_evt); return -1; }

    cl.SetKernelArg(ocl.k_reduce[di], 0, sizeof(cl_mem), &buf);
    cl.SetKernelArg(ocl.k_reduce[di], 1, sizeof(int), &offset);
    cl.SetKernelArg(ocl.k_reduce[di], 2, sizeof(int), &count);
    cl.SetKernelArg(ocl.k_reduce[di], 3, sizeof(int), &op);
    cl.SetKernelArg(ocl.k_reduce[di], 4, sizeof(cl_mem), &partial);
    cl.SetKernelArg(ocl.k_reduce[di], 5, local_size * 4, NULL); /* local scratch */

    cl_event kern_evt;
    cl.EnqueueNDRangeKernel(dev->queue, ocl.k_reduce[di], 1, NULL, &global, &local_size, 1, &up_evt, &kern_evt);
    cl.ReleaseEvent(up_evt);

    /* Read partial results back to CPU for final reduction */
    int32_t *partials = (int32_t *)malloc(num_groups * 4);
    cl_event read_evt;
    cl.EnqueueReadBuffer(dev->queue, partial, CL_TRUE, 0, num_groups * 4, partials, 1, &kern_evt, &read_evt);
    cl.ReleaseEvent(kern_evt);

    /* CPU final reduction across work-groups */
    int32_t result = partials[0];
    for (int i = 1; i < num_groups; i++) {
        if (op == 0) result += partials[i];
        else if (op == 1) result = result < partials[i] ? result : partials[i];
        else result = result > partials[i] ? result : partials[i];
    }
    free(partials);
    cl.ReleaseMemObject(partial);

    /* Write result to tape[offset] */
    tape[offset]   =  result        & 0xFF;
    tape[offset+1] = (result >> 8)  & 0xFF;
    tape[offset+2] = (result >> 16) & 0xFF;
    tape[offset+3] = (result >> 24) & 0xFF;

    cl.ReleaseEvent(read_evt);
    return 0; /* synchronous — result already in tape */
}

/* ── Async batch matrix transform ────────────────────────────── */

int bfpp_opencl_transform(uint8_t *tape, int matrices_offset, int count) {
    if (!ocl.initialized || count < 16) return -1;
    int di = pick_device();
    if (di < 0) return -1;
    bfpp_cl_device_t *dev = &ocl.devices[di];

    int data_size = count * 64; /* 16 x int32 per matrix = 64 bytes */
    cl_mem buf = ensure_buf(di, matrices_offset + data_size);
    if (!buf) return -1;

    cl_event up_evt;
    cl.EnqueueWriteBuffer(dev->queue, buf, CL_FALSE, matrices_offset, data_size,
                          tape + matrices_offset, 0, NULL, &up_evt);

    cl.SetKernelArg(ocl.k_transform[di], 0, sizeof(cl_mem), &buf);
    cl.SetKernelArg(ocl.k_transform[di], 1, sizeof(int), &matrices_offset);
    cl.SetKernelArg(ocl.k_transform[di], 2, sizeof(int), &count);

    size_t global = (size_t)count;
    cl_event kern_evt;
    cl.EnqueueNDRangeKernel(dev->queue, ocl.k_transform[di], 1, NULL, &global, NULL, 1, &up_evt, &kern_evt);
    cl.ReleaseEvent(up_evt);

    cl_event read_evt;
    cl.EnqueueReadBuffer(dev->queue, buf, CL_FALSE, matrices_offset, data_size,
                          tape + matrices_offset, 1, &kern_evt, &read_evt);
    cl.ReleaseEvent(kern_evt);
    cl.Flush(dev->queue);

    return alloc_op(di, read_evt, matrices_offset, data_size);
}

/* ── Async sort (not yet fully implemented — requires multi-pass radix) ── */

int bfpp_opencl_sort(uint8_t *tape, int offset, int count, int elem_size) {
    /* GPU radix sort requires multiple kernel dispatches (histogram + scatter per bit).
     * For now, fall back to CPU qsort for correctness. GPU path is a future optimization. */
    (void)elem_size;
    if (!ocl.initialized) return -1;

    /* CPU fallback: sort in-place using qsort */
    /* Elements are `elem_size` bytes each at tape[offset] */
    /* For 4-byte elements (the common case): */
    if (elem_size == 4) {
        qsort(tape + offset, count, 4, cmp_u32_le);
        return 0;
    }
    return -1; /* signal CPU fallback to caller */
}

/* ── Async rasterization (GPU edge-function per-pixel) ───────── */

int bfpp_opencl_rasterize(uint8_t *tape, int vert_offset, int vert_count,
                          int idx_offset, int idx_count,
                          int fb_offset, int width, int height) {
    /* GPU rasterization requires uploading transformed triangle data + z-buffer.
     * The rasterize kernel (BFPP_CL_RASTERIZE) is defined in the kernels header
     * but requires scene-specific setup (light positions, camera, etc.).
     * For now, fall back to CPU software rasterizer. */
    (void)tape; (void)vert_offset; (void)vert_count;
    (void)idx_offset; (void)idx_count;
    (void)fb_offset; (void)width; (void)height;
    return -1; /* CPU fallback — full GPU rasterize path is a future task */
}

/* ── Intrinsic wrappers (called from generated C) ────────────── */

void bfpp_gpu_init(uint8_t *tape, int ptr) {
    int count = bfpp_opencl_init();
    tape_set_u32(tape, ptr, (uint32_t)count);
}

void bfpp_gpu_count(uint8_t *tape, int ptr) {
    tape_set_u32(tape, ptr, (uint32_t)bfpp_opencl_device_count());
}

void bfpp_gpu_memset(uint8_t *tape, int ptr) {
    int offset = (int)tape_u32(tape, ptr);
    uint8_t value = tape[ptr + 4];
    int size = (int)tape_u32(tape, ptr + 8);
    int handle = bfpp_opencl_memset(tape, offset, value, size);
    if (handle < 0) {
        /* Fallback: CPU memset */
        memset(tape + offset, value, size);
    }
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_memcpy(uint8_t *tape, int ptr) {
    int dst = (int)tape_u32(tape, ptr);
    int src = (int)tape_u32(tape, ptr + 4);
    int size = (int)tape_u32(tape, ptr + 8);
    int handle = bfpp_opencl_memcpy(tape, dst, src, size);
    if (handle < 0) {
        memmove(tape + dst, tape + src, size);
    }
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_sort(uint8_t *tape, int ptr) {
    int offset = (int)tape_u32(tape, ptr);
    int count = (int)tape_u32(tape, ptr + 4);
    int elem_size = (int)tape_u32(tape, ptr + 8);
    int handle = bfpp_opencl_sort(tape, offset, count, elem_size);
    if (handle < 0) {
        /* Fallback: CPU qsort (would need comparator — skip for now) */
        bfpp_err = BFPP_ERR_GENERIC;
    }
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_reduce(uint8_t *tape, int ptr) {
    int offset = (int)tape_u32(tape, ptr);
    int count = (int)tape_u32(tape, ptr + 4);
    int op = (int)tape_u32(tape, ptr + 8);
    int handle = bfpp_opencl_reduce(tape, offset, count, op);
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_transform(uint8_t *tape, int ptr) {
    int offset = (int)tape_u32(tape, ptr);
    int count = (int)tape_u32(tape, ptr + 4);
    int handle = bfpp_opencl_transform(tape, offset, count);
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_rasterize(uint8_t *tape, int ptr) {
    int vert_off = (int)tape_u32(tape, ptr);
    int vert_count = (int)tape_u32(tape, ptr + 4);
    int idx_off = (int)tape_u32(tape, ptr + 8);
    int idx_count = (int)tape_u32(tape, ptr + 12);
    int handle = bfpp_opencl_rasterize(tape, vert_off, vert_count, idx_off, idx_count, 0, 0, 0);
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_blur(uint8_t *tape, int ptr) {
    int fb_offset = (int)tape_u32(tape, ptr);
    int width = (int)tape_u32(tape, ptr + 4);
    int height = (int)tape_u32(tape, ptr + 8);
    int radius = (int)tape_u32(tape, ptr + 12);
    int handle = bfpp_opencl_blur(tape, fb_offset, width, height, radius);
    tape_set_u32(tape, ptr, (uint32_t)handle);
}

void bfpp_gpu_poll(uint8_t *tape, int ptr) {
    int handle = (int)tape_u32(tape, ptr);
    tape_set_u32(tape, ptr, (uint32_t)bfpp_opencl_poll(handle));
}

void bfpp_gpu_wait(uint8_t *tape, int ptr) {
    int handle = (int)tape_u32(tape, ptr);
    bfpp_opencl_wait(handle);
}

void bfpp_gpu_dispatch(uint8_t *tape, int ptr) {
    /* Generic dispatch — TODO */
    (void)tape; (void)ptr;
    bfpp_err = BFPP_ERR_GENERIC;
}
