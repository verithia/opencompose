//! Deterministic in-memory backend used by tests and `--backend fake` tooling.

use std::collections::{HashMap, VecDeque};

use super::backend::{InputId, MidiBackend, MidiInputEvent, MidiPortInfo, OutputId, PortDirection};
use super::error::MidiError;
use super::message::LiveEvent;

#[derive(Debug, Default)]
struct FakeInput {
    name: String,
    queue: VecDeque<MidiInputEvent>,
    error: Option<MidiError>,
    terminal_error: Option<MidiError>,
}

#[derive(Debug, Default)]
struct FakeOutput {
    name: String,
    sent: Vec<Vec<u8>>,
    error: Option<MidiError>,
}

/// Backend with scripted ports and no platform dependency.
#[derive(Debug, Default)]
pub struct FakeBackend {
    inputs: Vec<MidiPortInfo>,
    outputs: Vec<MidiPortInfo>,
    open_inputs: HashMap<InputId, FakeInput>,
    open_outputs: HashMap<OutputId, FakeOutput>,
    next_id: u32,
    next_open_error: Option<MidiError>,
    pending_inputs: HashMap<String, VecDeque<MidiInputEvent>>,
    pending_input_errors: HashMap<String, MidiError>,
    pending_output_errors: HashMap<String, MidiError>,
}

impl FakeBackend {
    /// Create a fake backend with named ports.
    pub fn new(inputs: &[&str], outputs: &[&str]) -> Self {
        let inputs = inputs
            .iter()
            .enumerate()
            .map(|(index, name)| {
                MidiPortInfo::new(PortDirection::Input, index, *name, format!("in-{index}"))
            })
            .collect();
        let outputs = outputs
            .iter()
            .enumerate()
            .map(|(index, name)| {
                MidiPortInfo::new(PortDirection::Output, index, *name, format!("out-{index}"))
            })
            .collect();
        Self {
            inputs,
            outputs,
            ..Self::default()
        }
    }

    /// Make the next open call fail with a caller-supplied typed error.
    pub fn fail_next_open(&mut self, error: MidiError) {
        self.next_open_error = Some(error);
    }

    /// Queue an input event before the port is opened.
    pub fn queue_input(&mut self, port_name: &str, event: LiveEvent) {
        self.pending_inputs
            .entry(port_name.to_string())
            .or_default()
            .push_back(MidiInputEvent {
                event,
                timestamp_micros: 0,
            });
    }

    /// Fail after returning the named input's queued prefix.
    pub fn queue_input_failure(&mut self, port_name: &str, error: MidiError) {
        self.pending_input_errors
            .insert(port_name.to_string(), error);
    }

    /// Make the named output fail on its first send after opening.
    pub fn queue_output_failure(&mut self, port_name: &str, error: MidiError) {
        self.pending_output_errors
            .insert(port_name.to_string(), error);
    }

    /// Make sends on an output fail.
    pub fn fail_send(&mut self, output: OutputId, error: MidiError) {
        if let Some(port) = self.open_outputs.get_mut(&output) {
            port.error = Some(error);
        }
    }

    /// Make polling an input fail.
    pub fn fail_poll(&mut self, input: InputId, error: MidiError) {
        if let Some(port) = self.open_inputs.get_mut(&input) {
            port.error = Some(error);
        }
    }

    /// Queue a live event on an input.
    pub fn push_input(&mut self, input: InputId, event: LiveEvent, timestamp_micros: u64) {
        if let Some(port) = self.open_inputs.get_mut(&input) {
            port.queue.push_back(MidiInputEvent {
                event,
                timestamp_micros,
            });
        }
    }

    /// Bytes sent to an output.
    pub fn sent(&self, output: OutputId) -> Option<&[Vec<u8>]> {
        self.open_outputs
            .get(&output)
            .map(|port| port.sent.as_slice())
    }

    /// Open input id by port name.
    pub fn open_input_id(&self, name: &str) -> Option<InputId> {
        self.open_inputs
            .iter()
            .find(|(_, port)| port.name == name)
            .map(|(id, _)| *id)
    }

    /// Open output id by port name.
    pub fn open_output_id(&self, name: &str) -> Option<OutputId> {
        self.open_outputs
            .iter()
            .find(|(_, port)| port.name == name)
            .map(|(id, _)| *id)
    }

    fn allocate(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

impl MidiBackend for FakeBackend {
    fn list_inputs(&self) -> Result<Vec<MidiPortInfo>, MidiError> {
        Ok(self.inputs.clone())
    }

    fn list_outputs(&self) -> Result<Vec<MidiPortInfo>, MidiError> {
        Ok(self.outputs.clone())
    }

    fn open_input(&mut self, port: &MidiPortInfo) -> Result<InputId, MidiError> {
        if let Some(error) = self.next_open_error.take() {
            return Err(error);
        }
        let info = self
            .inputs
            .iter()
            .find(|candidate| candidate.id == port.id)
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Input,
                name: port.name.clone(),
            })?;
        if self.open_inputs.values().any(|open| open.name == info.name) {
            return Err(MidiError::AlreadyOpen {
                direction: PortDirection::Input,
                name: info.name.clone(),
            });
        }
        let input_name = info.name.clone();
        let queue = self.pending_inputs.remove(&input_name).unwrap_or_default();
        let terminal_error = self.pending_input_errors.remove(&input_name);
        let id = InputId(self.allocate());
        self.open_inputs.insert(
            id,
            FakeInput {
                name: input_name,
                queue,
                error: None,
                terminal_error,
            },
        );
        Ok(id)
    }

    fn open_output(&mut self, port: &MidiPortInfo) -> Result<OutputId, MidiError> {
        if let Some(error) = self.next_open_error.take() {
            return Err(error);
        }
        let info = self
            .outputs
            .iter()
            .find(|candidate| candidate.id == port.id)
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Output,
                name: port.name.clone(),
            })?;
        if self
            .open_outputs
            .values()
            .any(|open| open.name == info.name)
        {
            return Err(MidiError::AlreadyOpen {
                direction: PortDirection::Output,
                name: info.name.clone(),
            });
        }
        let output_name = info.name.clone();
        let error = self.pending_output_errors.remove(&output_name);
        let id = OutputId(self.allocate());
        self.open_outputs.insert(
            id,
            FakeOutput {
                name: output_name,
                error,
                ..FakeOutput::default()
            },
        );
        Ok(id)
    }

    fn close_input(&mut self, input: InputId) {
        self.open_inputs.remove(&input);
    }

    fn close_output(&mut self, output: OutputId) {
        self.open_outputs.remove(&output);
    }

    fn send(&mut self, output: OutputId, message: &[u8]) -> Result<(), MidiError> {
        let port = self
            .open_outputs
            .get_mut(&output)
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Output,
                name: format!("{output:?}"),
            })?;
        if let Some(error) = port.error.take() {
            return Err(error);
        }
        port.sent.push(message.to_vec());
        Ok(())
    }

    fn poll(&mut self, input: InputId, out: &mut [MidiInputEvent]) -> Result<usize, MidiError> {
        let port = self
            .open_inputs
            .get_mut(&input)
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Input,
                name: format!("{input:?}"),
            })?;
        if let Some(error) = port.error.take() {
            return Err(error);
        }
        if port.queue.is_empty()
            && !out.is_empty()
            && let Some(error) = &port.terminal_error
        {
            return Err(error.clone());
        }
        let mut written = 0;
        while written < out.len() {
            let Some(event) = port.queue.pop_front() else {
                break;
            };
            out[written] = event;
            written += 1;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_io::backend::find_port;

    #[test]
    fn fake_backend_is_usable_as_a_trait_object() {
        let mut backend: Box<dyn MidiBackend> = Box::new(FakeBackend::new(&["keys"], &["synth"]));
        let inputs = backend.list_inputs().unwrap();
        let input_port = find_port(&inputs, "keys", PortDirection::Input)
            .unwrap()
            .clone();
        let input = backend.open_input(&input_port).unwrap();
        let outputs = backend.list_outputs().unwrap();
        let output_port = find_port(&outputs, "synth", PortDirection::Output)
            .unwrap()
            .clone();
        let output = backend.open_output(&output_port).unwrap();
        backend
            .send_live(
                output,
                LiveEvent::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            )
            .unwrap();
        let mut buffer = [MidiInputEvent::default(); 4];
        assert_eq!(backend.poll(input, &mut buffer).unwrap(), 0);
    }

    #[test]
    fn open_errors_are_typed() {
        let mut backend = FakeBackend::new(&[], &[]);
        backend.fail_next_open(MidiError::PermissionDenied {
            direction: PortDirection::Output,
            name: "synth".into(),
        });
        let port = MidiPortInfo::new(PortDirection::Output, 0, "synth", "out-0");
        assert!(matches!(
            backend.open_output(&port),
            Err(MidiError::PermissionDenied { .. })
        ));
    }
}
