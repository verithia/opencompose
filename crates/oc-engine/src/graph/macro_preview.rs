//! Owned preview values supplied by the editable preset controls.

use crate::touch::{TouchError, TouchToken, new_owned_token};
use oc_dsl::ast::{ControlLinkSource, Project};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

const UNSET: u64 = f64::NAN.to_bits();
const MACRO_COUNT: usize = 8;

/// One normalized hand value, independent of the authored preset.
pub(super) struct MacroReading(AtomicU64);

impl MacroReading {
    pub(super) fn empty() -> Self {
        Self(AtomicU64::new(UNSET))
    }

    pub(super) fn read(&self) -> Option<f64> {
        let bits = self.0.load(Ordering::Relaxed);
        (bits != UNSET).then(|| f64::from_bits(bits))
    }

    pub(super) fn set(&self, value: Option<f64>) {
        self.0
            .store(value.map_or(UNSET, f64::to_bits), Ordering::Relaxed);
    }
}

struct PreviewSlot {
    hand: Arc<MacroReading>,
    owner: Option<u64>,
}

type PresetSlots = [Option<PreviewSlot>; MACRO_COUNT];

/// A desktop document's transient macro values. The control and preparation
/// planes own the registry. Render blocks only read pre-bound atomic handles.
#[derive(Default)]
pub struct PresetMacroInput {
    slots: Mutex<HashMap<String, PresetSlots>>,
}

impl PresetMacroInput {
    pub(super) fn bind(&self, preset: &str, index: u8) -> Option<Arc<MacroReading>> {
        let position = position(index)?;
        let mut presets = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let slots = presets.entry(preset.to_string()).or_default();
        Some(Arc::clone(
            &slots[position]
                .get_or_insert_with(|| PreviewSlot {
                    hand: Arc::new(MacroReading::empty()),
                    owner: None,
                })
                .hand,
        ))
    }

    pub(super) fn prune(&self) {
        self.slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|_, slots| {
                for item in slots.iter_mut() {
                    if item.as_ref().is_some_and(|slot| {
                        slot.owner.is_none() && Arc::strong_count(&slot.hand) == 1
                    }) {
                        *item = None;
                    }
                }
                slots.iter().any(Option::is_some)
            });
    }

    /// Begin a preview for a macro already bound by a live graph.
    pub fn begin(&self, preset: &str, index: u8) -> Result<Option<TouchToken>, TouchError> {
        let mut presets = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let Some(slot) = slot(&mut presets, preset, index) else {
            return Ok(None);
        };
        let (identity, token) = new_owned_token()?;
        slot.owner = Some(identity);
        slot.hand.set(None);
        Ok(Some(token))
    }

    /// Publish the latest hand value when the gesture still owns the slot.
    pub fn update(&self, preset: &str, index: u8, token: &TouchToken, value: f64) -> bool {
        if !(0.0..=1.0).contains(&value) {
            return false;
        }
        let mut presets = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let Some(slot) =
            slot(&mut presets, preset, index).filter(|entry| token.matches(entry.owner))
        else {
            return false;
        };
        slot.hand.set(Some(value));
        true
    }

    /// Return a macro to its authored value if this is still its owner.
    pub fn end(&self, preset: &str, index: u8, token: &TouchToken) -> bool {
        let mut presets = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let Some(slot) =
            slot(&mut presets, preset, index).filter(|entry| token.matches(entry.owner))
        else {
            return false;
        };
        slot.owner = None;
        slot.hand.set(None);
        true
    }

    /// Retire stale previews on transport or document replacement.
    pub fn clear(&self) {
        for presets in self
            .slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values_mut()
        {
            for slot in presets.iter_mut().flatten() {
                slot.owner = None;
                slot.hand.set(None);
            }
        }
    }
}

fn position(index: u8) -> Option<usize> {
    index
        .checked_sub(1)
        .map(usize::from)
        .filter(|i| *i < MACRO_COUNT)
}

fn slot<'a>(
    presets: &'a mut HashMap<String, PresetSlots>,
    preset: &str,
    index: u8,
) -> Option<&'a mut PreviewSlot> {
    presets.get_mut(preset)?[position(index)?].as_mut()
}

#[derive(Clone)]
struct Snapshot {
    hand: Arc<MacroReading>,
    latched: Arc<MacroReading>,
    saved_edit: Option<f64>,
}

pub(super) struct MacroSnapshots {
    input: Arc<PresetMacroInput>,
    values: HashMap<String, [Option<Snapshot>; MACRO_COUNT]>,
}

impl MacroSnapshots {
    pub(super) fn new(input: Arc<PresetMacroInput>, project: &Project) -> Self {
        Self::prepare(input, project, None)
    }

    pub(super) fn with_project(&self, project: &Project) -> Self {
        Self::prepare(Arc::clone(&self.input), project, Some(self))
    }

    fn prepare(input: Arc<PresetMacroInput>, project: &Project, previous: Option<&Self>) -> Self {
        input.prune();
        let mut values = HashMap::<String, [Option<Snapshot>; MACRO_COUNT]>::new();
        for link in &project.properties.control_links {
            let ControlLinkSource::PresetMacro { preset, index } = &link.source else {
                continue;
            };
            let Some(position) = position(*index) else {
                continue;
            };
            let value = &mut values.entry(preset.clone()).or_default()[position];
            if value.is_none() {
                *value = previous
                    .and_then(|old| old.values.get(preset)?[position].clone())
                    .or_else(|| {
                        Some(Snapshot {
                            hand: input.bind(preset, *index)?,
                            latched: Arc::new(MacroReading::empty()),
                            saved_edit: None,
                        })
                    });
            }
        }
        Self { input, values }
    }

    pub(super) fn reading(&self, preset: &str, index: u8) -> Option<Arc<MacroReading>> {
        self.values.get(preset)?[position(index)?]
            .as_ref()
            .map(|slot| Arc::clone(&slot.latched))
    }

    pub(super) fn latch(&self) {
        for slot in self
            .values
            .values()
            .flat_map(|slots| slots.iter().flatten())
        {
            slot.latched.set(slot.hand.read().or(slot.saved_edit));
        }
    }

    pub(super) fn commit_saved(&mut self, preset: &str, index: u8, value: f64) -> bool {
        if !(0.0..=1.0).contains(&value) {
            return false;
        }
        let Some(slot) =
            position(index).and_then(|position| self.values.get_mut(preset)?[position].as_mut())
        else {
            return false;
        };
        slot.saved_edit = Some(value);
        true
    }

    pub(super) fn clear_recompiled_values(&mut self, previous: &Project, next: &Project) {
        if previous.presets == next.presets {
            return;
        }
        for (preset, slots) in &mut self.values {
            if slots.iter().flatten().any(|slot| slot.saved_edit.is_some())
                && oc_dsl::preset::resolve_preset(&previous.presets, preset)
                    != oc_dsl::preset::resolve_preset(&next.presets, preset)
            {
                for slot in slots.iter_mut().flatten() {
                    slot.saved_edit = None;
                }
            }
        }
    }
}
