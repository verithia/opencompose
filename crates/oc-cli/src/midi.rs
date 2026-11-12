//! `ocp midi` commands: port listing and a timestamped monitor.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use oc_engine::midi_io::{
    LiveEvent, MidiBackend, MidiInputEvent, MidiPortInfo, MidiSession, MidirBackend,
};

/// List the available MIDI input and output ports.
pub fn list() -> Result<()> {
    let backend = MidirBackend::new().context("MIDI backend unavailable")?;
    let inputs = backend.list_inputs().context("list MIDI inputs")?;
    let outputs = backend.list_outputs().context("list MIDI outputs")?;
    println!("MIDI inputs:");
    print_ports(&inputs);
    println!("MIDI outputs:");
    print_ports(&outputs);
    Ok(())
}

fn print_ports(ports: &[MidiPortInfo]) {
    if ports.is_empty() {
        println!("  (none)");
        return;
    }
    for port in ports {
        println!("  [{}] {}", port.index, port.name);
    }
}

/// Print incoming events from one MIDI input until interrupted.
pub fn monitor(input: &str) -> Result<()> {
    let backend = MidirBackend::new().context("MIDI backend unavailable")?;
    let mut session = MidiSession::open(backend, Some(input), None)
        .with_context(|| format!("open MIDI input '{input}'"))?;
    let name = session.input_name().unwrap_or(input).to_string();
    println!("Monitoring '{name}' (Ctrl+C to stop)");
    let started = Instant::now();
    let mut buffer = [MidiInputEvent::default(); 256];
    loop {
        let count = session
            .poll(&mut buffer)
            .with_context(|| format!("read MIDI input '{name}'"))?;
        for event in &buffer[..count] {
            let seconds = if event.timestamp_micros == 0 {
                started.elapsed().as_secs_f64()
            } else {
                event.timestamp_micros as f64 / 1_000_000.0
            };
            println!("[{seconds:>10.6}] {}", describe(event.event));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Human-readable event text used by the monitor and live-play diagnostics.
pub fn describe(event: LiveEvent) -> String {
    match event {
        LiveEvent::NoteOn {
            channel,
            note,
            velocity,
        } => format!(
            "note on   ch={} {:<4} vel={}",
            channel + 1,
            note_name(note),
            velocity
        ),
        LiveEvent::NoteOff {
            channel,
            note,
            velocity,
        } => format!(
            "note off  ch={} {:<4} vel={}",
            channel + 1,
            note_name(note),
            velocity
        ),
        LiveEvent::PolyPressure {
            channel,
            note,
            pressure,
        } => format!(
            "poly at   ch={} {:<4} pressure={}",
            channel + 1,
            note_name(note),
            pressure
        ),
        LiveEvent::ControlChange {
            channel,
            controller,
            value,
        } => format!(
            "cc        ch={} controller={} value={}",
            channel + 1,
            controller,
            value
        ),
        LiveEvent::ProgramChange { channel, program } => {
            format!("program   ch={} program={}", channel + 1, program)
        }
        LiveEvent::ChannelPressure { channel, pressure } => {
            format!("pressure  ch={} value={}", channel + 1, pressure)
        }
        LiveEvent::PitchBend { channel, value } => {
            format!("bend      ch={} value={}", channel + 1, value)
        }
        LiveEvent::Clock => "clock".to_string(),
        LiveEvent::Start => "start".to_string(),
        LiveEvent::Continue => "continue".to_string(),
        LiveEvent::Stop => "stop".to_string(),
    }
}

fn note_name(note: u8) -> String {
    oc_dsl::ast::Pitch::from_midi(note)
        .map(|pitch| pitch.to_text())
        .unwrap_or_else(|| format!("invalid MIDI note {note}"))
}
