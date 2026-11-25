//! Prepared MIDI state and held-note indexes; processing borrows immutable tables.

use oc_audio::{MidiInputError, MidiInputMessage};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Timed {
    pub at: u64,
    pub message: MidiInputMessage,
}

struct StateValue {
    event: Timed,
    order: usize,
}

struct Note {
    start: u64,
    release: u64,
    end: u64,
    on: MidiInputMessage,
    off: MidiInputMessage,
}

// RP-015 retains bank, volume, pan, sound/effect controls and the program.
fn reset_value(message: MidiInputMessage) -> Option<MidiInputMessage> {
    use MidiInputMessage::*;
    match message {
        ProgramChange { .. } => None,
        ControlChange {
            controller: 0 | 7 | 10 | 32 | 39 | 42 | 70..=79 | 91..=95,
            ..
        } => None,
        ControlChange {
            channel,
            controller,
            ..
        } => Some(ControlChange {
            channel,
            controller,
            value: if matches!(controller, 11 | 43) {
                127
            } else {
                0
            },
        }),
        PitchBend { channel, .. } => Some(PitchBend { channel, value: 0 }),
        ChannelPressure { channel, .. } => Some(ChannelPressure {
            channel,
            pressure: 0,
        }),
        PolyPressure { channel, key, .. } => Some(PolyPressure {
            channel,
            key,
            pressure: 0,
        }),
        _ => None,
    }
}

pub(super) struct Schedule {
    pub events: Vec<Timed>,
    notes: Vec<Note>,
    end_tree: Vec<u64>,
    tree_base: usize,
    states: Vec<Vec<StateValue>>,
    resets: [Vec<(u64, usize)>; 16],
    chase_budget: usize,
}

impl Schedule {
    pub fn new(events: Vec<Timed>, block_frames: u32, capacity: usize) -> Result<Self, String> {
        const MAX_EVENTS: usize = 1_000_000;
        if capacity == 0 {
            return Err("effect has no MIDI input".into());
        }
        if events.len() > MAX_EVENTS || events.windows(2).any(|p| p[0].at > p[1].at) {
            return Err("MIDI input schedule exceeds its bound or is not ordered".into());
        }
        let mut notes: Vec<Note> = Vec::new();
        let mut open: Vec<Vec<usize>> = (0..2048).map(|_| Vec::new()).collect();
        let mut sustained: [Vec<usize>; 16] = std::array::from_fn(|_| Vec::new());
        let mut pedal = [false; 16];
        let mut states: BTreeMap<[u8; 2], Vec<StateValue>> = BTreeMap::new();
        let mut resets: [Vec<(u64, usize)>; 16] = std::array::from_fn(|_| Vec::new());
        for (order, &event) in events.iter().enumerate() {
            let bytes = event
                .message
                .bytes()
                .map_err(|_| "invalid effect MIDI message")?;
            let channel = usize::from(bytes[0] & 15);
            match event.message {
                MidiInputMessage::NoteOn {
                    channel,
                    key,
                    velocity,
                } if velocity != 0 => {
                    let index = notes.len();
                    notes.push(Note {
                        start: event.at,
                        release: u64::MAX,
                        end: u64::MAX,
                        on: event.message,
                        off: MidiInputMessage::NoteOff { channel, key },
                    });
                    open[usize::from(channel) * 128 + usize::from(key)].push(index);
                }
                MidiInputMessage::NoteOn { key, .. } | MidiInputMessage::NoteOff { key, .. } => {
                    if let Some(index) = open[channel * 128 + usize::from(key)].pop() {
                        notes[index].release = event.at;
                        if pedal[channel] {
                            sustained[channel].push(index);
                        } else {
                            notes[index].end = event.at;
                        }
                    }
                }
                MidiInputMessage::ControlChange {
                    controller, value, ..
                } => {
                    if matches!(controller, 66 | 69) {
                        return Err(
                            "MIDI input sostenuto and Hold 2 need a pedal-aware chase plan".into(),
                        );
                    }
                    if matches!(controller, 6 | 38 | 96..=101) {
                        return Err(
                            "MIDI input RPN/NRPN sequences need a parameter-aware chase plan"
                                .into(),
                        );
                    }
                    if controller == 64 || controller == 121 {
                        pedal[channel] = controller == 64 && value >= 64;
                        if !pedal[channel] {
                            for index in sustained[channel].drain(..) {
                                notes[index].end = event.at;
                            }
                        }
                    }
                    if controller == 120 || controller == 123 {
                        for voices in &mut open[channel * 128..(channel + 1) * 128] {
                            for index in voices.drain(..) {
                                notes[index].release = event.at;
                                if controller == 123 && pedal[channel] {
                                    sustained[channel].push(index);
                                } else {
                                    notes[index].end = event.at;
                                }
                            }
                        }
                        if controller == 120 {
                            for index in sustained[channel].drain(..) {
                                notes[index].end = event.at;
                            }
                        }
                    }
                    if controller == 121 {
                        resets[channel].push((event.at, order));
                    } else if controller < 120 {
                        states
                            .entry([bytes[0], controller])
                            .or_default()
                            .push(StateValue { event, order });
                    } else if !matches!(controller, 120 | 123) {
                        return Err(
                            "MIDI input channel-mode routing is not qualified for this controller"
                                .into(),
                        );
                    }
                }
                _ => {
                    states
                        .entry([bytes[0], if bytes[0] & 0xf0 == 0xa0 { bytes[1] } else { 0 }])
                        .or_default()
                        .push(StateValue { event, order });
                }
            }
        }
        for stream in states.values() {
            if stream[0].event.at != 0 && reset_value(stream[0].event.message).is_none() {
                return Err("MIDI input retained controls and programs require an explicit initial value at sample zero".into());
            }
        }
        // A prepared interval tree prunes expired notes even across long held notes.
        let mut changes = Vec::with_capacity(notes.len() * 2);
        for note in &notes {
            if note.end > note.start {
                changes.push((note.start, 1i32));
                changes.push((note.end, -1i32));
            }
        }
        changes.sort_unstable();
        let (mut active, mut maximum) = (0i32, 0usize);
        for (_, delta) in changes {
            active += delta;
            maximum = maximum.max(active as usize);
        }
        let mut left = 0;
        let mut burst = 0;
        let weights: Vec<usize> = events
            .iter()
            .map(|event| {
                if matches!(
                    event.message,
                    MidiInputMessage::ControlChange {
                        controller: 120 | 123,
                        ..
                    }
                ) {
                    maximum.saturating_add(1)
                } else {
                    1
                }
            })
            .collect();
        let mut window_weight = 0usize;
        for right in 0..events.len() {
            window_weight = window_weight.saturating_add(weights[right]);
            while left < right
                && events[right].at - events[left].at >= u64::from(block_frames.max(1))
            {
                window_weight -= weights[left];
                left += 1;
            }
            burst = burst.max(window_weight);
        }
        let chase_budget = maximum
            .saturating_mul(3)
            .saturating_add(states.len())
            .saturating_add(48);
        if chase_budget.saturating_add(burst) > capacity {
            return Err(
                "MIDI input burst and seek chase exceed the processor's prepared event capacity"
                    .into(),
            );
        }
        let tree_base = notes.len().max(1).next_power_of_two();
        let mut end_tree = vec![0; tree_base * 2];
        for (index, note) in notes.iter().enumerate() {
            end_tree[tree_base + index] = note.end;
        }
        for index in (1..tree_base).rev() {
            end_tree[index] = end_tree[index * 2].max(end_tree[index * 2 + 1]);
        }
        Ok(Self {
            events,
            notes,
            end_tree,
            tree_base,
            states: states.into_values().collect(),
            resets,
            chase_budget,
        })
    }

    /// Emit only state strictly before `at`; boundary events belong to the block.
    pub fn chase(
        &self,
        at: u64,
        mut emit: impl FnMut(MidiInputMessage) -> Result<(), MidiInputError>,
    ) -> Result<(), MidiInputError> {
        debug_assert!(self.chase_budget >= 48);
        for channel in 0..16 {
            emit(MidiInputMessage::ControlChange {
                channel,
                controller: 120,
                value: 0,
            })?;
            emit(MidiInputMessage::ControlChange {
                channel,
                controller: 64,
                value: 0,
            })?;
            emit(MidiInputMessage::ControlChange {
                channel,
                controller: 121,
                value: 0,
            })?;
        }
        let state_at = |stream: &[StateValue]| -> Option<MidiInputMessage> {
            let Some(index) = stream.partition_point(|e| e.event.at < at).checked_sub(1) else {
                return reset_value(stream[0].event.message);
            };
            let entry = &stream[index];
            let event = &entry.event;
            let bytes = event.message.bytes().ok()?;
            let reset = &self.resets[usize::from(bytes[0] & 15)];
            let latest = reset
                .partition_point(|&(time, _)| time < at)
                .checked_sub(1)
                .map(|i| reset[i]);
            if latest.is_some_and(|mark| (event.at, entry.order) < mark) {
                return reset_value(event.message).or(Some(event.message));
            }
            Some(event.message)
        };
        for stream in &self.states {
            if let Some(message) = state_at(stream)
                && !matches!(message, MidiInputMessage::PolyPressure { .. })
            {
                emit(message)?;
            }
        }
        let upper = self.notes.partition_point(|n| n.start < at);
        let mut stack = [(0usize, 0usize, 0usize); 64];
        stack[0] = (1, 0, self.tree_base);
        let mut count = 1;
        while count > 0 {
            count -= 1;
            let (node, lo, hi) = stack[count];
            if lo >= upper || self.end_tree[node] <= at {
                continue;
            }
            if hi - lo == 1 {
                let note = &self.notes[lo];
                emit(note.on)?;
                if note.release < at {
                    emit(note.off)?;
                }
            } else {
                let mid = (lo + hi) / 2;
                stack[count] = (node * 2 + 1, mid, hi);
                stack[count + 1] = (node * 2, lo, mid);
                count += 2;
            }
        }
        for stream in &self.states {
            if let Some(message) = state_at(stream)
                && matches!(message, MidiInputMessage::PolyPressure { .. })
            {
                emit(message)?;
            }
        }
        Ok(())
    }
}
