//! Block snapshots of the MIDI controllers consumed by mixer effects.

use crate::control_link::{CC_UNSET, MidiControlInput, cc_index};
use oc_dsl::ast::{ControlLink, ControlLinkSource, Project};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

pub(super) struct CcReading(AtomicU8);

impl CcReading {
    pub(super) fn value(&self) -> Option<f64> {
        let raw = self.0.load(Ordering::Relaxed);
        (raw != CC_UNSET).then_some(f64::from(raw))
    }
}

pub(super) struct LiveControls {
    input: Arc<MidiControlInput>,
    snapshots: HashMap<usize, Arc<CcReading>>,
}

impl LiveControls {
    pub(super) fn new(input: Arc<MidiControlInput>, project: &Project) -> Result<Self, String> {
        Self::prepare(input, project, None)
    }

    pub(super) fn with_project(&self, project: &Project) -> Result<Self, String> {
        Self::prepare(Arc::clone(&self.input), project, Some(self))
    }

    fn prepare(
        input: Arc<MidiControlInput>,
        project: &Project,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        let mut snapshots = HashMap::new();
        for link in mixer_cc_links(project) {
            let index = index_of(&link.source)
                .ok_or_else(|| format!("control link '{}': invalid MIDI CC source", link.name))?;
            snapshots.entry(index).or_insert_with(|| {
                previous
                    .and_then(|controls| controls.snapshots.get(&index))
                    .cloned()
                    .unwrap_or_else(|| Arc::new(CcReading(AtomicU8::new(input.read(index)))))
            });
        }
        Ok(Self { input, snapshots })
    }

    pub(super) fn snapshot(&self, source: &ControlLinkSource) -> Option<&Arc<CcReading>> {
        self.snapshots.get(&index_of(source)?)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Capture once so every consumer of a CC sees the same block value.
    pub(super) fn latch(&self) {
        for (index, value) in &self.snapshots {
            value.0.store(self.input.read(*index), Ordering::Relaxed);
        }
    }
}

pub(super) fn mixer_cc_links(project: &Project) -> impl Iterator<Item = &ControlLink> {
    project.properties.control_links.iter().filter(|link| {
        link.source.is_midi()
            && link
                .targets
                .iter()
                .any(|target| target.is_mixer_effect_target(&link.source))
    })
}

pub(super) fn changed_cc_owners<'a>(previous: &'a Project, next: &'a Project) -> HashSet<&'a str> {
    fn by_name(project: &Project) -> HashMap<&str, &ControlLink> {
        mixer_cc_links(project)
            .map(|link| (link.name.as_str(), link))
            .collect()
    }
    let previous = by_name(previous);
    let next = by_name(next);
    let mut owners = HashSet::new();
    for (left, right) in [(&previous, &next), (&next, &previous)] {
        for (name, link) in left {
            if right.get(name) == Some(link) {
                continue;
            }
            owners.extend(link.targets.iter().filter_map(|target| {
                target
                    .is_mixer_effect_target(&link.source)
                    .then_some(target.track.as_str())
            }));
        }
    }
    owners
}

fn index_of(source: &ControlLinkSource) -> Option<usize> {
    let ControlLinkSource::MidiCc {
        channel,
        controller,
    } = *source
    else {
        return None;
    };
    cc_index(channel.checked_sub(1)?, controller)
}
