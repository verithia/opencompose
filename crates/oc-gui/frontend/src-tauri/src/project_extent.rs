//! Desktop arrangement bounds, shared by streaming, cached playback and export.
use oc_dsl::ast::Project;

/// Longest declared arrangement the desktop renderer allocates for, in seconds.
pub(crate) const RENDER_BUDGET_SECONDS: f64 = 600.0;

/// Arrangement length in seconds: the expanded sections plus the last container,
/// read on the clock the arrangement plays. Available before any playback.
pub fn arrangement_seconds(project: &Project) -> Result<f64, String> {
    let quarter = oc_dsl::arrangement::content_end_quarter(project)?;
    let seconds = oc_dsl::arrangement::rendered_clock(project)?.seconds_at_quarter(quarter)?;
    if !seconds.is_finite() {
        return Err("arrangement duration is not finite".into());
    }
    Ok(seconds)
}

/// Rendered arrangement length, including section repeats and the first
/// section's written start. This is a minimum, never a truncation of events.
pub fn section_seconds(project: &Project) -> Result<f64, String> {
    let Some(seconds) = oc_dsl::arrangement::rendered_end_seconds(project)? else {
        return Ok(0.0);
    };
    // Match the explicit offline renderer's allocation budget. Fail visibly
    // rather than silently shorten a project that exceeds the supported bound.
    if !seconds.is_finite() || seconds > RENDER_BUDGET_SECONDS {
        return Err(format!(
            "Declared section timeline exceeds the {RENDER_BUDGET_SECONDS:.0}-second desktop render budget"
        ));
    }
    Ok(seconds)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use oc_dsl::ast::{BarBeat, Section, TimeRange};

    pub(crate) fn add_section(project: &mut Project, end_bar: u32) {
        project.sections.push(Section {
            name: "arrangement".into(),
            time_range: TimeRange {
                start: BarBeat::new(1, 1, 0),
                end: BarBeat::new(end_bar, 1, 0),
            },
            repeat: None,
            label: None,
            flags: None,
        });
    }

    /// A repeated verse holding a tempo change, then an outro at a new tempo.
    const REPEAT_TEMPO: &str = r#"@ocp "0.2.0"

@project "repeat tempo"
  tempo: 120
  time_signature: 4/4
  tempo_map:
    3:1 = 240
    7:1 = 60

@preset native
  source: builtin
  plugin: "builtin.open_synth"

@track synth | preset: native, instrument: synth/lead_square
  @phrase head [1:1 - 2:1]
    C4 w mf

@section verse [1:1 - 5:1]
  repeat: 2

@section outro [5:1 - 9:1]
"#;

    #[test]
    fn extent_uses_the_project_clock_and_rejects_invalid_or_unbounded_ranges() {
        let mut project =
            oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    2:1 = 60 bpm\n").unwrap();
        assert_eq!(section_seconds(&project).unwrap(), 0.0);
        add_section(&mut project, 3);
        assert_eq!(section_seconds(&project).unwrap(), 6.0);
        project.sections[0].time_range.end.bar = 1;
        assert!(section_seconds(&project).is_err());
        project.sections[0].time_range.end.bar = 200;
        assert!(section_seconds(&project).is_err());
    }

    #[test]
    fn slow_tempo_long_form_is_not_clamped_to_old_four_minute_budget() {
        let mut project = oc_dsl::parse("@project\n  tempo: 45\n").unwrap();
        add_section(&mut project, 49);
        assert_eq!(section_seconds(&project).unwrap(), 256.0);
    }

    #[test]
    fn extent_includes_section_repeats() {
        let mut project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        add_section(&mut project, 3);
        project.sections[0].repeat = Some(2);
        assert_eq!(section_seconds(&project).unwrap(), 8.0);
        project.sections[0].repeat = Some(3);
        assert_eq!(section_seconds(&project).unwrap(), 12.0);
    }

    #[test]
    fn arrangement_seconds_covers_sections_and_projects_without_them() {
        let mut project = oc_dsl::parse(
            "@project\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  @phrase held [1:1 - 3:1]\n    C4 w\n",
        )
        .unwrap();
        assert_eq!(arrangement_seconds(&project).unwrap(), 4.0);
        add_section(&mut project, 5);
        project.sections[0].repeat = Some(2);
        assert_eq!(arrangement_seconds(&project).unwrap(), 16.0);
    }

    /// The report: a repeat holding a tempo change plays longer than the written
    /// timeline, and the length the GUI reports is the length the renderer emits.
    #[test]
    fn the_reported_length_is_the_rendered_audio_of_a_repeat_holding_a_tempo_change() {
        let project = oc_dsl::parse(REPEAT_TEMPO).unwrap();
        oc_dsl::validate(&project).unwrap();
        const RATE: u32 = 48_000;
        let audio = oc_engine::render::render_to_buffer(&project, std::path::Path::new(""), RATE)
            .expect("the fixture renders");
        let rendered_frames = audio.len() / 2;

        let reported = section_seconds(&project).unwrap();
        // The verse repeat plays its tempo change twice and the outro point
        // lands 8 quarters later: 20 s played, 32 s on the written map.
        assert_eq!(reported, 20.0);
        assert_eq!(arrangement_seconds(&project).unwrap(), 20.0);
        assert_eq!(
            rendered_frames,
            (reported * f64::from(RATE)).ceil() as usize,
            "the render is {rendered_frames} frames"
        );
    }
}
