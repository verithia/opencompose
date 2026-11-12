# oc-dsp

The built-in device implementations: EQ, dynamics, time, colour and the vocal
and mastering processors, plus the shared kernels they use.

## What it owns

One module per device family, each implementing `oc_audio::AudioEffect`, and a
small set of shared primitives: biquads, the crossover, the sinc and Kaiser
kernels, the envelope generators, the SIMD kernels and the pitch detector. A
device reads its parameters by key, so `oc-device-schema` stays the single
description of what a control is called.

Because the devices are pure functions of their state and input, they are
tested per device, bit-identity tested across SIMD backends, and measured for CPU
cost through the `perf-tests` feature.

## Key types

| Type | Role |
|---|---|
| [`Limiter`](../api/oc_dsp/limiter/struct.Limiter.html) | True-peak limiter; the master ceiling and the loudness passes use it. |
| [`BiquadFilter`](../api/oc_dsp/biquad/struct.BiquadFilter.html) | The shared filter primitive: `FilterType`, coefficients and state. |
| [`MultibandCrossover`](../api/oc_dsp/crossover/struct.MultibandCrossover.html) | Linkwitz-Riley split used by the multiband devices and the stem fallback. |
| [`ParametricEq`](../api/oc_dsp/eq/struct.ParametricEq.html) | The EQ behind `eq:` and the curve editor's bands. |
| `simd` | `SimdLevel`, `select`, and the vector kernels with a scalar fallback. |
| `kernel` | `sinc`, `kaiser`, `half_band_kernel`: the resampling and filter kernels shared with the synth. |

## Depends on

`oc-audio` (the effect contract), `oc-device-schema` (parameter names and
ranges), `serde` and `rustfft`. No engine, host or plugin crate.

## Rules

- No plugin hosting, no file I/O, no project parsing. A device receives
  parameters and buffers.
- Every device must be deterministic for the same input, state and parameters;
  the render gate depends on it.
- A parameter that is added to a device must be added to its schema in the same
  change, or validation and the fader both miss it.
- Allocate in the constructor only. A device's `process` runs in the realtime
  callback.
- Keep the scalar path working: the SIMD kernels must agree with it bit for bit,
  and the tests assert that.
- A new device family gets its own module, its schema entry and a test that
  fails if the device is absent.

## Exact references and portable laws

The DeEsser defaults, multiband pre-lookahead defaults and FDN brightness tests
retain exact sample-bit hashes for all twelve cases. Their verified inputs also
have exact hash anchors, so input-generation drift is distinguished from device
output drift. The five differing output references are selected only for native
`x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc` builds. The original
references remain unchanged for `aarch64-apple-darwin`; other targets keep those
original assertions until their own references are independently verified.
This fallback is not a claim that unverified targets produce identical samples.

The Linux GNU and Windows MSVC references were captured with Rust 1.95.0 in
the ordinary native unit-test/debug profile, without DSP math changes. Every
input and output hash was reproduced against an independently preserved
same-target reference build before adoption. These references are not release,
performance-profile or all-toolchain guarantees. A compiler, math library or
optimization change requires the same independent comparison and response-law
validation before changing a reference.

| Case | Original / macOS ARM64 | Linux GNU x86-64 | Windows MSVC x86-64 |
|---|---|---|---|
| DeEsser split | `495ef0f3cc1ad463` | `6a7e629a8d273eb9` | `630c6fd606f274fb` |
| DeEsser split 6 kHz | `4a5b8c667bd518fc` | `6000d3d58afbaa9b` | `c4d5a87a005ad6e0` |
| Multiband linear, wet | `15811173f78c3d7d` | `e21ba3b04eb4d085` | `887bc703c73fd09b` |
| FDN brightness 0 | `40df18b6149b9969` | `bd908f081b01afb4` | `bd908f081b01afb4` |
| FDN brightness 75 | `cb804cf603856284` | `4c2cc4621b0986d3` | `4c2cc4621b0986d3` |

Rust documents unspecified, platform- and compiler-dependent precision for
[`sin`](https://doc.rust-lang.org/std/primitive.f64.html#method.sin),
[`cos`](https://doc.rust-lang.org/std/primitive.f64.html#method.cos),
[`exp`](https://doc.rust-lang.org/std/primitive.f64.html#method.exp) and
[`powf`](https://doc.rust-lang.org/std/primitive.f64.html#method.powf).
The devices' filter and feedback designs use these operations. That contract
precludes assuming universal bit identity; it does not establish the cause of
any particular mismatch. A hash difference alone says nothing about its audible
size or physical correctness.

Exact references guard preservation on a verified target. Portable tests remain
separate requirements: neutral and bypass sample identity, declared latency,
reset and block-partition determinism, crossover summation/response and slopes,
DeEsser reduction/phase laws, and FDN per-band decay and brightness laws. Target
selection changes none of those assertions and introduces no tolerance or
alternative production math.

## API

[`api/oc_dsp/`](../api/oc_dsp/index.html).
