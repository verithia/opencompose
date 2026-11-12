//! A single input/output session plus thru routing.

use super::backend::{InputId, MidiBackend, MidiInputEvent, OutputId, PortDirection, find_port};
use super::error::MidiError;
use super::message::{LiveEvent, encode_engine_message};
use crate::interpret::MidiMessage;

/// Owns one input and one output on a backend and forwards thru when enabled.
pub struct MidiSession<B: MidiBackend> {
    backend: B,
    input: Option<(InputId, String)>,
    output: Option<(OutputId, String)>,
    thru: bool,
}

impl<B: MidiBackend> MidiSession<B> {
    /// Open selected input and output ports on a backend.
    pub fn open(
        backend: B,
        input_selector: Option<&str>,
        output_selector: Option<&str>,
    ) -> Result<Self, MidiError> {
        let mut backend = backend;
        let input = match input_selector {
            Some(selector) => {
                let ports = backend.list_inputs()?;
                let port = find_port(&ports, selector, PortDirection::Input)?.clone();
                Some((backend.open_input(&port)?, port.name))
            }
            None => None,
        };
        let output = match output_selector {
            Some(selector) => {
                let ports = backend.list_outputs()?;
                let port = find_port(&ports, selector, PortDirection::Output)?.clone();
                Some((backend.open_output(&port)?, port.name))
            }
            None => None,
        };
        Ok(Self {
            backend,
            input,
            output,
            thru: false,
        })
    }

    /// The MIDI backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Mutable backend access.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Selected input name.
    pub fn input_name(&self) -> Option<&str> {
        self.input.as_ref().map(|(_, name)| name.as_str())
    }

    /// Selected output name.
    pub fn output_name(&self) -> Option<&str> {
        self.output.as_ref().map(|(_, name)| name.as_str())
    }

    /// Enable or disable MIDI thru.
    pub fn set_thru(&mut self, thru: bool) {
        self.thru = thru;
    }

    /// Whether thru is enabled.
    pub fn thru(&self) -> bool {
        self.thru
    }

    /// Poll live input events into `out`.
    pub fn poll(&mut self, out: &mut [MidiInputEvent]) -> Result<usize, MidiError> {
        let Some((input, _)) = self.input else {
            return Ok(0);
        };
        self.backend.poll(input, out)
    }

    /// Forward already-polled input events to the output when thru is enabled.
    pub fn forward(&mut self, events: &[MidiInputEvent]) -> Result<(), MidiError> {
        if !self.thru {
            return Ok(());
        }
        let Some((output, _)) = self.output else {
            return Ok(());
        };
        for event in events {
            self.backend.send_live(output, event.event)?;
        }
        Ok(())
    }

    /// Send one live event to the output.
    pub fn send_live(&mut self, event: LiveEvent) -> Result<(), MidiError> {
        let Some((output, _)) = self.output else {
            return Ok(());
        };
        self.backend.send_live(output, event)
    }

    /// Send every wire form of an engine message (0, 1, or 2 events).
    pub fn send_engine_message(&mut self, message: &MidiMessage) -> Result<(), MidiError> {
        let mut encoded = [LiveEvent::default(); 2];
        let count = encode_engine_message(message, &mut encoded);
        for event in &encoded[..count] {
            self.send_live(*event)?;
        }
        Ok(())
    }

    /// Send a MIDI clock tick.
    pub fn send_clock(&mut self) -> Result<(), MidiError> {
        self.send_live(LiveEvent::Clock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_io::fake::FakeBackend;

    #[test]
    fn missing_device_reports_the_port_name() {
        let backend = FakeBackend::new(&["keys"], &["synth"]);
        let Err(error) = MidiSession::open(backend, Some("missing"), None) else {
            panic!("missing port must fail");
        };
        assert_eq!(
            error,
            MidiError::PortNotFound {
                direction: PortDirection::Input,
                name: "missing".into()
            }
        );
    }

    #[test]
    fn poll_then_thru_forwards_wire_bytes() {
        let mut backend = FakeBackend::new(&["keys"], &["synth"]);
        backend.queue_input(
            "keys",
            LiveEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        );
        let mut session = MidiSession::open(backend, Some("keys"), Some("synth")).unwrap();
        session.set_thru(true);
        let mut buffer = [MidiInputEvent::default(); 8];
        let count = session.poll(&mut buffer).unwrap();
        assert_eq!(count, 1);
        session.forward(&buffer[..count]).unwrap();
        let sent = session
            .backend()
            .sent(session.backend().open_output_id("synth").unwrap())
            .unwrap();
        assert_eq!(sent, &[vec![0x90, 60, 100]]);
    }

    #[test]
    fn engine_messages_are_sent_for_output() {
        let backend = FakeBackend::new(&[], &["synth"]);
        let mut session = MidiSession::open(backend, None, Some("synth")).unwrap();
        session
            .send_engine_message(&MidiMessage::NoteOn {
                channel: 2,
                note: 65,
                velocity: 80,
            })
            .unwrap();
        session.send_clock().unwrap();
        let sent = session
            .backend()
            .sent(session.backend().open_output_id("synth").unwrap())
            .unwrap();
        assert_eq!(sent, &[vec![0x92, 65, 80], vec![0xF8]]);
    }

    #[test]
    fn send_failures_surface_as_typed_errors() {
        let mut backend = FakeBackend::new(&[], &["synth"]);
        backend.queue_output_failure(
            "synth",
            MidiError::Disconnected {
                direction: PortDirection::Output,
                name: "synth".into(),
            },
        );
        let mut session = MidiSession::open(backend, None, Some("synth")).unwrap();
        let error = session.send_clock().unwrap_err();
        assert!(matches!(error, MidiError::Disconnected { .. }));
    }
}
