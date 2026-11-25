//! Admit saved rack controls without reopening the playing sources or inserts.

use oc_dsl::ast::{ControlLinkSource, Project};
use std::path::Path;

use super::RenderState;

#[derive(Clone)]
pub struct RackMacroEdit {
    preset: String,
    index: u8,
    parameter: String,
    value: f64,
}

impl RackMacroEdit {
    pub(crate) fn from_saved(project: &Project, name: &str, key: &str) -> Option<Self> {
        let index = key.strip_prefix("macro.")?.parse::<u8>().ok()?;
        if !(1..=8).contains(&index)
            || key != format!("macro.{index}")
            || !is_rack_only(project, name, index)
        {
            return None;
        }
        let preset = project.presets.iter().find(|preset| preset.name == name)?;
        let value = preset
            .params
            .iter()
            .rev()
            .find(|(k, _)| k == key)?
            .1
            .as_f64()?;
        (0.0..=1.0).contains(&value).then(|| Self {
            preset: name.to_owned(),
            index,
            parameter: key.to_owned(),
            value,
        })
    }
}

fn is_rack_only(project: &Project, name: &str, index: u8) -> bool {
    project.properties.control_links.iter().any(|link| {
        matches!(&link.source, ControlLinkSource::PresetMacro { preset, index: slot }
            if preset == name && *slot == index)
    }) && !project
        .presets
        .iter()
        .any(|preset| preset.extends.as_deref() == Some(name))
        && !project.tracks.iter().any(|track| {
            track.preset.as_deref() == Some(name)
                || track
                    .inline_patterns
                    .iter()
                    .any(|pattern| pattern.preset.as_deref() == Some(name))
        })
        && !project
            .patterns
            .iter()
            .any(|pattern| pattern.preset.as_deref() == Some(name))
}

impl RenderState {
    pub(super) fn try_commit_rack_macro(
        &mut self,
        next: &mut Project,
        soundfont: &Path,
        root: Option<&Path>,
        edit: &RackMacroEdit,
    ) -> bool {
        let (Some(previous), Some(graph)) = (self.project.as_ref(), self.graph.as_mut()) else {
            return false;
        };
        if self.processing_fault
            || self.root.as_deref() != root
            || self.soundfont_path != soundfont
            || !is_only_macro_edit(previous, next, edit)
        {
            return false;
        }
        graph.commit_saved_preset_macro(&edit.preset, edit.index, edit.value)
    }
}

fn is_only_macro_edit(previous: &Project, next: &mut Project, edit: &RackMacroEdit) -> bool {
    let Some(index) = next
        .presets
        .iter()
        .position(|preset| preset.name == edit.preset)
    else {
        return false;
    };
    let Some(old) = previous
        .presets
        .get(index)
        .filter(|preset| preset.name == edit.preset)
    else {
        return false;
    };
    let candidate = &next.presets[index];
    if !is_rack_only(next, &edit.preset, edit.index)
        || candidate
            .params
            .iter()
            .rev()
            .find(|(key, _)| key == &edit.parameter)
            .and_then(|(_, value)| value.as_f64())
            != Some(edit.value)
    {
        return false;
    }
    let stripped = |preset: &oc_dsl::ast::PresetDef| {
        let mut copy = preset.clone();
        copy.params.retain(|(key, _)| key != &edit.parameter);
        copy
    };
    if stripped(old) != stripped(candidate) {
        return false;
    }
    // Compare the rest of the queued document, including any unacknowledged
    // source or timeline edit. Swap only one preset to avoid copying the score.
    let replacement = std::mem::replace(&mut next.presets[index], old.clone());
    let same = next == previous;
    next.presets[index] = replacement;
    same
}

#[cfg(test)]
mod tests {
    use super::super::{RenderCommand, RenderThreadHandle};
    use super::*;
    use oc_dsl::ast::ParamValue;
    use oc_engine::graph::PresetMacroInput;
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::Ordering, mpsc},
        time::Duration,
    };

    const SCORE: &str = r#"@project
  tempo: 120
@preset controls
  source: builtin
  plugin: "builtin.open_synth"
  params: {macro.8: 1}
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
@track lead | preset: tone, instrument: synth/lead
  effects:
    - gain: factor=1
    - delay: time_l=75ms, time_r=75ms, feedback=50%, mix=100%
  @phrase p [1:1 - 2:1]
    C4 w
@control_link amount
  from: macro(8, controls)
  -> track: lead param: fx1.factor formula: linear
"#;

    fn change(project: &Project) -> (Project, RackMacroEdit) {
        let mut saved = project.clone();
        saved.presets[0].params[0].1 = ParamValue::Number(0.25);
        let edit = RackMacroEdit::from_saved(&saved, "controls", "macro.8").unwrap();
        (saved, edit)
    }

    #[test]
    fn rack_macro_commit_preserves_the_running_delay_history() {
        let project = oc_dsl::parse(SCORE).unwrap();
        let setup = || {
            let input = Arc::new(PresetMacroInput::default());
            let mut state = RenderState::new(8_000);
            state.preset_macros = Some(Arc::clone(&input));
            state.load_project(&project, PathBuf::new()).unwrap();
            let first = state.render_chunk(640);
            assert!(first.iter().any(|value| value.abs() > 0.05));
            let token = input.begin("controls", 8).unwrap().unwrap();
            assert!(input.update("controls", 8, &token, 0.25));
            (state, input, token)
        };
        let (mut reference, _, _) = setup();
        let (mut saved, input, token) = setup();
        let (mut next, edit) = change(&project);
        assert!(saved.try_commit_rack_macro(&mut next, Path::new(""), None, &edit));
        assert!(input.end("controls", 8, &token));
        let before = saved.render_cursor;
        let expected = reference.render_chunk(512);
        assert!(expected.iter().any(|value| value.abs() > 0.05));
        assert_eq!(saved.render_chunk(512), expected);
        assert_eq!(saved.render_cursor, before + 512);

        next.properties.tempo = 128.0;
        assert!(!saved.try_commit_rack_macro(&mut next, Path::new(""), None, &edit));
        next.tracks[0].preset = Some("controls".into());
        assert!(RackMacroEdit::from_saved(&next, "controls", "macro.8").is_none());
    }

    #[test]
    fn rack_macro_commit_and_preview_release_share_the_worker_queue() {
        let project = oc_dsl::parse(SCORE).unwrap();
        let input = Arc::new(PresetMacroInput::default());
        let thread = RenderThreadHandle::spawn_with_controls(8_000, None, Some(Arc::clone(&input)));
        let budget = Duration::from_secs(2);
        thread.send(RenderCommand::ProjectChanged {
            project: Box::new(project.clone()),
            root: None,
            soundfont_path: PathBuf::new(),
            warm: false,
        });
        thread.wait_project_loaded(budget).unwrap();
        thread.send(RenderCommand::Play {
            start_frame: 0,
            resume: false,
            end_frame: None,
        });
        assert!(thread.wait_ready(budget));
        let loads = thread.playback.project_loads.load(Ordering::Acquire);
        let opened = thread
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire);
        let token = input.begin("controls", 8).unwrap().unwrap();
        assert!(input.update("controls", 8, &token, 0.25));
        let (saved, edit) = change(&project);
        thread.send(RenderCommand::SavedRackMacro {
            project: Box::new(saved),
            root: None,
            soundfont_path: PathBuf::new(),
            warm: false,
            edit,
        });
        thread.send(RenderCommand::EndPresetMacroPreview {
            preset: "controls".into(),
            index: 8,
            token: token.clone(),
        });
        let (entered, enter) = mpsc::channel();
        let (exit, release) = mpsc::channel();
        thread.send(RenderCommand::TestBarrier { entered, release });
        enter.recv_timeout(budget).unwrap();
        let loaded = thread.wait_project_loaded(budget);
        let end_loads = thread.playback.project_loads.load(Ordering::Acquire);
        let end_opened = thread
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire);
        exit.send(()).unwrap();
        loaded.unwrap();
        assert_eq!(loads, end_loads);
        assert_eq!(opened, end_opened);
        assert!(!input.update("controls", 8, &token, 0.1));
    }
}
