#ifndef BFPP_RT_PARALLEL_H
#define BFPP_RT_PARALLEL_H

/*
 * bfpp_rt_parallel.h — Threading and synchronization primitives for BF++.
 *
 * Provides the runtime support for BF++ parallel intrinsics:
 *   __spawn / __join    → pthread-based thread management
 *   __mutex_*           → mutex array (256 slots, auto-initialized)
 *   __barrier_*         → barrier array (64 slots)
 *   __atomic_*          → variable-width atomic ops on shared tape
 *
 * Generated C code calls these functions directly. The tape (uint8_t[])
 * is shared across all threads; per-thread state (ptr, sp, bfpp_err,
 * bfpp_call_depth) is _Thread_local and reset by bfpp_thread_entry().
 *
 * Link with -lpthread.
 */

#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>

/* ── Thread management ─────────────────────────────────────────── */

typedef struct {
    void (*func)(void);   /* subroutine entry point */
    int start_ptr;        /* initial tape pointer for this thread */
    int index;            /* thread-local index */
} bfpp_thread_arg_t;

/* Thread entry wrapper: sets thread-local state, calls subroutine.
   Frees the arg struct on return. */
void *bfpp_thread_entry(void *arg);

/* ── Mutex array (256 slots, lazily initialized) ───────────────── */

void bfpp_mutex_init(int id);
void bfpp_mutex_lock(int id);
void bfpp_mutex_unlock(int id);

/* ── Barrier array (64 slots) ──────────────────────────────────── */

void bfpp_barrier_init(int id, int count);
void bfpp_barrier_wait(int id);

/* ── Atomic operations on tape cells ───────────────────────────── */

/* All operate on tape[] which is shared across threads.
   cell_width: 1, 2, 4, or 8 bytes. Addresses must be naturally aligned
   for the given width or behavior is undefined (hardware-dependent). */

uint64_t bfpp_atomic_load(uint8_t *tape, int addr, int cell_width);
void     bfpp_atomic_store(uint8_t *tape, int addr, uint64_t value, int cell_width);
uint64_t bfpp_atomic_add(uint8_t *tape, int addr, uint64_t value, int cell_width);
int      bfpp_atomic_cas(uint8_t *tape, int addr, uint64_t expected, uint64_t desired, int cell_width);

/* ── Thread-local state extern declarations ────────────────────── */

/* These are _Thread_local in the generated C. The parallel runtime
   needs visibility for the thread entry wrapper to reset them. */

extern _Thread_local int ptr;
extern _Thread_local int sp;
extern _Thread_local int bfpp_err;
extern _Thread_local int bfpp_call_depth;

/* ── Thread index tracking ─────────────────────────────────────── */

/* Per-thread index (main = 0, spawned threads get monotonically
   increasing indices via atomic increment of bfpp_next_thread_index). */

extern _Thread_local int bfpp_thread_index;
extern atomic_int bfpp_next_thread_index;

#endif /* BFPP_RT_PARALLEL_H */
