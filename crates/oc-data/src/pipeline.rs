//! Batch pipeline: convert a directory of MIDI files to (DSL, WAV) training pairs

use std::path::{Path, PathBuf};

use oc_dsl::serializer;
use oc_engine::render;

use crate::midi2dsl;

/// Result of processing a single MIDI file
#[derive(Debug, serde::Serialize)]
pub struct ProcessResult {
    pub midi_path: String,
    pub ocp_path: String,
    pub wav_path: String,
    pub tracks: usize,
    pub success: bool,
    pub error: Option<String>,
    /// Shared import losses/repairs, including when strict acceptance refuses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_summary: Option<oc_engine::midi::ImportSummary>,
}

/// Process a directory of MIDI files into (DSL, WAV) pairs
pub fn process_directory(
    midi_dir: &Path,
    output_dir: &Path,
    soundfont_path: &Path,
    sample_rate: u32,
) -> Vec<ProcessResult> {
    // Find all .mid / .midi files
    let midi_files = find_midi_files(midi_dir);
    println!(
        "Found {} MIDI files in {}",
        midi_files.len(),
        midi_dir.display()
    );

    // Create output directories
    let ocp_dir = output_dir.join("ocp");
    let wav_dir = output_dir.join("wav");
    let _ = std::fs::create_dir_all(&ocp_dir);
    let _ = std::fs::create_dir_all(&wav_dir);

    let mut results = Vec::new();
    let total = midi_files.len();

    for (i, midi_path) in midi_files.iter().enumerate() {
        let stem = midi_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let ocp_path = ocp_dir.join(format!("{}.ocp", stem));
        let wav_path = wav_dir.join(format!("{}.wav", stem));

        print!("[{}/{}] {} ... ", i + 1, total, stem);

        let result = process_single(midi_path, &ocp_path, &wav_path, soundfont_path, sample_rate);
        match &result.error {
            None => println!("OK ({} tracks)", result.tracks),
            Some(e) => println!("FAILED: {}", e),
        }
        results.push(result);
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.iter().filter(|r| !r.success).count();
    println!(
        "\nDone: {} succeeded, {} failed, {} total",
        succeeded, failed, total
    );

    results
}

/// Process a single MIDI file → (.ocp, .wav) pair
pub fn process_single(
    midi_path: &Path,
    ocp_path: &Path,
    wav_path: &Path,
    soundfont_path: &Path,
    sample_rate: u32,
) -> ProcessResult {
    let mut result = ProcessResult {
        midi_path: midi_path.display().to_string(),
        ocp_path: ocp_path.display().to_string(),
        wav_path: wav_path.display().to_string(),
        tracks: 0,
        success: false,
        error: None,
        import_summary: None,
    };

    // Step 1: MIDI → AST
    let imported = match midi2dsl::midi_to_import(midi_path) {
        Ok(imported) => imported,
        Err(e) => {
            result.error = Some(format!("MIDI parse: {}", e));
            return result;
        }
    };

    result.import_summary = Some(imported.summary.clone());
    if let Err(error) = midi2dsl::validate_dataset_summary(&imported.summary) {
        result.error = Some(format!("MIDI acceptance: {error}"));
        return result;
    }
    let project = imported.project;
    result.tracks = project.tracks.len();

    if project.tracks.is_empty() {
        result.error = Some("No tracks found".into());
        return result;
    }

    // Step 2: AST → .ocp text
    let ocp_text = serializer::serialize(&project);

    // Verify the serialized text parses back
    if let Err(e) = oc_dsl::parse(&ocp_text) {
        result.error = Some(format!("Round-trip parse failed: {}", e));
        return result;
    }

    // Write .ocp file
    if let Err(e) = std::fs::write(ocp_path, &ocp_text) {
        result.error = Some(format!("Write .ocp: {}", e));
        return result;
    }

    // Step 3: Render → .wav
    if let Err(e) = render::render_to_wav(
        &project,
        soundfont_path,
        wav_path,
        oc_engine::encode::EncodeSpec::for_project(&project, sample_rate),
    ) {
        result.error = Some(format!("Render: {}", e));
        return result;
    }

    result.success = true;
    result
}

/// Recursively find all MIDI files in a directory
fn find_midi_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(find_midi_files(&path));
            } else if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if ext == "mid" || ext == "midi" {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}
