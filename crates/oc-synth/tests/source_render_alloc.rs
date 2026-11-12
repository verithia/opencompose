//! Both source engines must render without touching the heap.

use oc_synth::synth::granular::{GrainWindow, GranularPatch, GranularVoice, SourceSample};
use oc_synth::synth::spectral;
use oc_synth::synth::spectral::{SpectralPatch, SpectralVoice, SpectralWarp};
use oc_synth::synth::{OpenSynth, SynthPatch, Waveform};

/// Counts heap operations on the measuring thread; rendering here is single-threaded.
#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

fn write_tone(path: &std::path::Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for frame in 0..44_100 {
        let value = (std::f64::consts::TAU * 441.0 * frame as f64 / 44_100.0).sin();
        writer
            .write_sample((value * 0.5 * f64::from(i16::MAX)) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
}

fn measure(render: impl FnOnce()) -> usize {
    oc_test_support::operations(render)
}

#[test]
fn source_engines_render_without_allocating() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-source-alloc-{}.wav",
        std::process::id()
    ));
    write_tone(&path);
    let sample = SourceSample::load(&path).unwrap();

    let granular_patch = GranularPatch {
        sample: path.clone(),
        root_note: 60.0,
        density: 80.0,
        grain_length: 0.02,
        position: 0.05,
        loop_start: 0.0,
        loop_end: 0.0,
        ping_pong: false,
        scan_rate: 0.0,
        window: GrainWindow::Hann,
        window_param: 0.5,
        window_skew: 0.0,
        random_offset: 0.1,
        random_length: 0.1,
        random_pitch: 0.2,
        random_direction: 0.1,
        random_pan: 0.2,
        random_gain: 0.1,
        reverse: false,
        pitch_track: true,
        seed: 9,
    };
    let mut grain_voice = GranularVoice::new(granular_patch.seed);
    let mut granular_peak = 0.0f64;
    let granular_allocations = measure(|| {
        for _ in 0..4_096 {
            let (left, _right) =
                grain_voice.render(&sample, &granular_patch, 1.0, 0.0, 0.0, 48_000.0);
            granular_peak = granular_peak.max(left.abs());
        }
    });

    let spectral_source = spectral::SpectralSource::load(&path).unwrap();
    let spectral_patch = SpectralPatch {
        sample: path.clone(),
        root_note: 60.0,
        position: 0.05,
        loop_start: 0.0,
        loop_end: 0.0,
        ping_pong: false,
        scan_rate: 0.0,
        pitch_track: true,
        warp: SpectralWarp::Comb,
        morph: 0.5,
        warp2: SpectralWarp::Off,
        morph2: 0.0,
        bend: 0.0,
        phase_lock: 1.0,
        transients: false,
        low_hz: 0.0,
        high_hz: 0.0,
        band_smooth: false,
        seed: 1,
    };
    let mut spectral_voice = SpectralVoice::new();
    let ifft = rustfft::FftPlanner::<f64>::new().plan_fft_inverse(spectral::FFT_SIZE);
    let mut spectral_peak = 0.0f64;
    let spectral_allocations = measure(|| {
        for _ in 0..4_096 {
            let value = spectral_voice.render(
                &spectral_source,
                &spectral_patch,
                1.0,
                0.0,
                0.0,
                48_000.0,
                &*ifft,
            );
            spectral_peak = spectral_peak.max(value.abs());
        }
    });

    assert_eq!(
        granular_allocations, 0,
        "granular render allocated {granular_allocations} times"
    );
    assert_eq!(
        spectral_allocations, 0,
        "spectral render allocated {spectral_allocations} times"
    );
    assert!(granular_peak > 1.0e-4, "granular render was silent");
    assert!(spectral_peak > 1.0e-4, "spectral render was silent");
    let _ = std::fs::remove_file(&path);
}

/// A source patch for one oscillator slot, matching the synth's own tests.
fn source_patch(waveform: Waveform, sample: &std::path::Path) -> SynthPatch {
    let mut patch = SynthPatch {
        filter_wet: 0.0,
        oscillator_a_level: 1.0,
        oscillator_a_waveform: waveform,
        ..Default::default()
    };
    match waveform {
        Waveform::Granular => {
            patch.granular[0] = Some(GranularPatch {
                sample: sample.to_path_buf(),
                root_note: 60.0,
                density: 60.0,
                grain_length: 0.03,
                position: 0.1,
                loop_start: 0.0,
                loop_end: 0.0,
                ping_pong: false,
                scan_rate: 0.0,
                window: GrainWindow::Hann,
                window_param: 0.5,
                window_skew: 0.0,
                random_offset: 0.2,
                random_length: 0.1,
                random_pitch: 0.3,
                random_direction: 0.2,
                random_pan: 0.4,
                random_gain: 0.1,
                reverse: false,
                pitch_track: true,
                seed: 12_345,
            });
        }
        Waveform::Spectral => {
            patch.spectral[0] = Some(SpectralPatch {
                sample: sample.to_path_buf(),
                root_note: 60.0,
                position: 0.1,
                loop_start: 0.0,
                loop_end: 0.0,
                ping_pong: false,
                scan_rate: 0.0,
                pitch_track: true,
                warp: SpectralWarp::Comb,
                morph: 0.5,
                warp2: SpectralWarp::Off,
                morph2: 0.0,
                bend: 0.0,
                phase_lock: 1.0,
                transients: false,
                low_hz: 0.0,
                high_hz: 0.0,
                band_smooth: false,
                seed: 1,
            });
        }
        _ => unreachable!("source patch needs a source waveform"),
    }
    patch
}

/// Note-on must reuse the engines the synth built at construction; the older
/// test only measured rendering after the engine already existed.
#[test]
fn source_note_on_does_not_allocate() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-source-note-on-alloc-{}.wav",
        std::process::id()
    ));
    write_tone(&path);
    for waveform in [Waveform::Granular, Waveform::Spectral] {
        let mut synth = OpenSynth::new(source_patch(waveform, &path));
        // More note-ons than the default polyphony exercise the steal path too.
        let allocations = measure(|| {
            for offset in 0..40u8 {
                synth.note_on(0, 48 + offset % 12, 100);
            }
        });
        assert_eq!(
            allocations, 0,
            "{waveform:?} note-on allocated {allocations} times"
        );
        let mut peak = 0.0f32;
        for _ in 0..1_024 {
            let [left, right] = synth.next_stereo(48_000);
            peak = peak.max(left.abs()).max(right.abs());
        }
        assert!(peak > 1.0e-4, "{waveform:?} note-on was silent");
    }
    let _ = std::fs::remove_file(&path);
}
