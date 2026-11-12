//! Editor parameter edits forwarded from the UI thread to live processors.
//!
//! An editor reports a knob move through `IComponentHandler::performEdit` on
//! the UI thread. The processor only sees parameters in `ProcessData`, so each
//! edit lands in a preallocated slot that every processor drains before its
//! next block. Slots are fixed when a preset's bus is created: the editor
//! writes, processors read, and neither side locks or allocates.
//!
//! A fast drag coalesces to the latest value per parameter per block, so a
//! gesture cannot overflow a queue. The value lands at offset 0 of the next
//! block through the same queue that effect automation uses.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use vst3::Steinberg::Vst::ParamID;

/// Latest editor value for one parameter.
struct Slot {
    id: ParamID,
    /// `f64` bits, stored before `generation` with release ordering.
    value: AtomicU64,
    generation: AtomicU64,
}

/// Latest editor values for every parameter of one preset.
pub struct ParamEditBus {
    slots: Vec<Slot>,
    gesture_depth: AtomicU32,
    gesture_epoch: AtomicU64,
}

impl ParamEditBus {
    fn new(ids: Vec<ParamID>) -> Arc<Self> {
        Arc::new(Self {
            slots: ids
                .into_iter()
                .map(|id| Slot {
                    id,
                    value: AtomicU64::new(0.0_f64.to_bits()),
                    generation: AtomicU64::new(0),
                })
                .collect(),
            gesture_depth: AtomicU32::new(0),
            gesture_epoch: AtomicU64::new(0),
        })
    }

    fn slot(&self, id: ParamID) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.id == id)
    }

    /// Store one normalized value; the next block of every subscriber sees it.
    pub fn publish(&self, id: ParamID, value: f64) -> bool {
        let Some(slot) = self.slot(id) else {
            return false;
        };
        slot.value
            .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Release);
        slot.generation.fetch_add(1, Ordering::Release);
        true
    }

    /// Mark the start of a drag. Nesting follows `startGroupEdit`.
    pub fn begin_gesture(&self) {
        self.gesture_depth.fetch_add(1, Ordering::AcqRel);
    }

    /// Mark the end of a drag and let observers notice it happened.
    #[allow(
        deprecated,
        reason = "fetch_update supports the minimum Rust version, which predates try_update"
    )]
    /// Mark the end of a drag and bump the gesture epoch.
    pub fn end_gesture(&self) {
        let _ = self
            .gesture_depth
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                Some(depth.saturating_sub(1))
            });
        self.gesture_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Whether a drag is active.
    pub fn gesture_active(&self) -> bool {
        self.gesture_depth.load(Ordering::Acquire) > 0
    }

    /// Monotonic gesture epoch.
    pub fn gesture_epoch(&self) -> u64 {
        self.gesture_epoch.load(Ordering::Acquire)
    }

    /// A reader that owns its own high-water marks, one per subscriber.
    pub fn receiver(self: &Arc<Self>) -> ParamEditReceiver {
        ParamEditReceiver {
            bus: Arc::clone(self),
            seen: vec![0; self.slots.len()],
        }
    }

    /// Parameter ids in slot order.
    pub fn parameter_ids(&self) -> impl Iterator<Item = ParamID> + '_ {
        self.slots.iter().map(|slot| slot.id)
    }
}

/// Per-subscriber cursor over a [`ParamEditBus`].
pub struct ParamEditReceiver {
    bus: Arc<ParamEditBus>,
    seen: Vec<u64>,
}

impl ParamEditReceiver {
    /// Visit every parameter edited since the last drain; allocation-free.
    pub fn drain(&mut self, mut sink: impl FnMut(ParamID, f64)) -> usize {
        let mut visited = 0;
        for (index, slot) in self.bus.slots.iter().enumerate() {
            let generation = slot.generation.load(Ordering::Acquire);
            if generation == self.seen[index] {
                continue;
            }
            let value = f64::from_bits(slot.value.load(Ordering::Acquire));
            self.seen[index] = generation;
            sink(slot.id, value);
            visited += 1;
        }
        visited
    }
}

static BUSES: OnceLock<Mutex<HashMap<String, Arc<ParamEditBus>>>> = OnceLock::new();

fn buses() -> &'static Mutex<HashMap<String, Arc<ParamEditBus>>> {
    BUSES.get_or_init(Default::default)
}

/// The edit bus for a preset; the first caller fixes the parameter layout.
pub fn bus_for_preset(
    preset: &str,
    parameter_ids: impl IntoIterator<Item = ParamID>,
) -> Arc<ParamEditBus> {
    let mut buses = buses().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(bus) = buses.get(preset) {
        return Arc::clone(bus);
    }
    let mut ids: Vec<ParamID> = parameter_ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    let bus = ParamEditBus::new(ids);
    buses.insert(preset.to_owned(), Arc::clone(&bus));
    bus
}

/// The bus already created for a preset, if any.
pub fn existing_bus(preset: &str) -> Option<Arc<ParamEditBus>> {
    buses()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(preset)
        .cloned()
}

/// Drop every preset bus; call when the project is replaced.
pub fn clear_param_edit_buses() {
    buses()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// One message of the out-of-process editor protocol.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditLine {
    Edit(ParamID, f64),
    Begin,
    End,
}

/// Parse `e <id> <value>`, `b <id>` or `x <id>`.
pub fn parse_edit_line(line: &str) -> Option<EditLine> {
    let mut fields = line.split_whitespace();
    let tag = fields.next()?;
    let id: ParamID = fields.next()?.parse().ok()?;
    match tag {
        "e" => Some(EditLine::Edit(id, fields.next()?.parse().ok()?)),
        "b" => Some(EditLine::Begin),
        "x" => Some(EditLine::End),
        _ => None,
    }
}

/// Parse the `params <count> <id>...` header that precedes the edits.
pub fn parse_parameter_header(line: &str) -> Option<Vec<ParamID>> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "params" {
        return None;
    }
    let count: usize = fields.next()?.parse().ok()?;
    let ids: Vec<ParamID> = fields
        .map(|field| field.parse().ok())
        .collect::<Option<_>>()?;
    (ids.len() == count).then_some(ids)
}

#[cfg(unix)]
mod unix_socket {
    use super::*;
    use std::io::{BufRead, BufReader, BufWriter, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// How long the parent waits for the helper to connect.
    const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Helper-process side of the edit protocol: one line per edit.
    pub struct EditSocket {
        writer: Mutex<BufWriter<UnixStream>>,
    }

    impl EditSocket {
        /// Connect to the parent and announce this plug-in's parameter layout.
        pub fn connect(path: &Path, ids: &[ParamID]) -> Result<Self, String> {
            let stream = UnixStream::connect(path).map_err(|error| {
                format!(
                    "could not open the edit socket '{}': {error}",
                    path.display()
                )
            })?;
            let mut writer = BufWriter::new(stream);
            write!(writer, "params {}", ids.len()).map_err(|error| error.to_string())?;
            for id in ids {
                write!(writer, " {id}").map_err(|error| error.to_string())?;
            }
            writeln!(writer).map_err(|error| error.to_string())?;
            writer.flush().map_err(|error| error.to_string())?;
            Ok(Self {
                writer: Mutex::new(writer),
            })
        }

        fn send(&self, line: String) {
            let Ok(mut writer) = self.writer.lock() else {
                return;
            };
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }

        /// Publish a normalized value to subscribers.
        pub fn publish(&self, id: ParamID, value: f64) {
            self.send(format!("e {id} {value:.9}\n"));
        }

        /// Start a drag for one parameter.
        pub fn begin(&self, id: ParamID) {
            self.send(format!("b {id}\n"));
        }

        /// End a drag for one parameter.
        pub fn end(&self, id: ParamID) {
            self.send(format!("x {id}\n"));
        }
    }

    /// Parent side: accept the helper, then publish its edits into the preset bus.
    pub fn listen(path: &Path, preset: &str) -> Result<(), String> {
        let listener = UnixListener::bind(path)
            .map_err(|error| format!("could not listen on '{}': {error}", path.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("could not poll '{}': {error}", path.display()))?;
        let preset = preset.to_owned();
        std::thread::Builder::new()
            .name("ocp-editor-edits".to_string())
            .spawn(move || {
                let deadline = Instant::now() + ACCEPT_TIMEOUT;
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => return,
                    }
                };
                // An accepted socket inherits nonblocking on macOS; the reader
                // asks for a blocking stream.
                let _ = stream.set_nonblocking(false);
                let mut lines = BufReader::new(stream).lines();
                let Some(Ok(header)) = lines.next() else {
                    return;
                };
                let Some(ids) = parse_parameter_header(&header) else {
                    return;
                };
                if ids.is_empty() {
                    return;
                }
                let bus = bus_for_preset(&preset, ids);
                for line in lines.map_while(Result::ok) {
                    match parse_edit_line(&line) {
                        Some(EditLine::Edit(id, value)) => {
                            bus.publish(id, value);
                        }
                        Some(EditLine::Begin) => bus.begin_gesture(),
                        Some(EditLine::End) => bus.end_gesture(),
                        None => {}
                    }
                }
            })
            .map_err(|error| format!("could not start the edit listener: {error}"))?;
        Ok(())
    }
}

#[cfg(unix)]
pub use unix_socket::{EditSocket, listen as listen_for_edits};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bus_coalesces_a_drag_to_the_latest_value_per_parameter() {
        let bus = ParamEditBus::new(vec![7, 9]);
        let mut receiver = bus.receiver();
        for step in 0..500 {
            bus.publish(7, step as f64 / 500.0);
            bus.publish(9, 0.25);
        }
        let mut seen = Vec::new();
        assert_eq!(receiver.drain(|id, value| seen.push((id, value))), 2);
        assert_eq!(seen[0].0, 7);
        assert!((seen[0].1 - 0.998).abs() < 1e-9);
        assert_eq!(seen[1], (9, 0.25));
        assert_eq!(
            receiver.drain(|_, _| panic!("a drained edit must not repeat")),
            0
        );
    }

    #[test]
    fn every_subscriber_sees_the_same_edit() {
        let bus = ParamEditBus::new(vec![3]);
        let mut left = bus.receiver();
        let mut right = bus.receiver();
        bus.publish(3, 0.75);
        let mut left_values = Vec::new();
        let mut right_values = Vec::new();
        left.drain(|_, value| left_values.push(value));
        right.drain(|_, value| right_values.push(value));
        assert_eq!(left_values, vec![0.75]);
        assert_eq!(right_values, vec![0.75]);
    }

    #[test]
    fn a_gesture_counts_its_depth_and_reports_its_end() {
        let bus = ParamEditBus::new(vec![1]);
        let before = bus.gesture_epoch();
        bus.begin_gesture();
        bus.begin_gesture();
        assert!(bus.gesture_active());
        bus.end_gesture();
        assert!(bus.gesture_active());
        bus.end_gesture();
        bus.end_gesture();
        assert!(!bus.gesture_active());
        assert_eq!(bus.gesture_epoch(), before + 3);
    }

    #[test]
    fn an_unknown_parameter_is_not_published() {
        let bus = ParamEditBus::new(vec![1]);
        assert!(!bus.publish(2, 0.5));
    }

    #[test]
    fn edit_lines_round_trip() {
        assert_eq!(parse_edit_line("e 42 0.75"), Some(EditLine::Edit(42, 0.75)));
        assert_eq!(parse_edit_line("b 42"), Some(EditLine::Begin));
        assert_eq!(parse_edit_line("x 42"), Some(EditLine::End));
        assert_eq!(parse_edit_line("params 1 42"), None);
        assert_eq!(parse_parameter_header("params 2 42 7"), Some(vec![42, 7]));
        assert_eq!(parse_parameter_header("params 2 42"), None);
    }

    #[cfg(unix)]
    mod socket {
        use super::*;
        use std::time::{Duration, Instant};

        fn socket_path(name: &str) -> std::path::PathBuf {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            std::env::temp_dir().join(format!("ocp-edit-{name}-{unique}.sock"))
        }

        #[test]
        fn a_helper_socket_forwards_edits_and_gestures() {
            let path = socket_path("forward");
            let preset = format!("socket-test-{}", path.display());
            listen_for_edits(&path, &preset).expect("listener starts");
            let socket = EditSocket::connect(&path, &[5, 6]).expect("socket connects");
            socket.begin(5);
            socket.publish(5, 0.4);
            socket.publish(5, 0.9);
            socket.end(5);
            drop(socket);

            let deadline = Instant::now() + Duration::from_secs(2);
            let mut bus = None;
            while Instant::now() < deadline && bus.is_none() {
                bus = existing_bus(&preset);
                std::thread::sleep(Duration::from_millis(5));
            }
            let bus = bus.expect("the listener created the preset bus");
            assert_eq!(bus.parameter_ids().collect::<Vec<_>>(), vec![5, 6]);
            let mut receiver = bus.receiver();
            let mut values = Vec::new();
            while Instant::now() < deadline && values.is_empty() {
                receiver.drain(|_, value| values.push(value));
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(values, vec![0.9]);
            assert!(!bus.gesture_active());
            assert!(bus.gesture_epoch() >= 1);
            let _ = std::fs::remove_file(&path);
        }
    }
}
