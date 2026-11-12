//! Source insert preparation and offline latency alignment.

use oc_dsl::ast::{Project, Track};

/// Build only the preset and pattern inserts owned by one source. Sidechain
/// dependencies belong on the hosting mixer track, after sources are summed.
pub fn effect_chain(
    project: &Project,
    lane: &Track,
    sample_rate: u32,
) -> Result<crate::graph::EffectChain, String> {
    let chain =
        crate::graph::EffectChain::try_from_effects(&super::effects(project, lane), sample_rate)?;
    if chain.has_sidechain() {
        return Err("Source effect sidechains must be placed on the parent mixer track".into());
    }
    Ok(chain)
}

/// Delay a completed stereo source without losing its final frames. The graph
/// reserves the reported source latency when sizing its mixing buffers.
#[cfg(any(feature = "plugin-host", test))]
pub(crate) fn align(buffer: &mut Vec<f32>, frames: u32) -> Result<(), String> {
    let offset = usize::try_from(frames)
        .ok()
        .and_then(|frames| frames.checked_mul(2))
        .ok_or("Source lane alignment exceeds platform size")?;
    if offset == 0 {
        return Ok(());
    }
    let length = buffer.len();
    let total = length
        .checked_add(offset)
        .ok_or("Source lane alignment length overflow")?;
    buffer.resize(total, 0.0);
    buffer.copy_within(..length, offset);
    buffer[..offset].fill(0.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_limiter_and_dry_impulses_align_without_losing_the_tail() {
        let project = oc_dsl::parse("@project\n@track wet | instrument: synth/lead\n  effects:\n    - limiter: ceiling=-0.8dB\n").unwrap();
        let mut chain = effect_chain(&project, &project.tracks[0], 48_000).unwrap();
        let latency = chain.latency_samples();
        assert!(latency > 0);
        let mut wet = vec![0.0; (latency as usize + 16) * 2];
        wet[0] = 0.1;
        wet[1] = 0.1;
        chain.process(&mut wet, 48_000);
        let mut dry = vec![0.0; 32];
        dry[0] = 0.1;
        dry[1] = 0.1;
        align(&mut dry, latency).unwrap();
        assert_eq!(wet, dry);
        let mut last_frame = vec![0.0, 0.0, 0.2, -0.2];
        align(&mut last_frame, latency).unwrap();
        assert_eq!(&last_frame[last_frame.len() - 2..], &[0.2, -0.2]);
        assert_eq!(
            last_frame.iter().filter(|sample| **sample != 0.0).count(),
            2
        );
    }
}
