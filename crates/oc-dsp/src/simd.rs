//! Portable element-wise stereo kernels with one dispatch per call.
//!
//! Buffers are interleaved as `[left, right, left, right, ...]`. Every kernel is
//! bit-identical to its scalar reference: the vector bodies only widen the same
//! IEEE `f32`/`f64` operations and never reorder a reduction.
//!
//! Threading: stateless; the dispatch level is resolved through an atomic once
//! and never allocates or locks.

use std::sync::atomic::{AtomicU8, Ordering};

/// Selected instruction set for the element-wise kernels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdLevel {
    Scalar,
    Sse2,
    Avx2,
    Neon,
}

const UNRESOLVED: u8 = 0xFF;
static LEVEL: AtomicU8 = AtomicU8::new(UNRESOLVED);

impl SimdLevel {
    /// Stable lowercase name used by the `OCP_SIMD` override and diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            SimdLevel::Scalar => "scalar",
            SimdLevel::Sse2 => "sse2",
            SimdLevel::Avx2 => "avx2",
            SimdLevel::Neon => "neon",
        }
    }

    /// Parse a name accepted by [`select`].
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "scalar" => Some(SimdLevel::Scalar),
            "sse2" => Some(SimdLevel::Sse2),
            "avx2" => Some(SimdLevel::Avx2),
            "neon" => Some(SimdLevel::Neon),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            SimdLevel::Scalar => 0,
            SimdLevel::Sse2 => 1,
            SimdLevel::Avx2 => 2,
            SimdLevel::Neon => 3,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => SimdLevel::Sse2,
            2 => SimdLevel::Avx2,
            3 => SimdLevel::Neon,
            _ => SimdLevel::Scalar,
        }
    }

    /// Probe the backend this build can use. The result is cached for the
    /// process, so audio callbacks only pay a relaxed atomic load.
    pub fn detect() -> Self {
        #[cfg(target_arch = "aarch64")]
        {
            SimdLevel::Neon
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                SimdLevel::Avx2
            } else {
                SimdLevel::Sse2
            }
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            SimdLevel::Scalar
        }
    }
}

/// Backend used by the public kernels. The first call probes and caches it.
pub fn level() -> SimdLevel {
    let cached = LEVEL.load(Ordering::Relaxed);
    if cached != UNRESOLVED {
        return SimdLevel::from_code(cached);
    }
    let detected = SimdLevel::detect();
    LEVEL.store(detected.code(), Ordering::Relaxed);
    detected
}

/// Whether this build can run `level` on the current CPU.
pub fn available(level: SimdLevel) -> bool {
    match level {
        SimdLevel::Scalar => true,
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => true,
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => true,
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx2 => std::arch::is_x86_feature_detected!("avx2"),
        _ => false,
    }
}

/// Force the backend for diagnostics (bit-identity tests, benchmarks).
///
/// Returns `false` and keeps the current backend when this CPU cannot run the
/// requested level, so an override can never enable an instruction set that was
/// only detected at compile time. Process-wide and lock-free; call it before
/// audio starts.
pub fn select(level: SimdLevel) -> bool {
    if !available(level) {
        return false;
    }
    LEVEL.store(level.code(), Ordering::Relaxed);
    true
}

/// Multiply each interleaved channel by its own gain.
pub fn channel_gains(buffer: &mut [f32], gain_left: f32, gain_right: f32) {
    channel_gains_at(level(), buffer, gain_left, gain_right);
}

fn channel_gains_at(level: SimdLevel, buffer: &mut [f32], gain_left: f32, gain_right: f32) {
    match level {
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => channel_gains_neon(buffer, gain_left, gain_right),
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => channel_gains_sse2(buffer, gain_left, gain_right),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: dispatch only selects AVX2 after the runtime probe.
        SimdLevel::Avx2 => unsafe { channel_gains_avx2(buffer, gain_left, gain_right) },
        _ => channel_gains_scalar(buffer, gain_left, gain_right),
    }
}

fn channel_gains_scalar(buffer: &mut [f32], gain_left: f32, gain_right: f32) {
    for (index, sample) in buffer.iter_mut().enumerate() {
        *sample *= if index % 2 == 0 {
            gain_left
        } else {
            gain_right
        };
    }
}

#[cfg(target_arch = "aarch64")]
fn channel_gains_neon(buffer: &mut [f32], gain_left: f32, gain_right: f32) {
    // SAFETY: NEON is baseline on aarch64; every access is bounded by `buffer`.
    unsafe {
        use std::arch::aarch64::*;
        let gains = vld1q_f32([gain_left, gain_right, gain_left, gain_right].as_ptr());
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = vld1q_f32(pointer);
            let second = vld1q_f32(pointer.add(4));
            let third = vld1q_f32(pointer.add(8));
            let fourth = vld1q_f32(pointer.add(12));
            vst1q_f32(pointer, vmulq_f32(first, gains));
            vst1q_f32(pointer.add(4), vmulq_f32(second, gains));
            vst1q_f32(pointer.add(8), vmulq_f32(third, gains));
            vst1q_f32(pointer.add(12), vmulq_f32(fourth, gains));
            index += 16;
        }
        while index + 4 <= buffer.len() {
            let samples = vld1q_f32(buffer.as_ptr().add(index));
            vst1q_f32(buffer.as_mut_ptr().add(index), vmulq_f32(samples, gains));
            index += 4;
        }
        while index < buffer.len() {
            buffer[index] *= if index % 2 == 0 {
                gain_left
            } else {
                gain_right
            };
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn channel_gains_sse2(buffer: &mut [f32], gain_left: f32, gain_right: f32) {
    // SAFETY: SSE2 is baseline on x86_64; every access is bounded by `buffer`.
    unsafe {
        use std::arch::x86_64::*;
        let gains = _mm_setr_ps(gain_left, gain_right, gain_left, gain_right);
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = _mm_loadu_ps(pointer);
            let second = _mm_loadu_ps(pointer.add(4));
            let third = _mm_loadu_ps(pointer.add(8));
            let fourth = _mm_loadu_ps(pointer.add(12));
            _mm_storeu_ps(pointer, _mm_mul_ps(first, gains));
            _mm_storeu_ps(pointer.add(4), _mm_mul_ps(second, gains));
            _mm_storeu_ps(pointer.add(8), _mm_mul_ps(third, gains));
            _mm_storeu_ps(pointer.add(12), _mm_mul_ps(fourth, gains));
            index += 16;
        }
        while index + 4 <= buffer.len() {
            let samples = _mm_loadu_ps(buffer.as_ptr().add(index));
            _mm_storeu_ps(buffer.as_mut_ptr().add(index), _mm_mul_ps(samples, gains));
            index += 4;
        }
        while index < buffer.len() {
            buffer[index] *= if index % 2 == 0 {
                gain_left
            } else {
                gain_right
            };
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn channel_gains_avx2(buffer: &mut [f32], gain_left: f32, gain_right: f32) {
    // SAFETY: the caller checked AVX2 support; every access is bounded by `buffer`.
    unsafe {
        use std::arch::x86_64::*;
        let gains = _mm256_setr_ps(
            gain_left, gain_right, gain_left, gain_right, gain_left, gain_right, gain_left,
            gain_right,
        );
        let mut index = 0;
        while index + 32 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = _mm256_loadu_ps(pointer);
            let second = _mm256_loadu_ps(pointer.add(8));
            let third = _mm256_loadu_ps(pointer.add(16));
            let fourth = _mm256_loadu_ps(pointer.add(24));
            _mm256_storeu_ps(pointer, _mm256_mul_ps(first, gains));
            _mm256_storeu_ps(pointer.add(8), _mm256_mul_ps(second, gains));
            _mm256_storeu_ps(pointer.add(16), _mm256_mul_ps(third, gains));
            _mm256_storeu_ps(pointer.add(24), _mm256_mul_ps(fourth, gains));
            index += 32;
        }
        while index + 8 <= buffer.len() {
            let samples = _mm256_loadu_ps(buffer.as_ptr().add(index));
            _mm256_storeu_ps(
                buffer.as_mut_ptr().add(index),
                _mm256_mul_ps(samples, gains),
            );
            index += 8;
        }
        while index < buffer.len() {
            buffer[index] *= if index % 2 == 0 {
                gain_left
            } else {
                gain_right
            };
            index += 1;
        }
    }
}

/// Add a scaled copy of `source` into `target`, one gain per channel.
pub fn accumulate_channel_gains(
    target: &mut [f32],
    source: &[f32],
    gain_left: f32,
    gain_right: f32,
) {
    accumulate_channel_gains_at(level(), target, source, gain_left, gain_right);
}

fn accumulate_channel_gains_at(
    level: SimdLevel,
    target: &mut [f32],
    source: &[f32],
    gain_left: f32,
    gain_right: f32,
) {
    match level {
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => accumulate_channel_gains_neon(target, source, gain_left, gain_right),
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => accumulate_channel_gains_sse2(target, source, gain_left, gain_right),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: dispatch only selects AVX2 after the runtime probe.
        SimdLevel::Avx2 => unsafe {
            accumulate_channel_gains_avx2(target, source, gain_left, gain_right)
        },
        _ => accumulate_channel_gains_scalar(target, source, gain_left, gain_right),
    }
}

fn accumulate_channel_gains_scalar(
    target: &mut [f32],
    source: &[f32],
    gain_left: f32,
    gain_right: f32,
) {
    for (index, (target, source)) in target.iter_mut().zip(source).enumerate() {
        *target += *source
            * if index % 2 == 0 {
                gain_left
            } else {
                gain_right
            };
    }
}

#[cfg(target_arch = "aarch64")]
fn accumulate_channel_gains_neon(
    target: &mut [f32],
    source: &[f32],
    gain_left: f32,
    gain_right: f32,
) {
    // SAFETY: NEON is baseline on aarch64; the loop is bounded by both slices.
    unsafe {
        use std::arch::aarch64::*;
        let gains = vld1q_f32([gain_left, gain_right, gain_left, gain_right].as_ptr());
        let len = target.len().min(source.len());
        let mut index = 0;
        while index + 16 <= len {
            let pointer = target.as_mut_ptr().add(index);
            let first = vld1q_f32(pointer);
            let second = vld1q_f32(pointer.add(4));
            let third = vld1q_f32(pointer.add(8));
            let fourth = vld1q_f32(pointer.add(12));
            vst1q_f32(
                pointer,
                vaddq_f32(
                    first,
                    vmulq_f32(vld1q_f32(source.as_ptr().add(index)), gains),
                ),
            );
            vst1q_f32(
                pointer.add(4),
                vaddq_f32(
                    second,
                    vmulq_f32(vld1q_f32(source.as_ptr().add(index + 4)), gains),
                ),
            );
            vst1q_f32(
                pointer.add(8),
                vaddq_f32(
                    third,
                    vmulq_f32(vld1q_f32(source.as_ptr().add(index + 8)), gains),
                ),
            );
            vst1q_f32(
                pointer.add(12),
                vaddq_f32(
                    fourth,
                    vmulq_f32(vld1q_f32(source.as_ptr().add(index + 12)), gains),
                ),
            );
            index += 16;
        }
        while index + 4 <= len {
            let current = vld1q_f32(target.as_ptr().add(index));
            let incoming = vld1q_f32(source.as_ptr().add(index));
            vst1q_f32(
                target.as_mut_ptr().add(index),
                vaddq_f32(current, vmulq_f32(incoming, gains)),
            );
            index += 4;
        }
        while index < len {
            target[index] += source[index]
                * if index % 2 == 0 {
                    gain_left
                } else {
                    gain_right
                };
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn accumulate_channel_gains_sse2(
    target: &mut [f32],
    source: &[f32],
    gain_left: f32,
    gain_right: f32,
) {
    // SAFETY: SSE2 is baseline on x86_64; the loop is bounded by both slices.
    unsafe {
        use std::arch::x86_64::*;
        let gains = _mm_setr_ps(gain_left, gain_right, gain_left, gain_right);
        let len = target.len().min(source.len());
        let mut index = 0;
        while index + 16 <= len {
            let pointer = target.as_mut_ptr().add(index);
            let first = _mm_loadu_ps(pointer);
            let second = _mm_loadu_ps(pointer.add(4));
            let third = _mm_loadu_ps(pointer.add(8));
            let fourth = _mm_loadu_ps(pointer.add(12));
            _mm_storeu_ps(
                pointer,
                _mm_add_ps(
                    first,
                    _mm_mul_ps(_mm_loadu_ps(source.as_ptr().add(index)), gains),
                ),
            );
            _mm_storeu_ps(
                pointer.add(4),
                _mm_add_ps(
                    second,
                    _mm_mul_ps(_mm_loadu_ps(source.as_ptr().add(index + 4)), gains),
                ),
            );
            _mm_storeu_ps(
                pointer.add(8),
                _mm_add_ps(
                    third,
                    _mm_mul_ps(_mm_loadu_ps(source.as_ptr().add(index + 8)), gains),
                ),
            );
            _mm_storeu_ps(
                pointer.add(12),
                _mm_add_ps(
                    fourth,
                    _mm_mul_ps(_mm_loadu_ps(source.as_ptr().add(index + 12)), gains),
                ),
            );
            index += 16;
        }
        while index + 4 <= len {
            let current = _mm_loadu_ps(target.as_ptr().add(index));
            let incoming = _mm_loadu_ps(source.as_ptr().add(index));
            _mm_storeu_ps(
                target.as_mut_ptr().add(index),
                _mm_add_ps(current, _mm_mul_ps(incoming, gains)),
            );
            index += 4;
        }
        while index < len {
            target[index] += source[index]
                * if index % 2 == 0 {
                    gain_left
                } else {
                    gain_right
                };
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn accumulate_channel_gains_avx2(
    target: &mut [f32],
    source: &[f32],
    gain_left: f32,
    gain_right: f32,
) {
    // SAFETY: the caller checked AVX2 support; the loop is bounded by both slices.
    unsafe {
        use std::arch::x86_64::*;
        let gains = _mm256_setr_ps(
            gain_left, gain_right, gain_left, gain_right, gain_left, gain_right, gain_left,
            gain_right,
        );
        let len = target.len().min(source.len());
        let mut index = 0;
        while index + 32 <= len {
            let pointer = target.as_mut_ptr().add(index);
            let first = _mm256_loadu_ps(pointer);
            let second = _mm256_loadu_ps(pointer.add(8));
            let third = _mm256_loadu_ps(pointer.add(16));
            let fourth = _mm256_loadu_ps(pointer.add(24));
            _mm256_storeu_ps(
                pointer,
                _mm256_add_ps(
                    first,
                    _mm256_mul_ps(_mm256_loadu_ps(source.as_ptr().add(index)), gains),
                ),
            );
            _mm256_storeu_ps(
                pointer.add(8),
                _mm256_add_ps(
                    second,
                    _mm256_mul_ps(_mm256_loadu_ps(source.as_ptr().add(index + 8)), gains),
                ),
            );
            _mm256_storeu_ps(
                pointer.add(16),
                _mm256_add_ps(
                    third,
                    _mm256_mul_ps(_mm256_loadu_ps(source.as_ptr().add(index + 16)), gains),
                ),
            );
            _mm256_storeu_ps(
                pointer.add(24),
                _mm256_add_ps(
                    fourth,
                    _mm256_mul_ps(_mm256_loadu_ps(source.as_ptr().add(index + 24)), gains),
                ),
            );
            index += 32;
        }
        while index + 8 <= len {
            let current = _mm256_loadu_ps(target.as_ptr().add(index));
            let incoming = _mm256_loadu_ps(source.as_ptr().add(index));
            _mm256_storeu_ps(
                target.as_mut_ptr().add(index),
                _mm256_add_ps(current, _mm256_mul_ps(incoming, gains)),
            );
            index += 8;
        }
        while index < len {
            target[index] += source[index]
                * if index % 2 == 0 {
                    gain_left
                } else {
                    gain_right
                };
            index += 1;
        }
    }
}

/// Multiply every sample of a mono or stereo buffer by one gain.
pub fn apply_uniform_gain(buffer: &mut [f32], gain: f32) {
    apply_uniform_gain_at(level(), buffer, gain);
}

fn apply_uniform_gain_at(level: SimdLevel, buffer: &mut [f32], gain: f32) {
    match level {
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => apply_uniform_gain_neon(buffer, gain),
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => apply_uniform_gain_sse2(buffer, gain),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: dispatch only selects AVX2 after the runtime probe.
        SimdLevel::Avx2 => unsafe { apply_uniform_gain_avx2(buffer, gain) },
        _ => {
            for sample in buffer {
                *sample *= gain;
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn apply_uniform_gain_neon(buffer: &mut [f32], gain: f32) {
    // SAFETY: NEON is baseline on aarch64; every access is bounded by `buffer`.
    unsafe {
        use std::arch::aarch64::*;
        let gains = vdupq_n_f32(gain);
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = vld1q_f32(pointer);
            let second = vld1q_f32(pointer.add(4));
            let third = vld1q_f32(pointer.add(8));
            let fourth = vld1q_f32(pointer.add(12));
            vst1q_f32(pointer, vmulq_f32(first, gains));
            vst1q_f32(pointer.add(4), vmulq_f32(second, gains));
            vst1q_f32(pointer.add(8), vmulq_f32(third, gains));
            vst1q_f32(pointer.add(12), vmulq_f32(fourth, gains));
            index += 16;
        }
        while index + 4 <= buffer.len() {
            let samples = vld1q_f32(buffer.as_ptr().add(index));
            vst1q_f32(buffer.as_mut_ptr().add(index), vmulq_f32(samples, gains));
            index += 4;
        }
        while index < buffer.len() {
            buffer[index] *= gain;
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn apply_uniform_gain_sse2(buffer: &mut [f32], gain: f32) {
    // SAFETY: SSE2 is baseline on x86_64; every access is bounded by `buffer`.
    unsafe {
        use std::arch::x86_64::*;
        let gains = _mm_set1_ps(gain);
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = _mm_loadu_ps(pointer);
            let second = _mm_loadu_ps(pointer.add(4));
            let third = _mm_loadu_ps(pointer.add(8));
            let fourth = _mm_loadu_ps(pointer.add(12));
            _mm_storeu_ps(pointer, _mm_mul_ps(first, gains));
            _mm_storeu_ps(pointer.add(4), _mm_mul_ps(second, gains));
            _mm_storeu_ps(pointer.add(8), _mm_mul_ps(third, gains));
            _mm_storeu_ps(pointer.add(12), _mm_mul_ps(fourth, gains));
            index += 16;
        }
        while index + 4 <= buffer.len() {
            let samples = _mm_loadu_ps(buffer.as_ptr().add(index));
            _mm_storeu_ps(buffer.as_mut_ptr().add(index), _mm_mul_ps(samples, gains));
            index += 4;
        }
        while index < buffer.len() {
            buffer[index] *= gain;
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn apply_uniform_gain_avx2(buffer: &mut [f32], gain: f32) {
    // SAFETY: the caller checked AVX2 support; every access is bounded by `buffer`.
    unsafe {
        use std::arch::x86_64::*;
        let gains = _mm256_set1_ps(gain);
        let mut index = 0;
        while index + 32 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = _mm256_loadu_ps(pointer);
            let second = _mm256_loadu_ps(pointer.add(8));
            let third = _mm256_loadu_ps(pointer.add(16));
            let fourth = _mm256_loadu_ps(pointer.add(24));
            _mm256_storeu_ps(pointer, _mm256_mul_ps(first, gains));
            _mm256_storeu_ps(pointer.add(8), _mm256_mul_ps(second, gains));
            _mm256_storeu_ps(pointer.add(16), _mm256_mul_ps(third, gains));
            _mm256_storeu_ps(pointer.add(24), _mm256_mul_ps(fourth, gains));
            index += 32;
        }
        while index + 8 <= buffer.len() {
            let samples = _mm256_loadu_ps(buffer.as_ptr().add(index));
            _mm256_storeu_ps(
                buffer.as_mut_ptr().add(index),
                _mm256_mul_ps(samples, gains),
            );
            index += 8;
        }
        while index < buffer.len() {
            buffer[index] *= gain;
            index += 1;
        }
    }
}

/// Flip the sign of every sample. Kept bit-exact for signed zeros and NaN.
pub fn negate(buffer: &mut [f32]) {
    negate_at(level(), buffer);
}

fn negate_at(level: SimdLevel, buffer: &mut [f32]) {
    match level {
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => negate_neon(buffer),
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => negate_sse2(buffer),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: dispatch only selects AVX2 after the runtime probe.
        SimdLevel::Avx2 => unsafe { negate_avx2(buffer) },
        _ => {
            for sample in buffer {
                *sample = -*sample;
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn negate_neon(buffer: &mut [f32]) {
    // SAFETY: NEON is baseline on aarch64; every access is bounded by `buffer`.
    unsafe {
        use std::arch::aarch64::*;
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = vld1q_f32(pointer);
            let second = vld1q_f32(pointer.add(4));
            let third = vld1q_f32(pointer.add(8));
            let fourth = vld1q_f32(pointer.add(12));
            vst1q_f32(pointer, vnegq_f32(first));
            vst1q_f32(pointer.add(4), vnegq_f32(second));
            vst1q_f32(pointer.add(8), vnegq_f32(third));
            vst1q_f32(pointer.add(12), vnegq_f32(fourth));
            index += 16;
        }
        while index + 4 <= buffer.len() {
            let samples = vld1q_f32(buffer.as_ptr().add(index));
            vst1q_f32(buffer.as_mut_ptr().add(index), vnegq_f32(samples));
            index += 4;
        }
        while index < buffer.len() {
            buffer[index] = -buffer[index];
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn negate_sse2(buffer: &mut [f32]) {
    // SAFETY: SSE2 is baseline on x86_64; every access is bounded by `buffer`.
    unsafe {
        use std::arch::x86_64::*;
        let sign = _mm_set1_ps(-0.0);
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = _mm_loadu_ps(pointer);
            let second = _mm_loadu_ps(pointer.add(4));
            let third = _mm_loadu_ps(pointer.add(8));
            let fourth = _mm_loadu_ps(pointer.add(12));
            _mm_storeu_ps(pointer, _mm_xor_ps(first, sign));
            _mm_storeu_ps(pointer.add(4), _mm_xor_ps(second, sign));
            _mm_storeu_ps(pointer.add(8), _mm_xor_ps(third, sign));
            _mm_storeu_ps(pointer.add(12), _mm_xor_ps(fourth, sign));
            index += 16;
        }
        while index + 4 <= buffer.len() {
            let samples = _mm_loadu_ps(buffer.as_ptr().add(index));
            _mm_storeu_ps(buffer.as_mut_ptr().add(index), _mm_xor_ps(samples, sign));
            index += 4;
        }
        while index < buffer.len() {
            buffer[index] = -buffer[index];
            index += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn negate_avx2(buffer: &mut [f32]) {
    // SAFETY: the caller checked AVX2 support; every access is bounded by `buffer`.
    unsafe {
        use std::arch::x86_64::*;
        let sign = _mm256_set1_ps(-0.0);
        let mut index = 0;
        while index + 32 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            let first = _mm256_loadu_ps(pointer);
            let second = _mm256_loadu_ps(pointer.add(8));
            let third = _mm256_loadu_ps(pointer.add(16));
            let fourth = _mm256_loadu_ps(pointer.add(24));
            _mm256_storeu_ps(pointer, _mm256_xor_ps(first, sign));
            _mm256_storeu_ps(pointer.add(8), _mm256_xor_ps(second, sign));
            _mm256_storeu_ps(pointer.add(16), _mm256_xor_ps(third, sign));
            _mm256_storeu_ps(pointer.add(24), _mm256_xor_ps(fourth, sign));
            index += 32;
        }
        while index + 8 <= buffer.len() {
            let samples = _mm256_loadu_ps(buffer.as_ptr().add(index));
            _mm256_storeu_ps(buffer.as_mut_ptr().add(index), _mm256_xor_ps(samples, sign));
            index += 8;
        }
        while index < buffer.len() {
            buffer[index] = -buffer[index];
            index += 1;
        }
    }
}

/// Apply a mid/side width to interleaved stereo; `1.0` is the identity.
pub fn apply_mid_side_width(buffer: &mut [f32], width: f64) {
    apply_mid_side_width_at(level(), buffer, width);
}

fn apply_mid_side_width_at(level: SimdLevel, buffer: &mut [f32], width: f64) {
    match level {
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => apply_mid_side_width_neon(buffer, width),
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => apply_mid_side_width_sse2(buffer, width),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: dispatch only selects AVX2 after the runtime probe.
        SimdLevel::Avx2 => unsafe { apply_mid_side_width_avx2(buffer, width) },
        _ => apply_mid_side_width_scalar(buffer, width),
    }
}

fn apply_mid_side_width_scalar(buffer: &mut [f32], width: f64) {
    for frame in buffer.as_chunks_mut::<2>().0 {
        let left = frame[0] as f64;
        let right = frame[1] as f64;
        let mid = (left + right) * 0.5;
        let side = (left - right) * 0.5;
        frame[0] = (mid + side * width) as f32;
        frame[1] = (mid - side * width) as f32;
    }
}

#[cfg(target_arch = "aarch64")]
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn mid_side_width_neon_block(
    pointer: *mut f32,
    width: core::arch::aarch64::float64x2_t,
    half: core::arch::aarch64::float64x2_t,
) {
    // SAFETY: the caller guarantees four writable samples at `pointer`.
    unsafe {
        use std::arch::aarch64::*;
        let samples = vld1q_f32(pointer);
        let lefts = vcvt_f64_f32(vget_low_f32(vuzp1q_f32(samples, samples)));
        let rights = vcvt_f64_f32(vget_low_f32(vuzp2q_f32(samples, samples)));
        let mid = vmulq_f64(vaddq_f64(lefts, rights), half);
        let side = vmulq_f64(vsubq_f64(lefts, rights), half);
        let scaled = vmulq_f64(side, width);
        let out_left = vcvt_f32_f64(vaddq_f64(mid, scaled));
        let out_right = vcvt_f32_f64(vsubq_f64(mid, scaled));
        vst1q_f32(
            pointer,
            vzip1q_f32(
                vcombine_f32(out_left, out_left),
                vcombine_f32(out_right, out_right),
            ),
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn apply_mid_side_width_neon(buffer: &mut [f32], width: f64) {
    // SAFETY: NEON is baseline on aarch64; every block stays in bounds.
    unsafe {
        use std::arch::aarch64::*;
        let width_vector = vdupq_n_f64(width);
        let half = vdupq_n_f64(0.5);
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            mid_side_width_neon_block(pointer, width_vector, half);
            mid_side_width_neon_block(pointer.add(4), width_vector, half);
            mid_side_width_neon_block(pointer.add(8), width_vector, half);
            mid_side_width_neon_block(pointer.add(12), width_vector, half);
            index += 16;
        }
        while index + 4 <= buffer.len() {
            mid_side_width_neon_block(buffer.as_mut_ptr().add(index), width_vector, half);
            index += 4;
        }
        for frame in buffer[index..].chunks_exact_mut(2) {
            let left = frame[0] as f64;
            let right = frame[1] as f64;
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5;
            frame[0] = (mid + side * width) as f32;
            frame[1] = (mid - side * width) as f32;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn mid_side_width_sse2_block(
    pointer: *mut f32,
    width: core::arch::x86_64::__m128d,
    half: core::arch::x86_64::__m128d,
) {
    // SAFETY: the caller guarantees four writable samples at `pointer`.
    unsafe {
        use std::arch::x86_64::*;
        let samples = _mm_loadu_ps(pointer);
        let lefts = _mm_cvtps_pd(_mm_shuffle_ps(samples, samples, 0x88));
        let rights = _mm_cvtps_pd(_mm_shuffle_ps(samples, samples, 0xDD));
        let mid = _mm_mul_pd(_mm_add_pd(lefts, rights), half);
        let side = _mm_mul_pd(_mm_sub_pd(lefts, rights), half);
        let scaled = _mm_mul_pd(side, width);
        let out_left = _mm_cvtpd_ps(_mm_add_pd(mid, scaled));
        let out_right = _mm_cvtpd_ps(_mm_sub_pd(mid, scaled));
        _mm_storeu_ps(pointer, _mm_unpacklo_ps(out_left, out_right));
    }
}

#[cfg(target_arch = "x86_64")]
fn apply_mid_side_width_sse2(buffer: &mut [f32], width: f64) {
    // SAFETY: SSE2 is baseline on x86_64; every block stays in bounds.
    unsafe {
        use std::arch::x86_64::*;
        let width_vector = _mm_set1_pd(width);
        let half = _mm_set1_pd(0.5);
        let mut index = 0;
        while index + 16 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            mid_side_width_sse2_block(pointer, width_vector, half);
            mid_side_width_sse2_block(pointer.add(4), width_vector, half);
            mid_side_width_sse2_block(pointer.add(8), width_vector, half);
            mid_side_width_sse2_block(pointer.add(12), width_vector, half);
            index += 16;
        }
        while index + 4 <= buffer.len() {
            mid_side_width_sse2_block(buffer.as_mut_ptr().add(index), width_vector, half);
            index += 4;
        }
        for frame in buffer[index..].as_chunks_mut::<2>().0 {
            let left = frame[0] as f64;
            let right = frame[1] as f64;
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5;
            frame[0] = (mid + side * width) as f32;
            frame[1] = (mid - side * width) as f32;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn mid_side_width_avx2_block(
    pointer: *mut f32,
    width: core::arch::x86_64::__m256d,
    half: core::arch::x86_64::__m256d,
) {
    // SAFETY: the caller guarantees eight writable samples at `pointer`.
    unsafe {
        use std::arch::x86_64::*;
        let samples = _mm256_loadu_ps(pointer);
        let even = _mm256_setr_epi32(0, 2, 4, 6, 0, 0, 0, 0);
        let odd = _mm256_setr_epi32(1, 3, 5, 7, 0, 0, 0, 0);
        let lefts = _mm256_cvtps_pd(_mm256_castps256_ps128(_mm256_permutevar8x32_ps(
            samples, even,
        )));
        let rights = _mm256_cvtps_pd(_mm256_castps256_ps128(_mm256_permutevar8x32_ps(
            samples, odd,
        )));
        let mid = _mm256_mul_pd(_mm256_add_pd(lefts, rights), half);
        let side = _mm256_mul_pd(_mm256_sub_pd(lefts, rights), half);
        let scaled = _mm256_mul_pd(side, width);
        let out_left = _mm256_cvtpd_ps(_mm256_add_pd(mid, scaled));
        let out_right = _mm256_cvtpd_ps(_mm256_sub_pd(mid, scaled));
        _mm_storeu_ps(pointer, _mm_unpacklo_ps(out_left, out_right));
        _mm_storeu_ps(pointer.add(4), _mm_unpackhi_ps(out_left, out_right));
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn apply_mid_side_width_avx2(buffer: &mut [f32], width: f64) {
    // SAFETY: the caller checked AVX2 support; every block stays in bounds.
    unsafe {
        use std::arch::x86_64::*;
        let width_vector = _mm256_set1_pd(width);
        let half = _mm256_set1_pd(0.5);
        let mut index = 0;
        while index + 32 <= buffer.len() {
            let pointer = buffer.as_mut_ptr().add(index);
            mid_side_width_avx2_block(pointer, width_vector, half);
            mid_side_width_avx2_block(pointer.add(8), width_vector, half);
            mid_side_width_avx2_block(pointer.add(16), width_vector, half);
            mid_side_width_avx2_block(pointer.add(24), width_vector, half);
            index += 32;
        }
        while index + 8 <= buffer.len() {
            mid_side_width_avx2_block(buffer.as_mut_ptr().add(index), width_vector, half);
            index += 8;
        }
        for frame in buffer[index..].as_chunks_mut::<2>().0 {
            let left = frame[0] as f64;
            let right = frame[1] as f64;
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5;
            frame[0] = (mid + side * width) as f32;
            frame[1] = (mid - side * width) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic samples across normal values, signed zeros and denormals.
    fn test_buffer(len: usize) -> Vec<f32> {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        (0..len)
            .map(|index| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
                match index % 13 {
                    0 => 0.0,
                    1 => -0.0,
                    2 => f32::from_bits(1),
                    3 => f32::from_bits(0x8000_0001),
                    4 => -unit as f32,
                    _ => (unit * 2.0 - 1.0) as f32,
                }
            })
            .collect()
    }

    fn levels_on_this_host() -> Vec<SimdLevel> {
        let mut levels = vec![SimdLevel::Scalar, SimdLevel::detect()];
        #[cfg(target_arch = "x86_64")]
        {
            levels.push(SimdLevel::Sse2);
            if std::arch::is_x86_feature_detected!("avx2") {
                levels.push(SimdLevel::Avx2);
            }
        }
        #[cfg(target_arch = "aarch64")]
        levels.push(SimdLevel::Neon);
        levels.sort_by_key(|level| level.code());
        levels.dedup();
        levels
    }

    fn assert_same_bits(expected: &[f32], actual: &[f32], context: &str) {
        assert_eq!(expected.len(), actual.len(), "{context}");
        for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
            assert_eq!(
                expected.to_bits(),
                actual.to_bits(),
                "{context} differs at {index}: {expected} vs {actual}"
            );
        }
    }

    #[test]
    fn channel_gains_matches_scalar_bit_for_bit() {
        for len in [0, 1, 2, 3, 4, 7, 8, 15, 16, 31, 97, 1000] {
            let source = test_buffer(len);
            let mut reference = source.clone();
            channel_gains_scalar(&mut reference, 0.25, -1.5);
            for level in levels_on_this_host() {
                let mut actual = source.clone();
                channel_gains_at(level, &mut actual, 0.25, -1.5);
                assert_same_bits(&reference, &actual, &format!("channel_gains {level:?}"));
            }
        }
    }

    #[test]
    fn accumulate_channel_gains_matches_scalar_bit_for_bit() {
        for len in [0, 1, 2, 3, 4, 7, 8, 15, 16, 31, 97, 1000] {
            let source = test_buffer(len);
            let target = test_buffer(len + 3);
            let mut reference = target.clone();
            accumulate_channel_gains_scalar(&mut reference, &source, 0.75, -0.125);
            for level in levels_on_this_host() {
                let mut actual = target.clone();
                accumulate_channel_gains_at(level, &mut actual, &source, 0.75, -0.125);
                assert_same_bits(
                    &reference[..source.len()],
                    &actual[..source.len()],
                    &format!("accumulate_channel_gains {level:?}"),
                );
                assert_same_bits(
                    &target[source.len()..],
                    &actual[source.len()..],
                    &format!("accumulate_channel_gains tail {level:?}"),
                );
            }
        }
    }

    #[test]
    fn uniform_gain_matches_scalar_bit_for_bit() {
        for len in [0, 1, 2, 7, 8, 15, 16, 31, 97, 1000] {
            let source = test_buffer(len);
            let mut reference = source.clone();
            for sample in &mut reference {
                *sample *= 0.333;
            }
            for level in levels_on_this_host() {
                let mut actual = source.clone();
                apply_uniform_gain_at(level, &mut actual, 0.333);
                assert_same_bits(&reference, &actual, &format!("uniform_gain {level:?}"));
            }
        }
    }

    #[test]
    fn negate_matches_scalar_bit_for_bit() {
        for len in [0, 1, 2, 7, 8, 15, 16, 31, 97, 1000] {
            let source = test_buffer(len);
            let reference: Vec<f32> = source.iter().map(|sample| -*sample).collect();
            for level in levels_on_this_host() {
                let mut actual = source.clone();
                negate_at(level, &mut actual);
                assert_same_bits(&reference, &actual, &format!("negate {level:?}"));
            }
        }
    }

    #[test]
    fn mid_side_width_matches_scalar_bit_for_bit() {
        for len in [0, 2, 4, 6, 8, 14, 16, 30, 32, 62, 96, 1000] {
            let source = test_buffer(len);
            for width in [0.0, 1.0, 2.0, 0.37, -0.5] {
                let mut reference = source.clone();
                apply_mid_side_width_scalar(&mut reference, width);
                for level in levels_on_this_host() {
                    let mut actual = source.clone();
                    apply_mid_side_width_at(level, &mut actual, width);
                    assert_same_bits(
                        &reference,
                        &actual,
                        &format!("mid_side_width {level:?} width {width}"),
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "performance smoke test; run with --ignored --nocapture"]
    fn elementwise_kernel_throughput() {
        use std::hint::black_box;
        use std::time::Instant;

        for (len, rounds) in [(1 << 20, 4), (4096, 1024)] {
            let source = test_buffer(len);
            let target = test_buffer(len);
            println!("kernel, working set, level, ns/sample");
            for level in levels_on_this_host() {
                let mut buffer = source.clone();
                let start = Instant::now();
                for _ in 0..rounds {
                    channel_gains_at(level, black_box(&mut buffer), 0.75, -0.25);
                }
                let ns = start.elapsed().as_secs_f64() * 1e9 / (len * rounds) as f64;
                println!("channel_gains, {len}, {level:?}, {ns:.3}");

                let mut mixed = target.clone();
                let start = Instant::now();
                for _ in 0..rounds {
                    accumulate_channel_gains_at(
                        level,
                        black_box(&mut mixed),
                        black_box(&source),
                        0.75,
                        -0.25,
                    );
                }
                let ns = start.elapsed().as_secs_f64() * 1e9 / (len * rounds) as f64;
                println!("accumulate_channel_gains, {len}, {level:?}, {ns:.3}");

                let mut buffer = source.clone();
                let start = Instant::now();
                for _ in 0..rounds {
                    apply_uniform_gain_at(level, black_box(&mut buffer), 0.5);
                }
                let ns = start.elapsed().as_secs_f64() * 1e9 / (len * rounds) as f64;
                println!("apply_uniform_gain, {len}, {level:?}, {ns:.3}");

                let mut buffer = source.clone();
                let start = Instant::now();
                for _ in 0..rounds {
                    negate_at(level, black_box(&mut buffer));
                }
                let ns = start.elapsed().as_secs_f64() * 1e9 / (len * rounds) as f64;
                println!("negate, {len}, {level:?}, {ns:.3}");

                let mut buffer = source.clone();
                let start = Instant::now();
                for _ in 0..rounds {
                    apply_mid_side_width_at(level, black_box(&mut buffer), 0.6);
                }
                let ns = start.elapsed().as_secs_f64() * 1e9 / (len * rounds) as f64;
                println!("mid_side_width, {len}, {level:?}, {ns:.3}");
                black_box(&buffer);
            }
        }
    }

    #[test]
    fn public_kernels_dispatch_to_the_scalar_contract() {
        let source = test_buffer(1024);
        let mut dispatched = source.clone();
        channel_gains(&mut dispatched, 0.5, 0.25);
        let mut reference = source.clone();
        channel_gains_scalar(&mut reference, 0.5, 0.25);
        assert_same_bits(&reference, &dispatched, "public channel_gains");

        let mut dispatched = source.clone();
        apply_mid_side_width(&mut dispatched, 0.6);
        let mut reference = source.clone();
        apply_mid_side_width_scalar(&mut reference, 0.6);
        assert_same_bits(&reference, &dispatched, "public mid_side_width");

        let mut dispatched = source.clone();
        accumulate_channel_gains(&mut dispatched, &source, 0.5, -0.25);
        let mut reference = source.clone();
        accumulate_channel_gains_scalar(&mut reference, &source, 0.5, -0.25);
        assert_same_bits(&reference, &dispatched, "public accumulate_channel_gains");

        let mut dispatched = source.clone();
        apply_uniform_gain(&mut dispatched, 0.375);
        let mut reference = source.clone();
        for sample in &mut reference {
            *sample *= 0.375;
        }
        assert_same_bits(&reference, &dispatched, "public uniform_gain");

        let mut dispatched = source.clone();
        negate(&mut dispatched);
        let reference: Vec<f32> = source.iter().map(|sample| -*sample).collect();
        assert_same_bits(&reference, &dispatched, "public negate");
    }

    #[test]
    fn level_names_round_trip() {
        for level in [
            SimdLevel::Scalar,
            SimdLevel::Sse2,
            SimdLevel::Avx2,
            SimdLevel::Neon,
        ] {
            assert_eq!(SimdLevel::from_name(level.name()), Some(level));
        }
        assert_eq!(SimdLevel::from_name("avx512"), None);
        assert_eq!(SimdLevel::from_name("Scalar"), None);
    }

    #[test]
    fn the_detected_level_is_available() {
        assert!(available(SimdLevel::Scalar));
        assert!(available(SimdLevel::detect()));
    }
}
