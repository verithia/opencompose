//! Preallocated single-producer/single-consumer queue for the audio thread.
//!
//! The MIDI thread owns the producer and the audio callback owns the consumer.
//! `rtrb` allocates the storage once; `push`, `pop`, and `drain_into` touch only
//! atomics and ring slots, so the callback never blocks or allocates.

use super::message::LiveEvent;

/// A target that can apply a live event from the audio thread.
pub trait LiveEventTarget {
    fn apply_live_event(&mut self, event: LiveEvent);
}

impl LiveEventTarget for oc_synth::synth::OpenSynth {
    fn apply_live_event(&mut self, event: LiveEvent) {
        if let Some(message) = event.to_engine_message() {
            let _ = self.send_midi(&message);
        }
    }
}

/// Single-producer end of the live-event queue.
pub struct LiveEventProducer {
    producer: rtrb::Producer<LiveEvent>,
}

/// Single-consumer end of the live-event queue.
pub struct LiveEventConsumer {
    consumer: rtrb::Consumer<LiveEvent>,
}

/// Create a bounded queue. Capacity is rounded up by the ring implementation.
pub fn channel(capacity: usize) -> (LiveEventProducer, LiveEventConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity.max(2));
    (
        LiveEventProducer { producer },
        LiveEventConsumer { consumer },
    )
}

impl LiveEventProducer {
    /// Push one event; returns it when the queue is full.
    pub fn push(&mut self, event: LiveEvent) -> Result<(), LiveEvent> {
        self.producer.push(event).map_err(|error| match error {
            rtrb::PushError::Full(event) => event,
        })
    }

    /// Whether the queue is full.
    pub fn is_full(&self) -> bool {
        self.producer.is_full()
    }

    /// Queue capacity.
    pub fn capacity(&self) -> usize {
        self.producer.buffer().capacity()
    }
}

impl LiveEventConsumer {
    /// Pop one event.
    pub fn pop(&mut self) -> Option<LiveEvent> {
        self.consumer.pop().ok()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    /// Apply at most `max` queued events. Bounded work keeps the callback predictable.
    pub fn drain_into<T: LiveEventTarget>(&mut self, target: &mut T, max: usize) -> usize {
        let mut applied = 0;
        while applied < max {
            let Ok(event) = self.consumer.pop() else {
                break;
            };
            target.apply_live_event(event);
            applied += 1;
        }
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Recorder {
        count: usize,
        in_order: bool,
        expected: usize,
    }

    impl LiveEventTarget for Recorder {
        fn apply_live_event(&mut self, event: LiveEvent) {
            let LiveEvent::NoteOn { note, velocity, .. } = event else {
                self.in_order = false;
                return;
            };
            let value = usize::from(note) | (usize::from(velocity) << 7);
            if value != self.expected {
                self.in_order = false;
            }
            self.expected += 1;
            self.count += 1;
        }
    }

    #[test]
    fn bounded_queue_rejects_when_full() {
        let (mut producer, mut consumer) = channel(2);
        assert_eq!(
            producer.push(LiveEvent::ProgramChange {
                channel: 0,
                program: 1
            }),
            Ok(())
        );
        assert_eq!(
            producer.push(LiveEvent::ProgramChange {
                channel: 0,
                program: 2
            }),
            Ok(())
        );
        assert_eq!(
            producer.push(LiveEvent::ProgramChange {
                channel: 0,
                program: 3
            }),
            Err(LiveEvent::ProgramChange {
                channel: 0,
                program: 3
            })
        );
        assert!(consumer.pop().is_some());
        assert!(producer.push(LiveEvent::Clock).is_ok());
    }

    #[test]
    fn spsc_queue_preserves_order_under_contention() {
        const EVENTS: usize = 16_000;
        let (mut producer, mut consumer) = channel(64);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for sequence in 0..EVENTS {
                    let event = LiveEvent::NoteOn {
                        channel: 0,
                        note: (sequence & 0x7F) as u8,
                        velocity: ((sequence >> 7) & 0x7F) as u8,
                    };
                    let mut pending = event;
                    loop {
                        match producer.push(pending) {
                            Ok(()) => break,
                            Err(rejected) => {
                                pending = rejected;
                                std::hint::spin_loop();
                            }
                        }
                    }
                }
                done.store(true, Ordering::Release);
            });
            let mut recorder = Recorder {
                in_order: true,
                ..Default::default()
            };
            while recorder.count < EVENTS {
                let applied = consumer.drain_into(&mut recorder, 256);
                if applied == 0 && !done.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            }
            assert_eq!(recorder.count, EVENTS);
            assert!(recorder.in_order);
        });
    }

    #[test]
    fn queued_events_play_the_engine_synth() {
        use oc_synth::synth::{OpenSynth, SynthPatch};
        let mut synth = OpenSynth::new_with_tempo(SynthPatch::default(), 120.0);
        let (mut producer, mut consumer) = channel(8);
        producer
            .push(LiveEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            })
            .unwrap();
        assert_eq!(consumer.drain_into(&mut synth, 8), 1);
        let mut audio = [0.0f32; 2048];
        synth.render_interleaved(&mut audio, 48_000);
        assert!(audio.iter().any(|sample| sample.abs() > 1e-6));
        producer
            .push(LiveEvent::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0,
            })
            .unwrap();
        assert_eq!(consumer.drain_into(&mut synth, 8), 1);
    }

    #[test]
    fn drain_respects_the_budget() {
        let (mut producer, mut consumer) = channel(8);
        for program in 0..4 {
            producer
                .push(LiveEvent::ProgramChange {
                    channel: 0,
                    program,
                })
                .unwrap();
        }
        let mut recorder = Recorder {
            in_order: true,
            ..Default::default()
        };
        assert_eq!(consumer.drain_into(&mut recorder, 2), 2);
        assert_eq!(consumer.drain_into(&mut recorder, 2), 2);
        assert_eq!(consumer.drain_into(&mut recorder, 2), 0);
    }
}
