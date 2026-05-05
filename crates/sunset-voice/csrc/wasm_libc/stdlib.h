/* Minimal <stdlib.h> stub for the wasm32-unknown-unknown libopus build.
 *
 * `malloc` / `free` / `calloc` / `realloc` / `abort` are defined in
 * `codec::wasm_runtime` and resolve at wasm-ld time.
 */
#ifndef SUNSET_WASM_STDLIB_H
#define SUNSET_WASM_STDLIB_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
void abort(void) __attribute__((noreturn));
int abs(int);
int atoi(const char *);

/* libopus uses ALLOC(...) → alloca(...) (without #include <alloca.h>)
 * so we mirror glibc and surface the builtin from <stdlib.h> too. */
#define alloca(n) __builtin_alloca(n)

#ifdef __cplusplus
}
#endif

#endif
