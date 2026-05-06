/* Stub <alloca.h> for the wasm32-unknown-unknown libopus build.
 *
 * libopus, with `-DUSE_ALLOCA`, calls `alloca()` for stack-local
 * scratch arrays. clang exposes it as a builtin; just route the name.
 */
#ifndef SUNSET_WASM_ALLOCA_H
#define SUNSET_WASM_ALLOCA_H

#define alloca(n) __builtin_alloca(n)

#endif
