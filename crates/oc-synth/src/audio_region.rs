//! Sample-accurate audio-region primitive. No time stretch, resampling or hidden fades.
//! Callers decode media and declare its rate; project/GUI integration is separate.

/// Built-in plugin id that hosts a region document.
pub const PLUGIN_ID: &str = "builtin.audio_regions";

/// A rate-tagged list of regions rendered into one stereo stream.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionDocument {
    pub sample_rate: u32,
    pub regions: Vec<FileRegion>,
}

/// One region, either from a media file or from a nested document.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRegion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<std::path::PathBuf>,
    /// Editable compound source, rendered before the containing region's envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_document: Option<Box<RegionDocument>>,
    pub region: AudioRegion,
    /// Optional gain applied to already rendered lower layers before adding this region.
    /// Document order is bottom-to-top. Absent means additive mixing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlay_envelope: Option<Vec<(u64, f32)>>,
}

/// Decode the region document named by the patch parameters, resolving a
/// relative path against `root`. Returns the document and the file it came from.
pub fn load_document(
    params: &[(String, oc_dsl::ast::ParamValue)],
    root: Option<&std::path::Path>,
) -> Result<(RegionDocument, std::path::PathBuf), String> {
    use oc_dsl::ast::ParamValue;
    let value = params
        .iter()
        .rev()
        .find(|(key, _)| key == "audio_region_state_file")
        .ok_or("audio_region_state_file required")?;
    let ParamValue::String(name) = &value.1 else {
        return Err("region state must be a path".into());
    };
    let path = std::path::Path::new(name);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.ok_or("relative region state requires resource root")?
            .join(path)
    };
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
        return Err("region document exceeds 4MiB".into());
    }
    let document: RegionDocument =
        serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if document.sample_rate == 0 || document.regions.is_empty() {
        return Err("invalid region document".into());
    }
    Ok((document, path))
}

impl RegionDocument {
    /// Every media path the document references, nested documents included.
    pub fn media_files(&self) -> Result<Vec<&std::path::Path>, String> {
        fn visit<'a>(
            doc: &'a RegionDocument,
            depth: usize,
            files: &mut Vec<&'a std::path::Path>,
        ) -> Result<(), String> {
            if depth > 8 {
                return Err("compound region nesting exceeds eight levels".into());
            }
            for item in &doc.regions {
                match (&item.file, &item.source_document) {
                    (Some(file), None) => files.push(file.as_path()),
                    (None, Some(child)) => visit(child, depth + 1, files)?,
                    _ => return Err("region requires exactly one file or source_document".into()),
                }
            }
            Ok(())
        }
        let mut files = Vec::new();
        visit(self, 0, &mut files)?;
        Ok(files)
    }
    /// The exclusive end frame at the declared rate.
    pub fn end_frame(&self, rate: u32) -> Result<u64, String> {
        if rate != self.sample_rate {
            return Err("audio-region sample rate mismatch".into());
        }
        let mut end = 0;
        for item in &self.regions {
            let next = item
                .region
                .timeline_start
                .checked_add(item.region.length)
                .ok_or("region overflow")?;
            if item.region.length == 0 || next > rate as u64 * 1800 {
                return Err("region duration invalid or exceeds 30min bound".into());
            }
            end = end.max(next);
        }
        Ok(end)
    }

    /// Render the whole document into interleaved stereo frames.
    pub fn render(
        &self,
        document_path: &std::path::Path,
        frames: usize,
        rate: u32,
    ) -> Result<Vec<f32>, String> {
        self.media_files()?;
        self.render_bounded(document_path, 0, frames, rate, rate as usize * 1800)
    }

    /// Render `frames` from `start_frame` without requiring the whole document.
    pub fn render_window(
        &self,
        document_path: &std::path::Path,
        start_frame: u64,
        frames: usize,
        rate: u32,
    ) -> Result<Vec<f32>, String> {
        self.media_files()?;
        self.render_bounded(
            document_path,
            start_frame,
            frames,
            rate,
            rate as usize * 1800,
        )
    }

    fn render_bounded(
        &self,
        document_path: &std::path::Path,
        start_frame: u64,
        frames: usize,
        rate: u32,
        budget: usize,
    ) -> Result<Vec<f32>, String> {
        if frames > budget {
            return Err("compound region render memory bound exceeded".into());
        }
        if start_frame == 0 && self.end_frame(rate)? > frames as u64 {
            return Err("output would truncate regions".into());
        }
        let mut output = vec![0.0; frames.checked_mul(2).ok_or("output overflow")?];
        for item in &self.regions {
            let stereo = if let Some(child) = &item.source_document {
                let child_frames = usize::try_from(child.end_frame(rate)?)
                    .map_err(|_| "compound length overflow")?;
                child.render_bounded(document_path, 0, child_frames, rate, budget - frames)?
            } else {
                let path = document_path
                    .parent()
                    .ok_or("missing resource root")?
                    .join(item.file.as_ref().ok_or("missing region file")?);
                if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 128 * 1024 * 1024 {
                    return Err("region media exceeds 128MiB".into());
                }
                let mut wav = oc_audio::wav::open_reader(&path).map_err(|e| e.to_string())?;
                let spec = wav.spec();
                if spec.sample_rate != rate || !(1..=2).contains(&spec.channels) {
                    return Err("region media rate/channels unsupported".into());
                }
                let samples: Vec<f32> = match spec.sample_format {
                    hound::SampleFormat::Float => wav
                        .samples::<f32>()
                        .collect::<Result<_, _>>()
                        .map_err(|e| e.to_string())?,
                    hound::SampleFormat::Int => {
                        if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                            return Err("invalid PCM width".into());
                        }
                        let scale = 2.0_f64.powi(spec.bits_per_sample as i32 - 1);
                        wav.samples::<i32>()
                            .map(|v| v.map(|v| (v as f64 / scale) as f32))
                            .collect::<Result<_, _>>()
                            .map_err(|e| e.to_string())?
                    }
                };
                if spec.channels == 1 {
                    samples.iter().flat_map(|v| [*v, *v]).collect()
                } else {
                    samples
                }
            };
            item.region.validate(&stereo)?;
            if let Some(points) = &item.underlay_envelope {
                let mut mask = item.region.clone();
                mask.envelope = points.clone();
                mask.validate(&stereo)?;
                if points.is_empty() || points.iter().any(|(_, v)| !(0.0..=1.0).contains(v)) {
                    return Err("invalid underlay envelope".into());
                }
                let block_end = start_frame.saturating_add(frames as u64);
                let first = start_frame.max(item.region.timeline_start);
                let last = block_end.min(item.region.timeline_start + item.region.length);
                for frame in first..last {
                    let relative = frame - item.region.timeline_start;
                    let dst = ((frame - start_frame) * 2) as usize;
                    let gain = mask.envelope_gain(relative);
                    output[dst] *= gain;
                    output[dst + 1] *= gain;
                }
            }
            item.region
                .mix_into(&stereo, rate, &mut output, rate, start_frame)?;
        }
        Ok(output)
    }
}

/// One placed region with its source offset, length, gain and envelope.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioRegion {
    pub timeline_start: u64,
    pub source_start: u64,
    pub length: u64,
    pub gain: f32,
    /// Linear gain points at region-relative sample positions; endpoints hold.
    pub envelope: Vec<(u64, f32)>,
}

impl AudioRegion {
    fn validate(&self, source: &[f32]) -> Result<(), String> {
        if !source.len().is_multiple_of(2) || !self.gain.is_finite() || self.length == 0 {
            return Err("invalid stereo source or region".into());
        }
        let end = self
            .source_start
            .checked_add(self.length)
            .ok_or("source overflow")?;
        if end > (source.len() / 2) as u64 || self.timeline_start.checked_add(self.length).is_none()
        {
            return Err("region exceeds source or timeline bounds".into());
        }
        if self
            .envelope
            .iter()
            .any(|(at, value)| *at > self.length || !value.is_finite())
            || self.envelope.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return Err("invalid envelope points".into());
        }
        Ok(())
    }

    fn envelope_gain(&self, frame: u64) -> f32 {
        let Some(first) = self.envelope.first() else {
            return 1.0;
        };
        if frame <= first.0 {
            return first.1;
        }
        for pair in self.envelope.windows(2) {
            if frame <= pair[1].0 {
                let fraction = (frame - pair[0].0) as f64 / (pair[1].0 - pair[0].0) as f64;
                return (pair[0].1 as f64 + fraction * (pair[1].1 as f64 - pair[0].1 as f64))
                    as f32;
            }
        }
        self.envelope.last().unwrap().1
    }

    /// Add the region intersection to interleaved stereo output at an absolute frame.
    /// All validation precedes output mutation. Source and output rates must match.
    pub fn mix_into(
        &self,
        source: &[f32],
        source_rate: u32,
        output: &mut [f32],
        output_rate: u32,
        block_start: u64,
    ) -> Result<(), String> {
        self.validate(source)?;
        if source_rate == 0 || source_rate != output_rate || !output.len().is_multiple_of(2) {
            return Err("rate mismatch or invalid output".into());
        }
        let block_end = block_start
            .checked_add((output.len() / 2) as u64)
            .ok_or("block overflow")?;
        let start = block_start.max(self.timeline_start);
        let end = block_end.min(self.timeline_start + self.length);
        // Check only this block, not the entire media file on every render call.
        for frame in start..end {
            let relative = frame - self.timeline_start;
            let src = ((self.source_start + relative) * 2) as usize;
            let dst = ((frame - block_start) * 2) as usize;
            let gain = self.gain * self.envelope_gain(relative);
            if !(output[dst] + source[src] * gain).is_finite()
                || !(output[dst + 1] + source[src + 1] * gain).is_finite()
            {
                return Err("non-finite region mix".into());
            }
        }
        for frame in start..end {
            let relative = frame - self.timeline_start;
            let src = ((self.source_start + relative) * 2) as usize;
            let dst = ((frame - block_start) * 2) as usize;
            let gain = self.gain * self.envelope_gain(relative);
            output[dst] += source[src] * gain;
            output[dst + 1] += source[src + 1] * gain;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_regions_attenuate_lower_layer() {
        let root = std::env::temp_dir().join(format!(
            "ocp-layer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut writer = hound::WavWriter::create(
            root.join("source.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: 8000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..8 {
            writer.write_sample(1.0_f32).unwrap();
        }
        writer.finalize().unwrap();
        let source = root.join("source.wav");
        let mut bytes = std::fs::read(&source).unwrap();
        bytes.splice(12..12, *b"JUNK\x01\0\0\0A\0");
        let size = (bytes.len() as u32 - 8).to_le_bytes();
        bytes[4..8].copy_from_slice(&size);
        std::fs::write(source, bytes).unwrap();
        let mut doc: RegionDocument = serde_json::from_value(serde_json::json!({
            "sample_rate":8000, "regions":[
                {"file":"source.wav", "region":{"timeline_start":0,"source_start":0,"length":8,"gain":0.25,"envelope":[]}},
                {"file":"source.wav", "region":{"timeline_start":2,"source_start":0,"length":4,"gain":0.5,"envelope":[[0,0],[2,1]]},
                 "underlay_envelope":[[0,1],[2,0]]}
            ]
        })).unwrap();
        let rendered = doc.render(&root.join("state.json"), 8, 8000).unwrap();
        assert_eq!(
            rendered.chunks_exact(2).map(|x| x[0]).collect::<Vec<_>>(),
            vec![0.25, 0.25, 0.25, 0.375, 0.5, 0.5, 0.25, 0.25]
        );
        let window = doc
            .render_window(&root.join("state.json"), 2, 4, 8000)
            .unwrap();
        assert_eq!(window, rendered[4..12]);
        let mut compound: RegionDocument = serde_json::from_value(serde_json::json!({
            "sample_rate":8000,"regions":[{
                "source_document":serde_json::to_value(&doc).unwrap(),
                "region":{"timeline_start":1,"source_start":2,"length":4,"gain":0.5,"envelope":[]}
            }]
        }))
        .unwrap();
        assert_eq!(compound.media_files().unwrap().len(), 2);
        let nested = compound.render(&root.join("state.json"), 6, 8000).unwrap();
        assert_eq!(
            nested.chunks_exact(2).map(|x| x[0]).collect::<Vec<_>>(),
            vec![0.0, 0.125, 0.1875, 0.25, 0.25, 0.0]
        );
        assert!(
            compound
                .render_bounded(&root.join("state.json"), 0, 6, 8000, 10)
                .is_err()
        );
        compound.regions[0].file = Some(root.join("source.wav"));
        assert!(compound.render(&root.join("state.json"), 6, 8000).is_err());
        doc.regions[1].underlay_envelope = Some(vec![(0, f32::NAN)]);
        assert!(doc.render(&root.join("state.json"), 8, 8000).is_err());
        std::fs::remove_file(root.join("source.wav")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn offset_envelope_and_block_partition_match() {
        let source: Vec<f32> = (0..20).map(|sample| sample as f32).collect();
        let region = AudioRegion {
            timeline_start: 2,
            source_start: 3,
            length: 4,
            gain: 0.5,
            envelope: vec![(0, 0.0), (3, 1.0)],
        };
        let mut whole = vec![0.0; 20];
        region
            .mix_into(&source, 44100, &mut whole, 44100, 0)
            .unwrap();
        let mut blocks = vec![0.0; 20];
        for (index, block) in blocks.chunks_mut(4).enumerate() {
            region
                .mix_into(&source, 44100, block, 44100, index as u64 * 2)
                .unwrap();
        }
        assert_eq!(whole, blocks);
        assert_eq!(whole[10], 6.0);
        assert_eq!(whole[12], 0.0);
    }

    #[test]
    fn invalid_region_does_not_modify_output() {
        let mut output = vec![0.25; 8];
        let region = AudioRegion {
            timeline_start: 0,
            source_start: 8,
            length: 4,
            gain: 1.0,
            envelope: vec![],
        };
        assert!(
            region
                .mix_into(&[1.0; 8], 44100, &mut output, 44100, 0)
                .is_err()
        );
        assert_eq!(output, vec![0.25; 8]);
    }

    #[test]
    fn nonfinite_mix_fails_before_partial_write() {
        let region = AudioRegion {
            timeline_start: 0,
            source_start: 0,
            length: 2,
            gain: 2.0,
            envelope: vec![],
        };
        let mut output = vec![0.0; 4];
        assert!(
            region
                .mix_into(&[1.0, 1.0, f32::MAX, 1.0], 44100, &mut output, 44100, 0)
                .is_err()
        );
        assert_eq!(output, vec![0.0; 4]);
    }
}
