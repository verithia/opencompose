//! Target selection for exact device references, not a cross-platform math law.
//! See docs/components/oc-dsp.md for capture profile and supported targets.

/// Preserve the original reference unless this exact native target has a
/// separately verified reference. No device implementation depends on this helper.
pub(crate) const fn select(original: u64, linux_gnu_x86_64: u64, windows_msvc_x86_64: u64) -> u64 {
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        linux_gnu_x86_64
    } else if cfg!(all(
        target_os = "windows",
        target_arch = "x86_64",
        target_env = "msvc"
    )) {
        windows_msvc_x86_64
    } else {
        original
    }
}
