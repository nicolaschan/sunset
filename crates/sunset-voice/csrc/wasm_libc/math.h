/* Minimal <math.h> stub for the wasm32-unknown-unknown libopus build.
 *
 * libopus is plain ISO C and reaches for <math.h> for declarations of
 * `sin` / `cos` / `sqrt` / etc. There is no libc on
 * wasm32-unknown-unknown, and the host glibc headers don't compile
 * cleanly for a 32-bit wasm target (gnu/stubs-32.h missing).
 *
 * We declare just the functions libopus actually calls. The matching
 * definitions live in `crates/sunset-voice/src/codec/wasm_runtime.rs`
 * and resolve at wasm-ld time. (See that file for the libm-backed
 * implementations.)
 */
#ifndef SUNSET_WASM_MATH_H
#define SUNSET_WASM_MATH_H

#ifdef __cplusplus
extern "C" {
#endif

double sin(double);
double cos(double);
double tan(double);
double atan(double);
double atan2(double, double);
double exp(double);
double log(double);
double log10(double);
double log2(double);
double pow(double, double);
double asin(double);
double acos(double);
double sqrt(double);
double floor(double);
double ceil(double);
double fabs(double);
double fmod(double, double);
double round(double);
double ldexp(double, int);
double frexp(double, int *);
long lrint(double);

float sinf(float);
float cosf(float);
float tanf(float);
float atanf(float);
float atan2f(float, float);
float expf(float);
float logf(float);
float log10f(float);
float log2f(float);
float powf(float, float);
float asinf(float);
float acosf(float);
float sqrtf(float);
float floorf(float);
float ceilf(float);
float fabsf(float);
float fmodf(float, float);
float roundf(float);
long lrintf(float);

#define M_PI 3.14159265358979323846
#define M_PI_2 1.57707963267948966192
#define HUGE_VAL (1.0e+300 * 1.0e+300)
#define HUGE_VALF ((float)HUGE_VAL)
#define INFINITY HUGE_VALF
#define NAN (__builtin_nanf(""))

#ifdef __cplusplus
}
#endif

#endif
