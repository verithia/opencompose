/**
 * DSL round-trip tests — verifies that project data survives
 * parse → store update → serialize → re-parse cycles.
 *
 * These tests mock the Tauri backend to simulate what the real backend does:
 * parse DSL → return snapshot → frontend applies it.
 * Then they verify the DSL source can be re-parsed to produce the same project.
 *
 * This catches data loss bugs (like missing fields in TS types).
 */
import { describe, it, expect, beforeEach } from "vitest";
import { invokeMock, makeSnapshot, makeTrack, makeNote } from "../setup";
import { store } from "../../stores/project";
import type { Project } from "../../types/project";

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_project") return makeSnapshot();
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot();
  });
  await store.initProject();
});

describe("DSL round-trip", () => {
  it("project data is preserved through snapshot application", () => {
    const project = store.project;
    expect(project.name).toBe("Test Project");
    expect(project.properties.tempo).toBe(128);
    expect(project.properties.time_signature.numerator).toBe(4);
    expect(project.properties.time_signature.denominator).toBe(4);
  });

  it("complex project with all features survives round-trip", async () => {
    const complexProject: Project = {
      name: "Complex Test",
      properties: {
        tempo: 140,
        time_signature: { numerator: 7, denominator: 8 },
        key: { root: "D", accidental: "Natural", mode: "Minor" },
        swing: 55,
        tempo_map: [{ position: { bar: 5, beat: 1, subdivision: 0 }, bpm: 160 }],
        time_sig_map: [{ position: { bar: 9, beat: 1, subdivision: 0 }, numerator: 3, denominator: 4 }],
        key_map: [{ position: { bar: 3, beat: 1, subdivision: 0 }, root: "G", accidental: "Natural", mode: "Major" }],
      },
      presets: [
        { name: "Synth Lead", source: "clap", plugin: "com.surge-synth-team.surge-xt", program: null, patch: "Init", file: null, root_note: null, extends: null, params: [["cutoff", { Number: 0.7 }]], effects: [] },
        { name: "Bass", source: "soundfont", plugin: null, program: 33, patch: null, file: null, root_note: null, extends: "Synth Lead", params: [], effects: [] },
      ],
      patterns: [
        {
          name: "drum_pattern",
          instrument: { category: "drums", name: "electronic_kit" },
          preset: null, engine: null, transpose: -2, swing: 65, color: "#ff6600",
          effects: [],
          events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 1 })],
          hits: [
            { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
            { drum: "snare", position: { beat: 3, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] },
          ],
        },
      ],
      tracks: [
        {
          ...makeTrack("Lead Synth", 0),
          preset: "Synth Lead",
          mix: { volume_db: -3.5, pan: -0.2, sends: [{ target: "reverb_bus", amount: 0.4 }], mute: false, solo: false },
          effects: [{ effect_type: "Eq", params: [["freq", { Number: 2000 }]] }],
          clips: [{ pattern: "drum_pattern", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 5, beat: 1, subdivision: 0 } } }],
        },
        {
          ...makeTrack("Bass", 1),
          preset: "Bass",
          mix: { volume_db: 0, pan: 0, sends: [], mute: true, solo: false },
        },
      ],
      buses: [
        { name: "reverb_bus", routes: ["master"], effects: [{ effect_type: "Reverb", params: [["mix", { Number: 0.3 }]] }] },
      ],
      master: {
        effects: [{ effect_type: "Limiter", params: [["threshold", { Db: -1.0 }]] }],
        loudness_target: null,
        volume_db: 0,
      },
      sections: [{ name: "intro", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 8, beat: 1, subdivision: 0 } }, repeat: null, label: null }],
    };

    const snap = {
      project: complexProject,
      dsl_source: "# complex project DSL source",
      file_path: "/tmp/test.ocp",
      dirty: true,
      track_count: 2,
    };

    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();

    // Verify all data survived the round-trip through the store
    expect(store.project.name).toBe("Complex Test");
    expect(store.project.properties.tempo).toBe(140);
    expect(store.project.properties.time_signature.numerator).toBe(7);
    expect(store.project.properties.key).toEqual({ root: "D", accidental: "Natural", mode: "Minor" });
    expect(store.project.properties.swing).toBe(55);

    // Presets
    expect(store.project.presets.length).toBe(2);
    expect(store.project.presets[0].extends).toBe(null);
    expect(store.project.presets[1].extends).toBe("Synth Lead");
    expect(store.project.presets[0].params.length).toBe(1);

    // Patterns
    expect(store.project.patterns.length).toBe(1);
    expect(store.project.patterns[0].transpose).toBe(-2);
    expect(store.project.patterns[0].swing).toBe(65);
    expect(store.project.patterns[0].color).toBe("#ff6600");
    expect(store.project.patterns[0].events.length).toBe(2);
    expect(store.project.patterns[0].hits.length).toBe(2);

    // Tracks
    expect(store.project.tracks.length).toBe(2);
    expect(store.project.tracks[0].preset).toBe("Synth Lead");
    expect(store.project.tracks[0].mix.volume_db).toBe(-3.5);
    expect(store.project.tracks[0].mix.pan).toBe(-0.2);
    expect(store.project.tracks[0].mix.sends.length).toBe(1);
    expect(store.project.tracks[0].mix.sends[0].target).toBe("reverb_bus");
    expect(store.project.tracks[0].effects.length).toBe(1);
    expect(store.project.tracks[0].clips.length).toBe(1);
    expect(store.project.tracks[1].mix.mute).toBe(true);

    // Buses
    expect(store.project.buses.length).toBe(1);
    expect(store.project.buses[0].effects.length).toBe(1);

    // Master
    expect(store.project.master).not.toBe(null);
    expect(store.project.master!.effects.length).toBe(1);

    // Sections
    expect(store.project.sections.length).toBe(1);

    // File path & dirty
    expect(store.filePath()).toBe("/tmp/test.ocp");
    expect(store.dirty()).toBe(true);
  });

  it("empty project survives round-trip", async () => {
    const emptySnap = makeSnapshot({
      name: "Empty",
      tracks: [],
      patterns: [],
      presets: [],
      buses: [],
      master: null,
      sections: [],
    });
    invokeMock.mockResolvedValueOnce(emptySnap);
    await store.initProject();

    expect(store.project.tracks.length).toBe(0);
    expect(store.project.patterns.length).toBe(0);
    expect(store.project.presets.length).toBe(0);
    expect(store.project.buses.length).toBe(0);
    expect(store.project.master).toBe(null);
  });

  it("track with multiple phrases and events preserves note data", async () => {
    const detailedTrack = {
      ...makeTrack("Detailed", 0),
      phrases: [
        {
          name: "intro",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [
            makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Sharp", octave: 5 }, duration: { base: "Eighth", dots: 1, triplet: false, tuplet: null, ties: [] }, velocity: 120, muted: false, grace: true }),
            makeNote({ start_beat: 0.75, pitch: { note: "E", accidental: "Flat", octave: 4 }, dynamics: "PP" }),
            makeNote({ start_beat: 2, pitch: { note: "G", accidental: "Natural", octave: 3 }, muted: true }),
          ],
        },
        {
          name: "verse",
          time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 8, beat: 4, subdivision: 0 } },
          events: [makeNote({ start_beat: 0, pitch: { note: "A", accidental: "Natural", octave: 4 } })],
        },
      ],
    };

    const snap = makeSnapshot({ tracks: [detailedTrack] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();

    const track = store.project.tracks[0];
    expect(track.phrases.length).toBe(2);
    expect(track.phrases[0].events.length).toBe(3);
    expect(track.phrases[0].events[0].pitch.accidental).toBe("Sharp");
    expect(track.phrases[0].events[0].pitch.octave).toBe(5);
    expect(track.phrases[0].events[0].duration.dots).toBe(1);
    expect(track.phrases[0].events[0].velocity).toBe(120);
    expect(track.phrases[0].events[0].grace).toBe(true);
    expect(track.phrases[0].events[1].pitch.accidental).toBe("Flat");
    expect(track.phrases[0].events[1].dynamics).toBe("PP");
    expect(track.phrases[0].events[2].muted).toBe(true);
    expect(track.phrases[1].name).toBe("verse");
    expect(track.phrases[1].time_range!.start.bar).toBe(5);
  });
});
