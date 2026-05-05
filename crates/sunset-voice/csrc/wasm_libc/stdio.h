/* Stub <stdio.h> for the wasm32-unknown-unknown libopus build.
 *
 * libopus reaches for <stdio.h> only for assertion / debug printout
 * code paths that are off by default. We declare nothing here — the
 * preprocessor will see an empty header and any reference to
 * `printf` / `FILE` will become a compile error, surfacing accidental
 * use rather than letting it sneak into the wasm bundle.
 *
 * If the libopus build ever genuinely needs printf, the right move
 * is to opt out (`#define DISABLE_FLOAT_API` / similar), not to
 * implement a printf shim.
 */
#ifndef SUNSET_WASM_STDIO_H
#define SUNSET_WASM_STDIO_H

#include <stddef.h>

#endif
