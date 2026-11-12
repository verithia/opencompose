//! Backend contract for platform MIDI I/O.

use super::error::MidiError;
use super::message::LiveEvent;

/// Direction of a MIDI port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortDirection {
    Input,
    Output,
}

/// A port exposed by a backend. `id` is backend-defined and stable while the device exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortInfo {
    pub direction: PortDirection,
    pub index: usize,
    pub name: String,
    pub id: String,
}

impl MidiPortInfo {
    /// Describe one MIDI port.
    pub fn new(
        direction: PortDirection,
        index: usize,
        name: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            direction,
            index,
            name: name.into(),
            id: id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Opaque opened-input id.
pub struct InputId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Opaque opened-output id.
pub struct OutputId(pub u32);

/// A received event plus the backend timestamp (microseconds since the connection opened).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiInputEvent {
    pub event: LiveEvent,
    pub timestamp_micros: u64,
}

/// One open MIDI endpoint per backend. `poll` never allocates.
pub trait MidiBackend: Send {
    fn list_inputs(&self) -> Result<Vec<MidiPortInfo>, MidiError>;
    fn list_outputs(&self) -> Result<Vec<MidiPortInfo>, MidiError>;
    fn open_input(&mut self, port: &MidiPortInfo) -> Result<InputId, MidiError>;
    fn open_output(&mut self, port: &MidiPortInfo) -> Result<OutputId, MidiError>;
    fn close_input(&mut self, input: InputId);
    fn close_output(&mut self, output: OutputId);
    fn send(&mut self, output: OutputId, message: &[u8]) -> Result<(), MidiError>;
    /// Drain pending input events into `out`; returns how many were written.
    fn poll(&mut self, input: InputId, out: &mut [MidiInputEvent]) -> Result<usize, MidiError>;

    fn send_live(&mut self, output: OutputId, event: LiveEvent) -> Result<(), MidiError> {
        let mut bytes = [0u8; 3];
        let len = event.pack_into(&mut bytes);
        self.send(output, &bytes[..len])
    }
}

/// Resolve a CLI/UI selector by exact name, opaque id, or zero-based index.
pub fn find_port<'a>(
    ports: &'a [MidiPortInfo],
    selector: &str,
    direction: PortDirection,
) -> Result<&'a MidiPortInfo, MidiError> {
    if let Some(port) = ports.iter().find(|port| port.name == selector) {
        return Ok(port);
    }
    if let Some(port) = ports.iter().find(|port| port.id == selector) {
        return Ok(port);
    }
    if let Ok(index) = selector.parse::<usize>()
        && let Some(port) = ports.iter().find(|port| port.index == index)
    {
        return Ok(port);
    }
    Err(MidiError::PortNotFound {
        direction,
        name: selector.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_matches_name_id_and_index() {
        let ports = vec![
            MidiPortInfo::new(PortDirection::Input, 0, "Keys", "in-0"),
            MidiPortInfo::new(PortDirection::Input, 1, "Pads", "in-1"),
        ];
        assert_eq!(
            find_port(&ports, "Keys", PortDirection::Input)
                .unwrap()
                .name,
            "Keys"
        );
        assert_eq!(
            find_port(&ports, "in-1", PortDirection::Input)
                .unwrap()
                .name,
            "Pads"
        );
        assert_eq!(
            find_port(&ports, "0", PortDirection::Input).unwrap().name,
            "Keys"
        );
        assert!(matches!(
            find_port(&ports, "missing", PortDirection::Input),
            Err(MidiError::PortNotFound { .. })
        ));
    }
}
