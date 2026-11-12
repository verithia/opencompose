// Vite binds localhost (IPv6); 127.0.0.1 is refused, so probes default there.
export const baseUrl = process.env.OCP_UI_URL ?? "http://localhost:5173";

export function createSmokeSnapshot() {
  return {
    project: {
      name: "Theme Smoke Project",
      properties: {
        tempo: 124,
        time_signature: { numerator: 4, denominator: 4 },
        key: null,
        swing: null,
        tempo_map: [],
        time_sig_map: [],
        key_map: [],
      },
      presets: [
        {
          name: "Graphite Piano",
          source: "soundfont",
          plugin: null,
          program: 0,
          patch: null,
          file: null,
          root_note: null,
          extends: null,
          params: [],
          effects: [],
        },
      ],
      tracks: [
        {
          name: "Piano Lead",
          instrument: { category: "piano", name: "acoustic_grand" },
          preset: "Graphite Piano",
          engine: null,
          mix: { volume_db: -3, pan: 0, sends: [], mute: false, solo: false },
          effects: [],
          phrases: [
            {
              name: "chord_pad",
              time_range: {
                start: { bar: 9, beat: 1, subdivision: 0 },
                end: { bar: 13, beat: 1, subdivision: 0 },
              },
              events: [
                {
                  start_beat: 0,
                  explicit_position: true,
                  pitch: { note: "C", accidental: "Natural", octave: 4 },
                  duration: { base: "Half", dots: 0, triplet: false, tuplet: null, ties: [] },
                  dynamics: "MF",
                  velocity: 76,
                  articulation: null,
                  vibrato: null,
                  expression: null,
                  pitch_bend: null,
                  muted: false,
                  grace: false,
                  text: null,
                  modifiers: [],
                },
                {
                  start_beat: 2,
                  explicit_position: true,
                  pitch: { note: "G", accidental: "Natural", octave: 4 },
                  duration: { base: "Half", dots: 0, triplet: false, tuplet: null, ties: [] },
                  dynamics: "F",
                  velocity: 88,
                  articulation: null,
                  vibrato: null,
                  expression: null,
                  pitch_bend: null,
                  muted: false,
                  grace: false,
                  text: null,
                  modifiers: [],
                },
              ],
            },
          ],
          patterns: [],
          inline_patterns: [],
          clips: [
            {
              pattern: "piano_hook",
              time_range: {
                start: { bar: 1, beat: 1, subdivision: 0 },
                end: { bar: 5, beat: 1, subdivision: 0 },
              },
            },
          ],
          automation: [],
          engine_params: null,
        },
        {
          name: "Drum Room",
          instrument: { category: "drums", name: "acoustic_kit" },
          preset: null,
          engine: null,
          mix: { volume_db: -6, pan: 0.08, sends: [], mute: false, solo: false },
          effects: [],
          phrases: [],
          patterns: [],
          inline_patterns: [],
          clips: [
            {
              pattern: "drum_room",
              time_range: {
                start: { bar: 1, beat: 1, subdivision: 0 },
                end: { bar: 9, beat: 1, subdivision: 0 },
              },
            },
          ],
          automation: [],
          engine_params: null,
        },
      ],
      patterns: [
        {
          name: "piano_hook",
          instrument: { category: "piano", name: "acoustic_grand" },
          preset: "Graphite Piano",
          engine: null,
          transpose: null,
          swing: null,
          effects: [],
          color: null,
          events: [
            {
              start_beat: 0,
              explicit_position: true,
              pitch: { note: "C", accidental: "Natural", octave: 4 },
              duration: { base: "Quarter", dots: 0, triplet: false, tuplet: null, ties: [] },
              dynamics: "MF",
              velocity: 84,
              articulation: null,
              vibrato: null,
              expression: null,
              pitch_bend: null,
              muted: false,
              grace: false,
              text: null,
              modifiers: [],
            },
            {
              start_beat: 1,
              explicit_position: true,
              pitch: { note: "E", accidental: "Natural", octave: 4 },
              duration: { base: "Quarter", dots: 0, triplet: false, tuplet: null, ties: [] },
              dynamics: "MF",
              velocity: 88,
              articulation: null,
              vibrato: null,
              expression: null,
              pitch_bend: null,
              muted: false,
              grace: false,
              text: null,
              modifiers: [],
            },
            {
              start_beat: 2,
              explicit_position: true,
              pitch: { note: "G", accidental: "Natural", octave: 4 },
              duration: { base: "Quarter", dots: 0, triplet: false, tuplet: null, ties: [] },
              dynamics: "F",
              velocity: 96,
              articulation: null,
              vibrato: null,
              expression: null,
              pitch_bend: null,
              muted: false,
              grace: false,
              text: null,
              modifiers: [],
            },
          ],
          hits: [],
        },
        {
          name: "drum_room",
          instrument: { category: "drums", name: "acoustic_kit" },
          preset: null,
          engine: null,
          transpose: null,
          swing: null,
          effects: [],
          color: null,
          events: [],
          hits: [
            { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] },
            { drum: "snare", position: { beat: 3, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
          ],
        },
      ],
      buses: [],
      master: null,
      sections: [],
    },
    dsl_source: '@project "Theme Smoke Project"\n  tempo: 124\n  time_signature: 4/4\n',
    file_path: null,
    dirty: false,
    track_count: 2,
  };
}

const q = (dots = 0, triplet = false) => ({ base: "Quarter", dots, triplet, tuplet: null, ties: [] });
const h = (dots = 0) => ({ base: "Half", dots, triplet: false, tuplet: null, ties: [] });
const w = () => ({ base: "Whole", dots: 0, triplet: false, tuplet: null, ties: [] });
const eighth = () => ({ base: "Eighth", dots: 0, triplet: false, tuplet: null, ties: [] });

function note(noteName, octave, startBeat, duration, dynamics = "MF", velocity = 84, articulation = null) {
  return {
    start_beat: startBeat,
    explicit_position: true,
    pitch: { note: noteName, accidental: "Natural", octave },
    duration,
    dynamics,
    velocity,
    articulation,
    vibrato: null,
    expression: null,
    pitch_bend: null,
    muted: false,
    grace: false,
    text: null,
    modifiers: [],
  };
}

function clip(pattern, startBar, endBar) {
  return {
    pattern,
    time_range: {
      start: { bar: startBar, beat: 1, subdivision: 0 },
      end: { bar: endBar, beat: 1, subdivision: 0 },
    },
  };
}

function preset(name, program, effects = []) {
  return { name, source: "soundfont", plugin: null, program, patch: null, file: null, root_note: null, extends: null, params: [], effects };
}

function track(name, category, instrumentName, presetName, volumeDb, pan = 0, sends = [], clips = [], phrases = [], patterns = [], effects = []) {
  return {
    name,
    instrument: { category, name: instrumentName },
    preset: presetName,
    engine: null,
    mix: { volume_db: volumeDb, pan, sends, mute: false, solo: false },
    effects,
    phrases,
    patterns,
    inline_patterns: [],
    clips,
    automation: [],
    engine_params: null,
  };
}

export function createFullFeaturedSnapshot(dslSource = "") {
  const leadEvents = [
    note("B", 4, 0, q(1), "FF", 118, "accent"),
    note("A", 5, 1.5, eighth(), "F", 108),
    note("G", 5, 2, q(), "F", 104, "legato"),
    note("F", 5, 3, q(), "MF", 92),
    note("E", 5, 4, h(1), "F", 106),
    note("D", 5, 7, q(), "MF", 88, "legato"),
    note("B", 4, 8, h(), "MF", 86),
    note("D", 5, 10, q(), "F", 108),
    note("E", 5, 11, q(), "F", 106, "accent"),
    note("F", 5, 12, w(), "FF", 120),
  ];
  leadEvents[6].vibrato = { rate: { value: 5, unit: "Hz" }, depth: { value: 20, unit: "Cents" }, delay: null };

  return {
    project: {
      name: "Midnight City",
      properties: {
        tempo: 110,
        time_signature: { numerator: 4, denominator: 4 },
        key: { root: "B", accidental: "Flat", mode: "Minor" },
        swing: null,
        tempo_map: [],
        time_sig_map: [],
        key_map: [{ position: { bar: 17, beat: 1, subdivision: 0 }, root: "D", accidental: "Flat", mode: "Major" }],
      },
      presets: [
        preset("lush_pad", 89, [{ effect_type: "Chorus", params: [["mix", { Percentage: 30 }]], automation: [] }]),
        preset("aggressive_lead", 81, [{ effect_type: "Compression", params: [["ratio", { String: "3:1" }]], automation: [] }]),
        preset("punchy_bass", 33),
        preset("tight_drums", 0, [{ effect_type: "Compression", params: [["threshold", { Db: -8 }]], automation: [] }]),
        preset("brass_stab", 61),
        preset("clean_guitar", 27),
        preset("lofi_keys", 4),
        { name: "surge_lead", source: "clap", plugin: "org.surge-synth-team.surge-xt", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] },
      ],
      tracks: [
        track("pad", "piano", "acoustic_grand", "lush_pad", -4, 0, [{ target: "mix_bus", amount: 0.55 }], [], [
          { name: "intro_pad", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 8, beat: 4, subdivision: 0 } }, events: [note("B", 3, 0, w(), "MP", 64, "sustained"), note("D", 4, 4, w(), "MP", 68), note("E", 4, 8, w(), "MF", 78), note("F", 4, 12, w(), "MF", 82)] },
          { name: "chord_pad", time_range: { start: { bar: 17, beat: 1, subdivision: 0 }, end: { bar: 24, beat: 4, subdivision: 0 } }, events: [note("B", 3, 0, h(), "MF", 74), note("D", 4, 0, h(), "MF", 74), note("F", 4, 0, h(), "MF", 76), note("E", 4, 2, h(), "F", 88), note("G", 4, 2, h(), "F", 88), note("B", 4, 2, h(), "F", 90)] },
        ]),
        track("lead", "piano", "acoustic_grand", "aggressive_lead", -6, 0.15, [{ target: "mix_bus", amount: 0.4 }], [clip("lead_chorus", 17, 25)], [], []),
        track("surge_synth", "piano", "acoustic_grand", "surge_lead", -8, -0.2, [{ target: "mix_bus", amount: 0.3 }], [clip("surge_melody", 17, 25)]),
        track("bass", "bass", "electric_bass", "punchy_bass", -2.7, 0, [{ target: "drum_bus", amount: 0.7 }], [clip("bass_chorus", 17, 25)], [], [{ effect_type: "Compression", params: [["sidechain", { String: "drums" }]], automation: [] }]),
        track("drums", "drums", "acoustic_kit", "tight_drums", -1, 0, [{ target: "drum_bus", amount: 0.85 }], [clip("drums_verse_beat", 9, 10), clip("drums_verse_beat", 10, 11), clip("drums_verse_beat", 11, 12), clip("drums_verse_beat", 12, 13), clip("drums_chorus_beat", 17, 18), clip("drums_chorus_beat", 18, 19), clip("drums_chorus_beat", 19, 20), clip("drums_chorus_beat", 20, 21), clip("drums_chorus_beat", 21, 22), clip("drums_chorus_beat", 22, 23), clip("drums_chorus_beat", 23, 24)], [], [
          {
            name: "chorus_fill",
            time_range: { start: { bar: 24, beat: 1, subdivision: 0 }, end: { bar: 25, beat: 1, subdivision: 0 } },
            repeat: 1,
            hits: [
              { drum: "snare", position: { beat: 1, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
              { drum: "snare", position: { beat: 1, subdivision: 2 }, dynamics: "MF", articulation: null, modifiers: [] },
              { drum: "tom_high", position: { beat: 2, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
              { drum: "tom_high", position: { beat: 2, subdivision: 2 }, dynamics: "MF", articulation: null, modifiers: [] },
              { drum: "tom_mid", position: { beat: 3, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
              { drum: "tom_mid", position: { beat: 3, subdivision: 2 }, dynamics: "F", articulation: null, modifiers: [] },
              { drum: "tom_low", position: { beat: 4, subdivision: 0 }, dynamics: "FF", articulation: null, modifiers: [] },
              { drum: "kick", position: { beat: 4, subdivision: 2 }, dynamics: "FF", articulation: null, modifiers: [] },
              { drum: "crash", position: { beat: 4, subdivision: 0 }, dynamics: "FF", articulation: null, modifiers: [] },
            ],
          },
        ]),
        track("brass", "brass", "brass_section", null, -5, 0.05, [{ target: "mix_bus", amount: 0.35 }], [clip("brass_chorus_hits", 17, 25)]),
        track("guitar", "guitar", "electric", "clean_guitar", -8, -0.25, [{ target: "mix_bus", amount: 0.15 }], [], [{ name: "verse_gtr", time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 16, beat: 4, subdivision: 0 } }, events: [note("B", 3, 0, eighth(), "MP", 62, "palm_mute"), note("B", 3, 0.5, eighth(), "MP", 62, "palm_mute"), note("B", 3, 1, q(), "MF", 78), note("D", 4, 2, eighth(), "MP", 66, "palm_mute"), note("D", 4, 3, q(), "MF", 80)] }]),
        track("keys", "piano", "electric_piano", "lofi_keys", -12, 0.2, [], [], [{ name: "verse_keys", time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 16, beat: 4, subdivision: 0 } }, events: [note("B", 3, 0, q(), "MP", 60), note("D", 4, 1, q(), "MP", 64, "legato"), note("E", 4, 2, h(), "MF", 76), note("F", 4, 4, q(), "MF", 78), note("E", 4, 5, q(), "MP", 64)] }]),
      ],
      patterns: [
        { name: "lead_chorus", instrument: { category: "piano", name: "acoustic_grand" }, preset: "aggressive_lead", engine: null, transpose: null, swing: null, effects: [], color: "#54a0ff", events: leadEvents, hits: [] },
        { name: "surge_melody", instrument: { category: "piano", name: "acoustic_grand" }, preset: "surge_lead", engine: null, transpose: null, swing: null, effects: [], color: "#8f7dff", events: [note("B", 4, 0, eighth(), "F", 102, "staccato"), note("D", 5, 1, eighth(), "F", 104, "staccato"), note("E", 5, 2, q(), "FF", 118, "accent"), note("F", 5, 3, q(1), "FF", 120), note("E", 5, 4.5, eighth(), "MF", 86), note("D", 5, 5, q(), "MF", 84)] , hits: [] },
        { name: "bass_chorus", instrument: { category: "bass", name: "electric_bass" }, preset: "punchy_bass", engine: null, transpose: null, swing: null, effects: [], color: "#01a3a4", events: [note("B", 1, 0, q(), "FF", 112, "accent"), note("B", 1, 1.5, eighth(), "F", 100), note("B", 1, 2, q(), "FF", 112), note("D", 2, 3, q(), "FF", 114, "accent"), note("E", 2, 4, q(), "FF", 116), note("F", 2, 6, q(), "F", 102), note("G", 2, 7, q(), "F", 98)] , hits: [] },
        { name: "brass_chorus_hits", instrument: { category: "brass", name: "brass_section" }, preset: "brass_stab", engine: null, transpose: 0, swing: null, effects: [], color: "#ffcc00", events: [note("B", 4, 0, eighth(), "FF", 120, "marcato"), note("D", 5, 4, eighth(), "FF", 118, "marcato"), note("E", 5, 8, eighth(), "FF", 116, "sforzando"), note("D", 5, 9, eighth(), "F", 104), note("B", 4, 10, q(), "F", 100, "accent")] , hits: [] },
        { name: "drums_verse_beat", instrument: { category: "drums", name: "acoustic_kit" }, preset: "tight_drums", engine: null, transpose: null, swing: 55, effects: [], color: "#ff6b6b", events: [], hits: [{ drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] }, { drum: "hihat", position: { beat: 1, subdivision: 2 }, dynamics: "MP", articulation: "closed", modifiers: [] }, { drum: "snare", position: { beat: 2, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] }, { drum: "kick", position: { beat: 3, subdivision: 2 }, dynamics: "MF", articulation: null, modifiers: [] }, { drum: "snare", position: { beat: 4, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] }] },
        { name: "drums_chorus_beat", instrument: { category: "drums", name: "acoustic_kit" }, preset: "tight_drums", engine: null, transpose: null, swing: 60, effects: [], color: "#ff4444", events: [], hits: [{ drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "FF", articulation: null, modifiers: [] }, { drum: "ride", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] }, { drum: "snare", position: { beat: 2, subdivision: 0 }, dynamics: "FF", articulation: null, modifiers: [] }, { drum: "crash", position: { beat: 4, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] }] },
      ],
      buses: [
        { name: "mix_bus", routes: ["master"], effects: [{ effect_type: "Reverb", params: [["decay", { Time: 2.5 }], ["mix", { Percentage: 35 }]], automation: [] }, { effect_type: "Delay", params: [["feedback", { Percentage: 30 }]], automation: [] }] },
        { name: "drum_bus", routes: ["master"], effects: [{ effect_type: "Compression", params: [["ratio", { String: "4:1" }]], automation: [] }, { effect_type: "Saturator", params: [["drive", { Percentage: 15 }]], automation: [] }] },
      ],
      master: { effects: [{ effect_type: "Eq", params: [["low_shelf", { Db: 1 }]], automation: [] }, { effect_type: "Limiter", params: [["ceiling", { Db: -0.3 }]], automation: [] }] },
      sections: [
        { name: "intro", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } }, label: "Atmospheric intro - pad + arpeggiated synth" },
        { name: "verse", time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 17, beat: 1, subdivision: 0 } }, label: "Full band enters - bass, drums, rhythm guitar" },
        { name: "chorus", time_range: { start: { bar: 17, beat: 1, subdivision: 0 }, end: { bar: 25, beat: 1, subdivision: 0 } }, label: "Chorus - all instruments, brass stab, modulation" },
      ],
    },
    dsl_source: dslSource || '@ocp "0.2.0"\n\n@project "Midnight City"\n  tempo: 110\n  time_signature: 4/4\n',
    file_path: "examples/demo_full_featured.ocp",
    dirty: false,
    track_count: 8,
  };
}

export async function installTauriMock(page, initialSnapshot = createSmokeSnapshot()) {
  await page.addInitScript((initialSnapshot) => {
    const clone = (value) => JSON.parse(JSON.stringify(value));
    let snapshot = clone(initialSnapshot);
    let transport = { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 96 };
    let audition = null;
    let auditionActive = false;
    const stopAudition = () => {
      auditionActive = false;
      audition = { preset: null, status: "idle", ready_ms: null, instances: 0, loads: 0, error: null };
    };
    window.__OCP_INVOKES__ = [];

    const plugins = [
      { id: "ocp.demo_synth", name: "Demo Synth", vendor: "OpenCompose", format: "Internal", category: "Instrument", path: null },
      { id: "ocp.demo_reverb", name: "Demo Reverb", vendor: "OpenCompose", format: "Internal", category: "Effect", path: null },
    ];

    const markDirty = () => {
      snapshot = { ...snapshot, dirty: true };
      return clone(snapshot);
    };

    const durationFromBase = (baseDuration) => ({
      base: baseDuration === "w" || baseDuration === "Whole" ? "Whole" : baseDuration === "h" || baseDuration === "Half" ? "Half" : baseDuration === "8th" || baseDuration === "Eighth" ? "Eighth" : baseDuration === "16th" || baseDuration === "Sixteenth" ? "Sixteenth" : baseDuration === "32nd" || baseDuration === "ThirtySecond" ? "ThirtySecond" : "Quarter",
      dots: 0,
      triplet: false,
      tuplet: null,
      ties: [],
    });

    // Same duration preservation as the real move_* commands.
    const quarterOf = (bb) => (bb.bar - 1) * 4 + (bb.beat - 1) + (bb.subdivision ?? 0) / 4;
    const barBeatOf = (q) => {
      const bar = Math.floor(q / 4) + 1;
      const rest = q - (bar - 1) * 4;
      const beat = Math.floor(rest) + 1;
      return { bar, beat, subdivision: Math.round((rest - Math.floor(rest)) * 4) };
    };
    const retimedRange = (range, args) => {
      const duration = quarterOf(range.end) - quarterOf(range.start);
      const moveQ = (args.startBar - 1) * 4 + (args.startBeat - 1) + (args.startSub ?? 0) / 4;
      return {
        start: { bar: args.startBar, beat: args.startBeat, subdivision: args.startSub ?? 0 },
        end: barBeatOf(moveQ + duration),
      };
    };

    const parseParamValue = (raw) => {
      const numeric = Number(raw);
      if (raw.trim() !== "" && Number.isFinite(numeric)) return { Number: numeric };
      if (raw === "true" || raw === "false") return { Bool: raw === "true" };
      return { String: raw };
    };
    const writePresetParam = (preset, key, raw) => {
      const index = preset.params.findIndex(([name]) => name === key);
      if (raw === null) {
        if (index >= 0) preset.params.splice(index, 1);
      } else if (index >= 0) {
        preset.params[index] = [key, parseParamValue(raw)];
      } else {
        preset.params.push([key, parseParamValue(raw)]);
      }
    };
    const applyPresetBatch = (args) => {
      const preset = snapshot.project.presets.find((item) => item.name === args.name);
      if (!preset) throw new Error(`Preset '${args.name}' not found`);
      if (args.replaceSynth) preset.params = [];
      for (const [key, raw] of args.params) writePresetParam(preset, key, raw);
      return markDirty();
    };

    const effectTarget = (target) => {
      if (target === "master") return snapshot.project.master;
      if (target.startsWith("bus:")) return snapshot.project.buses.find((bus) => bus.name === target.slice(4));
      if (target.startsWith("track:")) return snapshot.project.tracks[Number(target.slice(6))];
      return null;
    };

    const callbacks = new Map();
    let nextCallback = 0;
    // `listen()` from @tauri-apps/api looks the unlisten helper up here; without
    // it every panel that subscribes to an event throws on mount.
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
      transformCallback: (callback) => { callbacks.set(++nextCallback, callback); return nextCallback; },
      unregisterCallback: (id) => callbacks.delete(id),
      invoke: async (cmd, args = {}) => {
        window.__OCP_INVOKES__.push({ cmd, args: clone(args) });

        if (cmd === "plugin:event|listen") return args.handler;
        if (cmd === "plugin:event|unlisten") { callbacks.delete(args.eventId); return; }

        if (cmd === "get_project") return clone(snapshot);
        // Peak payloads are opaque blobs; a fixture may attach one for waveform shots.
        if (cmd === "audio_clip_peaks") return snapshot.__audio_peaks ?? "";
        if (cmd === "get_dsl_source") return snapshot.dsl_source;
        if (cmd === "get_undo_state") {
          return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
        }
        if (cmd === "scan_plugins") return clone(plugins);
        if (cmd === "project_compatibility_report") return {compatible:true,installed_plugin_count:plugins.length,resolved_plugins:[],issues:[]};
        if (cmd === "list_sampler_assets") return [];
        if (cmd === "soundfonts_list") return { dir: "", items: [] };
        if (cmd === "sample_market_providers") return [
          { id: "openverse", label: "Openverse", requires_key: false, kinds: ["loop", "drum_one_shot"], license: "Aggregated per result.", search: true, download: true, available: true, reason: null, default_search: true },
        ];
        if (cmd === "sample_market_search") return {
          query: "snare", providers: [], count: 1, next_offset: null, pages: [{ provider: "openverse", provider_name: "Openverse", count: 1, total: 1, next_offset: null }],
          items: [{
            provider: "openverse", id: "openverse:snare-01", name: "Snare hit 01", kind: "drum_one_shot",
            duration_seconds: 0.42, provider_name: "Openverse", license: { label: "CC0", url: "https://example.test/cc0", requires_attribution: false },
            attribution: "Snare hit 01 by Openverse contributor", source_url: "https://example.test/source/snare-01",
            download_url: "https://example.test/files/snare-01.wav", preview_url: "https://example.test/preview/snare-01.mp3",
          }],
        };
        if (cmd === "list_audio_devices") return [];
        // The proxy notice appears only for a real detected URL; a fixture must
        // not answer with a project snapshot and render `[object Object]`.
        if (cmd === "detect_local_proxy") return null;
        if (cmd === "get_audio_config") return { device_name: null, sample_rate: 44100, buffer_size: 512 };

        if (cmd === "set_tempo") {
          snapshot.project.properties.tempo = args.tempo;
          return markDirty();
        }
        if (cmd === "update_dsl") {
          snapshot.dsl_source = args.source;
          return markDirty();
        }
        if (cmd === "update_track_mix") {
          const mix = snapshot.project.tracks[args.trackIndex]?.mix;
          if (mix) {
            if (args.volumeDb !== undefined && args.volumeDb !== null) mix.volume_db = args.volumeDb;
            if (args.pan !== undefined && args.pan !== null) mix.pan = args.pan;
          }
          return markDirty();
        }
        if (cmd === "set_track_mute") {
          snapshot.project.tracks[args.trackIndex].mix.mute = args.mute;
          return markDirty();
        }
        if (cmd === "set_track_solo") {
          snapshot.project.tracks[args.trackIndex].mix.solo = args.solo;
          return markDirty();
        }
        if (cmd === "add_track") {
          snapshot.project.tracks.push({
            name: args.name,
            instrument: { category: args.instrumentCategory, name: args.instrumentName },
            preset: null,
            engine: null,
            mix: { volume_db: 0, pan: 0, sends: [], mute: false, solo: false },
            effects: [],
            phrases: [],
            patterns: [],
            inline_patterns: [],
            clips: [],
            automation: [],
            engine_params: null,
          });
          snapshot.track_count = snapshot.project.tracks.length;
          return markDirty();
        }
        if (cmd === "rename_track") {
          snapshot.project.tracks[args.trackIndex].name = args.newName;
          return markDirty();
        }
        if (cmd === "set_audio_clip_envelope") {
          const next = clone(snapshot);
          const clip = next.project.properties.audio_clips?.[args.clipIndex];
          if (clip) {
            if (args.gainDb != null) clip.gain_db = args.gainDb;
            if (args.fadeInSeconds != null) clip.fade_in_seconds = args.fadeInSeconds;
            if (args.fadeOutSeconds != null) clip.fade_out_seconds = args.fadeOutSeconds;
            if (args.crossfadeSeconds != null) clip.crossfade_seconds = args.crossfadeSeconds;
            if (args.gainEnvelope != null) clip.gain_envelope = clone(args.gainEnvelope);
          }
          snapshot = { ...next, dirty: true };
          return clone(snapshot);
        }
        if (cmd === "set_track_preset") {
          const track = snapshot.project.tracks[args.trackIndex];
          if (track) track.preset = args.presetName;
          return markDirty();
        }
        if (cmd === "set_preset_param") {
          const preset = snapshot.project.presets.find((item) => item.name === args.name);
          if (!preset) throw new Error(`Preset '${args.name}' not found`);
          writePresetParam(preset, args.paramName, args.paramValue ?? null);
          return markDirty();
        }
        if (cmd === "set_preset_params") return applyPresetBatch(args);
        if (cmd === "edit_performance_event") {
          const track = snapshot.project.tracks[args.trackIndex];
          if (track?.name !== args.trackName || JSON.stringify(track.performance?.events[args.eventIndex]) !== JSON.stringify(args.expected)) {
            throw new Error("Performance event changed; refresh before editing");
          }
          track.performance.events[args.eventIndex] = clone(args.replacement);
          return markDirty();
        }
        if (cmd === "add_pattern") {
          snapshot.project.patterns.push({
            name: args.name,
            instrument: { category: args.instrumentCategory, name: args.instrumentName },
            preset: args.preset,
            engine: null,
            transpose: null,
            swing: null,
            effects: [],
            color: null,
            events: [],
            hits: [],
          });
          return markDirty();
        }
        if (cmd === "set_pattern_transpose") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          if (pattern) pattern.transpose = args.transpose;
          return markDirty();
        }
        if (cmd === "set_pattern_swing") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          if (pattern) pattern.swing = args.swing;
          return markDirty();
        }
        if (cmd === "set_pattern_color") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          if (pattern) pattern.color = args.color || null;
          return markDirty();
        }
        if (cmd === "insert_phrase" || cmd === "insert_inline_pattern") {
          const track = snapshot.project.tracks[args.trackIndex];
          const block = cmd === "insert_phrase" ? args.phrase : args.pattern;
          const toQuarter = (bb) => (bb.bar - 1) * 4 + (bb.beat - 1) + (bb.subdivision ?? 0) / 4;
          const toBarBeat = (q) => {
            const bar = Math.floor(q / 4) + 1;
            const rest = q - (bar - 1) * 4;
            const beat = Math.floor(rest) + 1;
            return { bar, beat, subdivision: Math.round((rest - Math.floor(rest)) * 4) };
          };
          const duration = toQuarter(block.time_range.end) - toQuarter(block.time_range.start);
          const startQuarter = (args.startBar - 1) * 4 + (args.startBeat - 1) + (args.startSub ?? 0) / 4;
          block.time_range = {
            start: { bar: args.startBar, beat: args.startBeat, subdivision: args.startSub ?? 0 },
            end: toBarBeat(startQuarter + duration),
          };
          const blocks = cmd === "insert_phrase" ? track.phrases : track.patterns;
          block.name = `${block.name} ${blocks.length + 1}`;
          blocks.push(block);
          return markDirty();
        }
        if (cmd === "add_clip") {
          snapshot.project.tracks[args.trackIndex].clips.push({
            pattern: args.patternName,
            time_range: {
              start: { bar: args.startBar, beat: args.startBeat, subdivision: args.startSub },
              end: { bar: args.endBar, beat: args.endBeat, subdivision: args.endSub },
            },
          });
          return markDirty();
        }
        if (cmd === "move_clip") {
          const clip = snapshot.project.tracks[args.trackIndex].clips[args.clipIndex];
          if (clip) clip.time_range = retimedRange(clip.time_range, args);
          return markDirty();
        }
        if (cmd === "remove_clip") {
          snapshot.project.tracks[args.trackIndex].clips.splice(args.clipIndex, 1);
          return markDirty();
        }
        if (cmd === "resize_clip" || cmd === "extend_phrase_range") {
          const end = { bar: args.endBar, beat: args.endBeat, subdivision: args.endSub ?? 0 };
          const track = snapshot.project.tracks[args.trackIndex];
          const block = cmd === "resize_clip"
            ? track.clips[args.clipIndex]
            : track.phrases[args.phraseIndex];
          if (block) block.time_range.end = end;
          return markDirty();
        }
        if (cmd === "move_phrase" || cmd === "move_inline_pattern") {
          const key = cmd === "move_phrase" ? "phrases" : "patterns";
          const block = snapshot.project.tracks[args.trackIndex][key][args[cmd === "move_phrase" ? "phraseIndex" : "patternIndex"]];
          if (block) block.time_range = retimedRange(block.time_range, args);
          return markDirty();
        }
        if (cmd === "move_phrase_to_track" || cmd === "move_inline_pattern_to_track") {
          const key = cmd === "move_phrase_to_track" ? "phrases" : "patterns";
          const index = args[cmd === "move_phrase_to_track" ? "phraseIndex" : "patternIndex"];
          const block = snapshot.project.tracks[args.fromTrackIndex][key][index];
          if (block) {
            block.time_range = retimedRange(block.time_range, args);
            snapshot.project.tracks[args.fromTrackIndex][key].splice(index, 1);
            snapshot.project.tracks[args.toTrackIndex][key].push(block);
          }
          return markDirty();
        }

        if (cmd === "add_pattern_note") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          if (pattern) {
            pattern.events.push({
              start_beat: args.startBeat,
              explicit_position: true,
              pitch: { note: args.noteName, accidental: args.accidental === "#" ? "Sharp" : args.accidental === "b" ? "Flat" : "Natural", octave: args.octave },
              duration: durationFromBase(args.baseDuration),
              dynamics: "MF",
              velocity: 80,
              articulation: null,
              vibrato: null,
              expression: null,
              pitch_bend: null,
              muted: false,
              grace: false,
              text: null,
              modifiers: [],
            });
          }
          return markDirty();
        }
        if (cmd === "add_effect") {
          const target = effectTarget(args.target);
          const effectType = args.effectType?.startsWith("plugin:") ? { Plugin: args.effectType.slice(7) } : args.effectType === "compressor" ? "Compression" : args.effectType === "stereo_width" ? "StereoWidth" : args.effectType ? args.effectType[0].toUpperCase() + args.effectType.slice(1) : "Reverb";
          target?.effects.push({ effect_type: effectType, params: [["mix", { Percentage: 35 }]], automation: [] });
          return markDirty();
        }
        if (cmd === "remove_effect") {
          const target = effectTarget(args.target);
          target?.effects.splice(args.effectIndex, 1);
          return markDirty();
        }
        if (cmd === "set_effect_param") {
          const target = effectTarget(args.target);
          const effect = target?.effects[args.effectIndex];
          if (effect) {
            const existing = effect.params.find(([name]) => name === args.paramName);
            const value = Number(args.paramValue.replace(/[^0-9.-]/g, ""));
            const paramValue = Number.isFinite(value) ? { Number: value } : { String: args.paramValue };
            if (existing) existing[1] = paramValue;
            else effect.params.push([args.paramName, paramValue]);
          }
          return markDirty();
        }
        if (cmd === "remove_pattern_note") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          pattern?.events.splice(args.noteIndex, 1);
          return markDirty();
        }
        if (cmd === "move_pattern_note") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          const note = pattern?.events[args.noteIndex];
          if (note) note.start_beat = args.newStartBeat;
          return markDirty();
        }
        if (cmd === "set_pattern_note_duration") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          const note = pattern?.events[args.noteIndex];
          if (note) {
            note.duration = {
              base: args.baseDuration === "w" ? "Whole" : args.baseDuration === "h" ? "Half" : args.baseDuration === "8th" ? "Eighth" : args.baseDuration === "16th" ? "Sixteenth" : args.baseDuration === "32nd" ? "ThirtySecond" : "Quarter",
              dots: args.dots,
              triplet: args.triplet,
              tuplet: null,
              ties: [],
            };
          }
          return markDirty();
        }
        if (cmd === "set_pattern_note_velocity") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          const note = pattern?.events[args.noteIndex];
          if (note) note.velocity = args.velocity;
          return markDirty();
        }
        if (cmd === "set_pattern_note_muted") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          const note = pattern?.events[args.noteIndex];
          if (note) note.muted = args.muted;
          return markDirty();
        }
        if (cmd === "set_pattern_note_grace") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          const note = pattern?.events[args.noteIndex];
          if (note) note.grace = args.grace;
          return markDirty();
        }
        if (cmd === "add_pattern_hit") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          pattern?.hits.push({ drum: args.drum, position: { beat: args.beat, subdivision: args.subdivision }, dynamics: args.dynamics ?? "MF", articulation: null, modifiers: [] });
          return markDirty();
        }
        if (cmd === "remove_pattern_hit") {
          const pattern = snapshot.project.patterns.find((pattern) => pattern.name === args.patternName);
          pattern?.hits.splice(args.hitIndex, 1);
          return markDirty();
        }

        if (cmd === "transport_play") {
          stopAudition();
          transport = { playing: true, position_beats: transport.position_beats, position_seconds: 0.5, duration_seconds: 96, did_render: true, warnings: [] };
          return clone(transport);
        }
        if (cmd === "transport_pause") {
          transport = { ...transport, playing: false };
          return clone(transport);
        }
        if (cmd === "transport_stop") {
          stopAudition();
          transport = { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 96 };
          return clone(transport);
        }
        if (cmd === "transport_seek") {
          transport = { ...transport, position_beats: args.beat, position_seconds: args.beat * 0.5 };
          return clone(transport);
        }
        if (cmd === "get_transport_state") {
          return { ...clone(transport), audition: audition ? clone(audition) : null, audition_active: auditionActive };
        }
        if (cmd === "transport_mute" || cmd === "transport_unmute") return null;
        if (cmd === "preview_track") {
          // Like the backend, a track preview is a fresh pass from frame 0.
          stopAudition();
          transport = { playing: true, position_beats: 0, position_seconds: 0, duration_seconds: 96, did_render: true, warnings: [] };
          return clone(transport);
        }
        if (cmd === "preview_preset") {
          auditionActive = true;
          audition = { preset: args.presetName, status: "ready", ready_ms: 12, instances: 0, loads: 0, error: null };
          return null;
        }
        if (cmd === "preview_pattern") {
          auditionActive = true;
          audition = { preset: null, status: "ready", ready_ms: 8, instances: 0, loads: 0, error: null };
          return null;
        }
        if (cmd === "stop_audition") {
          stopAudition();
          return null;
        }

        return clone(snapshot);
      },
    };
  }, initialSnapshot);
}

export async function invokeCommands(page) {
  return page.evaluate(() => window.__OCP_INVOKES__.map((entry) => entry.cmd));
}

export async function lastInvoke(page, cmd) {
  return page.evaluate((command) => {
    const matches = window.__OCP_INVOKES__.filter((entry) => entry.cmd === command);
    return matches.at(-1) ?? null;
  }, cmd);
}
