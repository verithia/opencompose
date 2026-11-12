//! Named-pattern leaf rendering.

use super::*;

impl RenderCache {
    /// Render named patterns (leaf nodes of DAG) — each rendered once, cached.
    pub(super) fn render_patterns(
        &mut self,
        project: &Project,
        soundfont_path: &Path,
        sample_rate: u32,
        tempo: f64,
    ) -> Result<(), String> {
        // Check if any patterns actually need rendering before loading a bank.
        if !project.properties.tempo_map.is_empty() || !project.properties.time_sig_map.is_empty() {
            // Preview audio has no clip placement. Do not reuse a constant-clock
            // buffer in a mapped project; full tracks use the actual interpreted clips.
            self.pattern_buffers.clear();
            for pattern in &project.patterns {
                self.pattern_dirty.insert(pattern.name.clone(), true);
            }
            return Ok(());
        }
        let any_dirty = project
            .patterns
            .iter()
            .any(|p| self.pattern_dirty.get(&p.name).copied().unwrap_or(true));
        if !any_dirty {
            return Ok(());
        }

        // Preview audio is diagnostic: nothing in the mix reads it, and patterns
        // render through the SoundFont player whatever their preset says. No
        // configured bank is a normal state (a project of builtin presets needs
        // none), so it leaves the previews empty instead of failing the render.
        if soundfont_path.as_os_str().is_empty() {
            return Ok(());
        }
        let mut renderer = match SoundFontRenderer::new(sample_rate, soundfont_path) {
            Ok(renderer) => renderer,
            Err(error) => {
                log::warn!("pattern previews skipped: {error}");
                return Ok(());
            }
        };
        renderer.enable_drum_channel();

        for (pat_idx, pattern) in project.patterns.iter().enumerate() {
            if !self
                .pattern_dirty
                .get(&pattern.name)
                .copied()
                .unwrap_or(true)
            {
                continue; // already cached and clean
            }

            // Assign unique channels per pattern to avoid instrument conflicts.
            // Channel 9 is reserved for drums (GM standard).
            // Non-drum patterns get channels 0-8, 10-15 (skipping 9).
            let channel: u8 = if pattern.instrument.category == "drums" {
                9
            } else {
                let ch = pat_idx % 15;
                if ch >= 9 { (ch + 1) as u8 } else { ch as u8 }
            };

            // Send program change for this pattern's instrument
            let program = interpret::instrument_to_gm_program(&pattern.instrument);
            renderer.send(&MidiMessage::ProgramChange { channel, program })?;

            let mut events = Vec::new();

            for ev in &pattern.events {
                if ev.modifiers.iter().any(|(k, _)| k == "rest") {
                    continue;
                }
                let velocity = ev
                    .velocity
                    .unwrap_or_else(|| ev.dynamics.map(|d| d.to_velocity()).unwrap_or(80));
                let note =
                    crate::interpret::transpose_pattern_note(ev.pitch.to_midi(), pattern.transpose);
                let note_on = crate::interpret::beats_to_samples(ev.start_beat, tempo, sample_rate);
                let note_off = crate::interpret::beats_to_samples(
                    ev.start_beat + ev.duration.to_beats(),
                    tempo,
                    sample_rate,
                );
                events.push(MidiEvent {
                    time_samples: note_on,
                    event: MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity,
                    },
                    track_index: 0,
                });
                events.push(MidiEvent {
                    time_samples: note_off,
                    event: MidiMessage::NoteOff { channel, note },
                    track_index: 0,
                });
            }

            for hit in &pattern.hits {
                let note = crate::interpret::drum_to_gm_note(&hit.drum);
                let vel = hit
                    .velocity
                    .unwrap_or_else(|| hit.dynamics.map(|d| d.to_velocity()).unwrap_or(100));
                let hit_beat = hit.written_quarters();
                let on = crate::interpret::beats_to_samples(hit_beat, tempo, sample_rate);
                let off = crate::interpret::beats_to_samples(hit_beat + 0.125, tempo, sample_rate);
                events.push(MidiEvent {
                    time_samples: on,
                    event: MidiMessage::NoteOn {
                        channel: 9,
                        note,
                        velocity: vel,
                    },
                    track_index: 0,
                });
                events.push(MidiEvent {
                    time_samples: off,
                    event: MidiMessage::NoteOff { channel: 9, note },
                    track_index: 0,
                });
            }

            events.sort_by_key(|e| e.time_samples);
            let max_time = events.iter().map(|e| e.time_samples).max().unwrap_or(0);
            let tail = sample_rate as u64;
            let duration = (max_time + tail) as f64 / sample_rate as f64;
            if duration > self.duration_seconds {
                self.duration_seconds = duration;
            }

            let buffer = renderer.try_render_events(&events, duration)?;
            self.pattern_buffers.insert(pattern.name.clone(), buffer);
            self.pattern_dirty.insert(pattern.name.clone(), false);
        }
        Ok(())
    }
}
