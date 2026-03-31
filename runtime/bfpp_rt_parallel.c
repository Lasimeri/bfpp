/*
 * bfpp_rt_parallel.c — BF++ multicore runtime: threads, mutexes, barriers, atomics
 *
 * Architecture:
 *   The BF++ tape (uint8_t[]) is shared across all threads. Per-thread state
 *   (pointer, stack pointer, error flag, call depth) is _Thread_local —
 *   each thread gets its own copy, initialized by bfpp_thread_entry().
 *
 *   Mutexes and barriers use fixed-size static arrays (256 and 64 slots
 *   respectively). Mutexes are lazily initialized on first lock if not
 *   explicitly init'd. Barriers must be explicitly initialized with a
 *   participant count before use.
 *
 *   Atomic operations dispatch on cell_width to use the correct atomic
 *   type width. Addresses should be naturally aligned for the given width.
 *
 * Threading model:
 *   Main thread: bfpp_thread_index = 0
 *   Spawned threads: index assigned via atomic increment of bfpp_next_thread_index
 *   The caller allocates a bfpp_thread_arg_t (malloc'd), passes it to
 *   pthread_create with bfpp_thread_entry. The entry wrapper frees the arg.
 *
 * Link with: -lpthread
 */

#include "bfpp_rt_parallel.h"
#include <stdlib.h>
#include <string.h>

/* ── Thread-local state ────────────────────────────────────────── */

/* Thread index: main thread = 0, spawned threads get 1, 2, 3, ... */
_Thread_local int bfpp_thread_index = 0;
atomic_int bfpp_next_thread_index = 1;

/* ── Thread entry wrapper ──────────────────────────────────────── */

/* Called via pthread_create. Resets all thread-local state, invokes
   the BF++ subroutine, then frees the arg struct. The subroutine
   operates on the shared tape with its own ptr/sp/err/call_depth. */
void *bfpp_thread_entry(void *arg)
{
    bfpp_thread_arg_t *a = (bfpp_thread_arg_t *)arg;

    /* Initialize thread-local state */
    bfpp_thread_index = a->index;
    ptr = a->start_ptr;
    sp = 0;
    bfpp_err = 0;
    bfpp_call_depth = 0;
    /* cell_width is _Thread_local — zero-initialized by default on thread creation.
       Must be set to 1 (each cell is an independent 1-byte cell) before the
       subroutine runs. tape_size comes from the arg struct (TAPE_SIZE is a
       compile-time define in the generated C, not available here). */
    memset(cell_width, 1, (size_t)a->tape_size);

    /* Execute the subroutine */
    a->func();

    free(a);
    return NULL;
}

/* ── Mutex array ───────────────────────────────────────────────── */

#define BFPP_MAX_MUTEXES 256

static pthread_mutex_t bfpp_mutexes[BFPP_MAX_MUTEXES];
static int bfpp_mutex_initialized[BFPP_MAX_MUTEXES]; /* 0 = not init'd */

void bfpp_mutex_init(int id)
{
    if (id < 0 || id >= BFPP_MAX_MUTEXES) return;
    pthread_mutex_init(&bfpp_mutexes[id], NULL);
    bfpp_mutex_initialized[id] = 1;
}

/* Auto-initializes on first lock if not explicitly init'd.
   This is a convenience for generated code that may not emit
   explicit init calls for every mutex slot used. */
void bfpp_mutex_lock(int id)
{
    if (id < 0 || id >= BFPP_MAX_MUTEXES) return;
    if (!bfpp_mutex_initialized[id]) bfpp_mutex_init(id);
    pthread_mutex_lock(&bfpp_mutexes[id]);
}

void bfpp_mutex_unlock(int id)
{
    if (id < 0 || id >= BFPP_MAX_MUTEXES) return;
    pthread_mutex_unlock(&bfpp_mutexes[id]);
}

/* ── Barrier array ─────────────────────────────────────────────── */

#define BFPP_MAX_BARRIERS 64

static pthread_barrier_t bfpp_barriers[BFPP_MAX_BARRIERS];

/* Must be called before bfpp_barrier_wait(). count = number of
   threads that must call wait before any are released. */
void bfpp_barrier_init(int id, int count)
{
    if (id < 0 || id >= BFPP_MAX_BARRIERS || count < 1) return;
    pthread_barrier_init(&bfpp_barriers[id], NULL, (unsigned)count);
}

void bfpp_barrier_wait(int id)
{
    if (id < 0 || id >= BFPP_MAX_BARRIERS) return;
    pthread_barrier_wait(&bfpp_barriers[id]);
}

/* ── Atomic operations on tape cells ───────────────────────────── */

/* All functions dispatch on cell_width (1/2/4/8 bytes).
   The tape pointer is cast to the appropriate _Atomic type.
   Callers must ensure addr is naturally aligned for the width. */

uint64_t bfpp_atomic_load(uint8_t *tape, int addr, int cell_width)
{
    switch (cell_width) {
        case 1: return atomic_load((_Atomic uint8_t  *)&tape[addr]);
        case 2: return atomic_load((_Atomic uint16_t *)&tape[addr]);
        case 4: return atomic_load((_Atomic uint32_t *)&tape[addr]);
        case 8: return atomic_load((_Atomic uint64_t *)&tape[addr]);
        default: return tape[addr];
    }
}

void bfpp_atomic_store(uint8_t *tape, int addr, uint64_t value, int cell_width)
{
    switch (cell_width) {
        case 1: atomic_store((_Atomic uint8_t  *)&tape[addr], (uint8_t)value);  break;
        case 2: atomic_store((_Atomic uint16_t *)&tape[addr], (uint16_t)value); break;
        case 4: atomic_store((_Atomic uint32_t *)&tape[addr], (uint32_t)value); break;
        case 8: atomic_store((_Atomic uint64_t *)&tape[addr], value);           break;
    }
}

uint64_t bfpp_atomic_add(uint8_t *tape, int addr, uint64_t value, int cell_width)
{
    switch (cell_width) {
        case 1: return atomic_fetch_add((_Atomic uint8_t  *)&tape[addr], (uint8_t)value);
        case 2: return atomic_fetch_add((_Atomic uint16_t *)&tape[addr], (uint16_t)value);
        case 4: return atomic_fetch_add((_Atomic uint32_t *)&tape[addr], (uint32_t)value);
        case 8: return atomic_fetch_add((_Atomic uint64_t *)&tape[addr], value);
        default: return 0;
    }
}

/* Returns 1 on successful swap, 0 on failure (expected != actual). */
int bfpp_atomic_cas(uint8_t *tape, int addr, uint64_t expected, uint64_t desired, int cell_width)
{
    switch (cell_width) {
        case 1: {
            uint8_t exp = (uint8_t)expected;
            return atomic_compare_exchange_strong((_Atomic uint8_t *)&tape[addr], &exp, (uint8_t)desired);
        }
        case 2: {
            uint16_t exp = (uint16_t)expected;
            return atomic_compare_exchange_strong((_Atomic uint16_t *)&tape[addr], &exp, (uint16_t)desired);
        }
        case 4: {
            uint32_t exp = (uint32_t)expected;
            return atomic_compare_exchange_strong((_Atomic uint32_t *)&tape[addr], &exp, (uint32_t)desired);
        }
        case 8: {
            uint64_t exp = expected;
            return atomic_compare_exchange_strong((_Atomic uint64_t *)&tape[addr], &exp, desired);
        }
        default: return 0;
    }
}

/* ── Standalone compilation stubs ──────────────────────────────── */

/* When compiling the runtime in isolation (not linked with generated code),
   provide stub definitions for the _Thread_local externs that would normally
   be defined in the generated C output. */
#ifdef BFPP_RT_PARALLEL_STANDALONE
_Thread_local int ptr = 0;
_Thread_local int sp = 0;
_Thread_local int bfpp_err = 0;
_Thread_local int bfpp_call_depth = 0;
_Thread_local uint8_t cell_width[65536];
#endif
