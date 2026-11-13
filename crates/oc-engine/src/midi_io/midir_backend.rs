//! `midir` backend: CoreMIDI on macOS, WinMM on Windows, ALSA on Linux.
//!
//! Input callbacks only copy bytes into a preallocated ring; parsing and
//! delivery happen on the MIDI thread in `poll`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use midir::{
    ConnectErrorKind, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection, SendError,
};

use super::backend::{InputId, MidiBackend, MidiInputEvent, MidiPortInfo, OutputId, PortDirection};
use super::error::MidiError;
use super::message::{LiveEvent, RawMidi};

const CLIENT_NAME: &str = "OpenCompose";
const INPUT_QUEUE_CAPACITY: usize = 1024;

#[derive(Clone, Copy)]
struct QueuedRaw {
    raw: RawMidi,
    timestamp_micros: u64,
}

struct InputQueue {
    queue: rtrb::Consumer<QueuedRaw>,
    overflow: Arc<AtomicBool>,
}

impl InputQueue {
    fn new(capacity: usize) -> (impl FnMut(u64, &[u8]) + Send, Self) {
        let (mut producer, queue) = rtrb::RingBuffer::new(capacity);
        let overflow = Arc::new(AtomicBool::new(false));
        let lost = Arc::clone(&overflow);
        let push = move |timestamp_micros, message: &[u8]| {
            // A gap can lose a release. End this connection's event stream
            // here, so the consumer can safely finish the intact prefix.
            if !lost.load(Ordering::Relaxed)
                && let Some(raw) = RawMidi::new(message)
                && producer
                    .push(QueuedRaw {
                        raw,
                        timestamp_micros,
                    })
                    .is_err()
            {
                lost.store(true, Ordering::Release);
            }
        };
        (push, Self { queue, overflow })
    }

    fn poll(&mut self, name: &str, out: &mut [MidiInputEvent]) -> Result<usize, MidiError> {
        // Read the latch before draining. If it is set, the producer has
        // published every packet in the prefix and will never add another.
        let overflowed = self.overflow.load(Ordering::Acquire);
        let mut written = 0;
        while written < out.len() {
            let Ok(queued) = self.queue.pop() else {
                break;
            };
            let Ok(event) = LiveEvent::parse(queued.raw.as_slice()) else {
                continue;
            };
            out[written] = MidiInputEvent {
                event,
                timestamp_micros: queued.timestamp_micros,
            };
            written += 1;
        }
        if overflowed && written == 0 && !out.is_empty() {
            return Err(MidiError::InputOverflow {
                name: name.to_owned(),
            });
        }
        Ok(written)
    }
}

struct OpenInput {
    name: String,
    _connection: MidiInputConnection<()>,
    queue: InputQueue,
}

struct OpenOutput {
    name: String,
    connection: MidiOutputConnection,
}

/// Platform MIDI backend backed by `midir`.
#[derive(Default)]
pub struct MidirBackend {
    inputs: HashMap<InputId, OpenInput>,
    outputs: HashMap<OutputId, OpenOutput>,
    next_id: u32,
}

impl MidirBackend {
    /// Probe MIDI support and create the backend.
    pub fn new() -> Result<Self, MidiError> {
        // Probe both directions so "no MIDI support" fails at startup, not on play.
        MidiInput::new(CLIENT_NAME).map_err(MidiError::unavailable)?;
        MidiOutput::new(CLIENT_NAME).map_err(MidiError::unavailable)?;
        Ok(Self::default())
    }

    fn allocate(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

fn connect_error(
    direction: PortDirection,
    name: &str,
    error: &midir::ConnectErrorKind,
) -> MidiError {
    match error {
        ConnectErrorKind::InvalidPort => MidiError::PortNotFound {
            direction,
            name: name.to_string(),
        },
        ConnectErrorKind::Other(message) => MidiError::connect(direction, name, message),
    }
}

impl MidiBackend for MidirBackend {
    fn list_inputs(&self) -> Result<Vec<MidiPortInfo>, MidiError> {
        let input = MidiInput::new(CLIENT_NAME).map_err(MidiError::unavailable)?;
        Ok(input
            .ports()
            .iter()
            .enumerate()
            .filter_map(|(index, port)| {
                let name = input.port_name(port).ok()?;
                Some(MidiPortInfo::new(
                    PortDirection::Input,
                    index,
                    name,
                    port.id(),
                ))
            })
            .collect())
    }

    fn list_outputs(&self) -> Result<Vec<MidiPortInfo>, MidiError> {
        let output = MidiOutput::new(CLIENT_NAME).map_err(MidiError::unavailable)?;
        Ok(output
            .ports()
            .iter()
            .enumerate()
            .filter_map(|(index, port)| {
                let name = output.port_name(port).ok()?;
                Some(MidiPortInfo::new(
                    PortDirection::Output,
                    index,
                    name,
                    port.id(),
                ))
            })
            .collect())
    }

    fn open_input(&mut self, port: &MidiPortInfo) -> Result<InputId, MidiError> {
        if self.inputs.values().any(|open| open.name == port.name) {
            return Err(MidiError::AlreadyOpen {
                direction: PortDirection::Input,
                name: port.name.clone(),
            });
        }
        let input = MidiInput::new(CLIENT_NAME).map_err(MidiError::unavailable)?;
        let actual = input
            .find_port_by_id(&port.id)
            .or_else(|| {
                let ports = input.ports();
                ports
                    .into_iter()
                    .find(|candidate| input.port_name(candidate).ok() == Some(port.name.clone()))
            })
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Input,
                name: port.name.clone(),
            })?;
        let (mut push, queue) = InputQueue::new(INPUT_QUEUE_CAPACITY);
        let connection = input
            .connect(
                &actual,
                "opencompose-input",
                move |timestamp_micros, message, _| push(timestamp_micros, message),
                (),
            )
            .map_err(|error| connect_error(PortDirection::Input, &port.name, &error.kind()))?;
        let id = InputId(self.allocate());
        self.inputs.insert(
            id,
            OpenInput {
                name: port.name.clone(),
                _connection: connection,
                queue,
            },
        );
        Ok(id)
    }

    fn open_output(&mut self, port: &MidiPortInfo) -> Result<OutputId, MidiError> {
        if self.outputs.values().any(|open| open.name == port.name) {
            return Err(MidiError::AlreadyOpen {
                direction: PortDirection::Output,
                name: port.name.clone(),
            });
        }
        let output = MidiOutput::new(CLIENT_NAME).map_err(MidiError::unavailable)?;
        let actual = output
            .find_port_by_id(&port.id)
            .or_else(|| {
                let ports = output.ports();
                ports
                    .into_iter()
                    .find(|candidate| output.port_name(candidate).ok() == Some(port.name.clone()))
            })
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Output,
                name: port.name.clone(),
            })?;
        let connection = output
            .connect(&actual, "opencompose-output")
            .map_err(|error| connect_error(PortDirection::Output, &port.name, &error.kind()))?;
        let id = OutputId(self.allocate());
        self.outputs.insert(
            id,
            OpenOutput {
                name: port.name.clone(),
                connection,
            },
        );
        Ok(id)
    }

    fn close_input(&mut self, input: InputId) {
        self.inputs.remove(&input);
    }

    fn close_output(&mut self, output: OutputId) {
        self.outputs.remove(&output);
    }

    fn send(&mut self, output: OutputId, message: &[u8]) -> Result<(), MidiError> {
        let port = self
            .outputs
            .get_mut(&output)
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Output,
                name: format!("{output:?}"),
            })?;
        port.connection.send(message).map_err(|error| match error {
            SendError::InvalidData(detail) => MidiError::invalid(detail),
            SendError::Other(_) => MidiError::Disconnected {
                direction: PortDirection::Output,
                name: port.name.clone(),
            },
        })
    }

    fn poll(&mut self, input: InputId, out: &mut [MidiInputEvent]) -> Result<usize, MidiError> {
        let port = self
            .inputs
            .get_mut(&input)
            .ok_or_else(|| MidiError::PortNotFound {
                direction: PortDirection::Input,
                name: format!("{input:?}"),
            })?;
        port.queue.poll(&port.name, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::time::{Duration, Instant};

    #[test]
    fn input_overflow_drains_only_the_intact_prefix_and_stays_latched() {
        let (mut push, mut queue) = InputQueue::new(2);
        push(11, &[0x90, 60, 100]);
        push(22, &[0xb0, 64, 127]);
        push(33, &[0x80, 60, 0]); // First lost event.
        let mut output = [MidiInputEvent::default(); 1];
        assert_eq!(queue.poll("Keys", &mut output).unwrap(), 1);
        assert_eq!(output[0].timestamp_micros, 11);
        assert!(matches!(
            output[0].event,
            LiveEvent::NoteOn { note: 60, .. }
        ));
        push(44, &[0xb0, 64, 0]); // The freed slot does not admit a post-gap event.
        assert_eq!(queue.poll("Keys", &mut output).unwrap(), 1);
        assert_eq!(output[0].timestamp_micros, 22);
        assert!(matches!(
            output[0].event,
            LiveEvent::ControlChange {
                controller: 64,
                value: 127,
                ..
            }
        ));
        assert_eq!(queue.poll("Keys", &mut []).unwrap(), 0);
        for _ in 0..2 {
            assert_eq!(
                queue.poll("Keys", &mut output),
                Err(MidiError::InputOverflow {
                    name: "Keys".into()
                }),
            );
        }

        let (mut push, mut fresh) = InputQueue::new(2);
        push(55, &[0x90, 62, 100]);
        assert_eq!(fresh.poll("Keys", &mut output).unwrap(), 1);
        assert_eq!(output[0].timestamp_micros, 55);
    }

    #[test]
    fn missing_port_is_a_typed_error() {
        let Ok(mut backend) = MidirBackend::new() else {
            eprintln!("skipping missing-port test: MIDI support is unavailable");
            return;
        };
        let missing = MidiPortInfo::new(
            PortDirection::Input,
            999,
            "definitely-not-a-midi-port",
            "missing",
        );
        assert!(matches!(
            backend.open_input(&missing),
            Err(MidiError::PortNotFound { .. })
        ));
    }

    /// Serialises the virtual-MIDI loopback across processes: the MIDI server is
    /// shared, and several test binaries creating virtual endpoints at once have
    /// been seen to abort the runner.
    #[cfg(target_os = "macos")]
    fn loopback_lock() -> Option<std::fs::File> {
        let path = std::env::temp_dir().join("opencompose-midi-loopback.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .ok()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if file.try_lock().is_ok() {
                return Some(file);
            }
            if std::time::Instant::now() >= deadline {
                eprintln!("skipping CoreMIDI loopback test: another run holds the lock");
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn coremidi_virtual_loopback() {
        use midir::os::unix::{VirtualInput, VirtualOutput};
        use std::sync::{Arc, Mutex};

        const TIMEOUT: Duration = Duration::from_secs(3);
        let Some(_lock) = loopback_lock() else {
            return;
        };
        let suffix = std::process::id();
        let input_name = format!("OpenCompose Loopback Input {suffix}");
        let output_name = format!("OpenCompose Loopback Output {suffix}");

        let received = Arc::new(Mutex::new(Vec::<LiveEvent>::new()));
        let sink = Arc::clone(&received);
        let input = match MidiInput::new("OpenCompose loopback test") {
            Ok(input) => input,
            Err(error) => {
                eprintln!("skipping CoreMIDI loopback test: {error}");
                return;
            }
        };
        let _virtual_input = match input.create_virtual(
            &input_name,
            move |_timestamp, message, _| {
                if let Ok(event) = LiveEvent::parse(message) {
                    sink.lock().expect("loopback sink").push(event);
                }
            },
            (),
        ) {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("skipping CoreMIDI loopback test: {error}");
                return;
            }
        };

        let mut backend = MidirBackend::new().expect("MIDI backend");
        let Some(port) = wait_for_port(|| {
            backend
                .list_outputs()
                .ok()?
                .into_iter()
                .find(|port| port.name.contains(&input_name))
        }) else {
            eprintln!("skipping CoreMIDI loopback test: virtual input was not enumerated");
            return;
        };
        let output = backend.open_output(&port).expect("open loopback output");
        backend
            .send_live(
                output,
                LiveEvent::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            )
            .expect("send loopback note");
        let deadline = Instant::now() + TIMEOUT;
        while received.lock().expect("loopback sink").is_empty() {
            if Instant::now() > deadline {
                panic!("CoreMIDI virtual output did not deliver the note");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            received.lock().expect("loopback sink").as_slice(),
            &[LiveEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            }]
        );

        // The reverse direction: a virtual source feeds MidirBackend's input.
        let output = MidiOutput::new("OpenCompose loopback source").expect("MIDI output");
        let mut source = match output.create_virtual(&output_name) {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("skipping CoreMIDI input loopback: {error}");
                return;
            }
        };
        let Some(port) = wait_for_port(|| {
            backend
                .list_inputs()
                .ok()?
                .into_iter()
                .find(|port| port.name.contains(&output_name))
        }) else {
            eprintln!("skipping CoreMIDI input loopback: virtual source was not enumerated");
            return;
        };
        let input = backend.open_input(&port).expect("open loopback input");
        source.send(&[0x90, 64, 90]).expect("send source note");
        let mut buffer = [MidiInputEvent::default(); 8];
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let count = backend
                .poll(input, &mut buffer)
                .expect("poll loopback input");
            if count > 0 {
                assert_eq!(
                    buffer[0].event,
                    LiveEvent::NoteOn {
                        channel: 0,
                        note: 64,
                        velocity: 90
                    }
                );
                break;
            }
            if Instant::now() > deadline {
                panic!("CoreMIDI virtual input did not receive the note");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_for_port(mut find: impl FnMut() -> Option<MidiPortInfo>) -> Option<MidiPortInfo> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(port) = find() {
                return Some(port);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
