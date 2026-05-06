/* Minimal <string.h> stub for the wasm32-unknown-unknown libopus build.
 *
 * `memcpy` / `memset` / `memmove` / `memcmp` resolve at link time
 * against Rust's `compiler-builtins`, which exports them as C
 * symbols on wasm32. Other functions here have shims in
 * `codec::wasm_runtime`.
 */
#ifndef SUNSET_WASM_STRING_H
#define SUNSET_WASM_STRING_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *memcpy(void *, const void *, size_t);
void *memmove(void *, const void *, size_t);
int memcmp(const void *, const void *, size_t);
void *memset(void *, int, size_t);
size_t strlen(const char *);

#ifdef __cplusplus
}
#endif

#endif
