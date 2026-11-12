import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invoke } from "@tauri-apps/api/core";
import { AiToolResult, autoOpenResult } from "../components/AiToolResult";
import { i18n } from "../stores/i18n";
import { rememberToolResult } from "../ai/agent";
import { store } from "../stores/project";

function mount(tool: string, value: unknown) {
  return render(() => <AiToolResult tool={tool} content={JSON.stringify(value)} />);
}

function testWav() {
  const bytes = new Uint8Array(60);
  const view = new DataView(bytes.buffer);
  for (const [offset, tag] of [[0, "RIFF"], [8, "WAVE"], [12, "fmt "], [36, "data"]] as const) {
    bytes.set(new TextEncoder().encode(tag), offset);
  }
  view.setUint32(4, 52, true);
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, 4, true);
  view.setUint32(28, 8, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  view.setUint32(40, 16, true);
  view.setInt16(44, 16384, true);
  return btoa(String.fromCharCode(...bytes));
}

afterEach(() => i18n.setLocale("en"));

describe("rich tool results", () => {
  it("shows each comparison passage in its own source time", () => {
    const view = mount("audio_compare", {
      reference: { path: "song.wav" }, candidate: { path: "take.wav" },
      window: { reference_start_s: 40, reference_end_s: 42, candidate_start_s: 0, candidate_end_s: 2 },
    });
    expect(view.getByText("song.wav · 40–42 s")).toBeTruthy();
    expect(view.getByText("take.wav · 0–2 s")).toBeTruthy();
  });

  it("shows the measured listening pair without zero-valued analysis placeholders", () => {
    const view = mount("audio_compare", {
      listening_only: true,
      reference: { path: "song.wav" }, candidate: { path: "take.wav" },
      listening: { status: "matched", delta_lu: 0.012, reference_lufs: -23, candidate_lufs: -22.988 },
    });
    expect(view.getByText("0.012 LU")).toBeTruthy();
    expect(view.getByText("song.wav")).toBeTruthy();
    expect(view.queryByText("0 ms")).toBeNull();
  });

  it("requests the independent source windows from comparison cards", async () => {
    const ipc = vi.mocked(invoke);
    ipc.mockClear();
    const excerpt = { mime: "audio/wav", clip_duration_s: 2, truncated: false, base64: testWav() };
    ipc.mockResolvedValueOnce({ ...excerpt, start_s: 40, end_s: 42 });
    ipc.mockResolvedValueOnce({ ...excerpt, start_s: 1, end_s: 3 });
    const view = mount("audio_compare", {
      reference: { path: "song.wav" }, candidate: { path: "take.wav" },
      window: { reference_start_s: 40, reference_end_s: 42, candidate_start_s: 1, candidate_end_s: 3 },
    });
    expect(ipc).not.toHaveBeenCalled();
    for (const load of view.getAllByRole("button", { name: "Load audio preview" })) fireEvent.click(load);
    await waitFor(() => expect(view.getByText("Source 40.000–42.000 s")).toBeTruthy());
    expect(view.getByText("Source 1.000–3.000 s")).toBeTruthy();
    expect(ipc.mock.calls).toEqual([
      ["audio_file_preview", { path: "song.wav", startSeconds: 40, durationSeconds: 2 }],
      ["audio_file_preview", { path: "take.wav", startSeconds: 1, durationSeconds: 2 }],
    ]);
  });

  it("shows playback verification separately from the comparison metrics", () => {
    const view = mount("audio_compare", {
      reference: { path: "a.wav" }, candidate: { path: "b.wav" },
      listening: { status: "matched", delta_lu: 0.012, reference_lufs: -18, candidate_lufs: -17.988 },
    });
    expect(view.getByText("Listening comparison")).toBeTruthy();
    expect(view.getByText("Playback levels matched")).toBeTruthy();
    expect(view.getByText("0.012 LU")).toBeTruthy();
  });

  it("renders a track list as a table and keeps the raw result one click away", () => {
    i18n.setLocale("en");
    const view = mount("tracks_list", {
      count: 2,
      tracks: [
        { index: 0, name: "kick", preset: "Kit", instrument: { category: "drums", name: "kit" }, volume_db: -3, pan: 0, mute: false, solo: false, output: "master" },
        { index: 1, name: "bass", preset: null, instrument: { category: "synth", name: "bass" }, volume_db: -6, pan: -0.2, mute: true, solo: false, output: "bus" },
      ],
    });
    expect(view.container.querySelectorAll(".ai-tool-table tbody tr")).toHaveLength(2);
    expect(view.getByText("kick")).toBeTruthy();
    (view.container.querySelector(".ai-tool-table tbody tr") as HTMLElement).click();
    expect(store.selectedTrackIndex()).toBe(0);
    fireEvent.click(view.container.querySelector(".ai-tool-raw-toggle") as HTMLElement);
    expect(view.container.querySelector(".ai-tool-table")).toBeNull();
    expect(view.container.querySelector("pre")?.textContent).toContain('"tracks"');
  });

  it("draws sections on a timeline", () => {
    const view = mount("sections_list", {
      count: 2,
      sections: [
        { index: 0, name: "intro", start: "1:1", end: "9:1", bars: 8, repeat: 1 },
        { index: 1, name: "chorus", start: "9:1", end: "17:1", bars: 8, repeat: 2 },
      ],
      rendered_end: "33:1",
      rendered_bars: 32,
      rendered_seconds: 64,
    });
    expect(view.container.querySelectorAll(".ai-tool-timeline-span")).toHaveLength(2);
    expect(view.getAllByText("chorus").length).toBeGreaterThan(0);
  });

  it("shows analysis metrics, a spectrum, and the segment timeline", () => {
    const view = mount("audio_analyze", {
      path: "/tmp/mix.wav", sample_rate: 48000, channels: 2, duration_s: 4,
      level: { peak_dbfs: -1, rms_dbfs: -12, crest_db: 11, integrated_lufs: -14 },
      true_peak_dbfs: -0.8,
      octave: { centers_hz: [31.5, 63, 125, 250], energy_db: [-40, -30, -20, -10] },
      spectral_peaks: { peaks: [{ frequency_hz: 440, level_db: -12, relative_db: 0 }] },
      stereo: { correlation: 0.8, mid_side_ratio_db: 6 },
      sections: [{ start_s: 0, end_s: 2, peak_dbfs: -1, rms_dbfs: -12 }],
      silence: [], warnings: [],
    });
    expect(view.container.querySelectorAll(".ai-tool-stat").length).toBeGreaterThanOrEqual(10);
    expect(view.container.querySelectorAll(".ai-tool-spectrum-bar")).toHaveLength(4);
    expect(view.container.querySelectorAll(".ai-tool-timeline-span")).toHaveLength(1);
    expect(view.getByText("440")).toBeTruthy();
  });

  it("summarizes the aligned stem import and retains its warnings", () => {
    const view = mount("import_aligned_stems", {
      revision: 6, stem_count: 2, timestamped_stem_count: 2, midi_track_count: 1,
      midi: { warnings: ["Skipped one MIDI controller."] },
      project_file_error: "The project file could not be written.",
    });
    expect(view.getByText("Stems")).toBeTruthy();
    expect(view.getByText("MIDI tracks (muted)")).toBeTruthy();
    expect(view.getByText("2")).toBeTruthy();
    expect(view.getByText("1")).toBeTruthy();
    expect(view.getByText("Broadcast WAV start timestamps agree.")).toBeTruthy();
    expect(view.getByText("Skipped one MIDI controller.")).toBeTruthy();
    expect(view.getByText("The project file could not be written.")).toBeTruthy();
  });

  it("shows pulse evidence and its source anchors without assuming a downbeat", () => {
    const view = mount("audio_analyze", {
      path: "song.wav", downbeat_confirmed: false, time_signature: null,
      beat_grid: {
        start_s: 40, end_s: 50, attack_count: 20,
        candidates: [{ tempo_bpm: 120, score: 0.8, pulse_origin_s: 40.25, anchors_s: [40.25, 45.25] }],
      },
    });
    expect(view.getByText("40.000–50.000 s")).toBeTruthy();
    expect(view.getByText("120 BPM")).toBeTruthy();
    const anchors = view.getByRole("img", { name: "Observed attack times" });
    expect(anchors.querySelector("line")?.getAttribute("x1")).toBe("2.5");
    expect(view.getByText("Confirm the pulse, meter, and downbeat before applying a grid.")).toBeTruthy();
    expect(view.queryByText("4/4")).toBeNull();
  });

  it("shows raw levels for a short file window", () => {
    const view = mount("audio_analyze", {
      path: "warp.wav",
      level_window: { start_s: 1.96, end_s: 2 },
      level: { peak_dbfs: -6, rms_dbfs: -9.5, crest_db: 3.5 },
    });
    expect(view.getByText("1.960–2.000 s")).toBeTruthy();
    expect(view.getByText("-9.5 dBFS")).toBeTruthy();
    expect(view.getByText("-6 dBFS")).toBeTruthy();
    expect(view.queryByRole("button", { name: "Load audio preview" })).toBeNull();
  });

  it("shows close file attacks with their timing resolution and paused preview", () => {
    const ipc = vi.mocked(invoke);
    ipc.mockClear();
    const view = mount("audio_analyze", {
      path: "processed.wav",
      transients: {
        start_s: 0.25, end_s: 0.5, resolution_seconds: 64 / 48000, effective_gap_ms: 5.3333,
        attacks: [{ file_seconds: 0.3618, strength: 0.8 }, { file_seconds: 0.3499, strength: 0.6 }],
      },
    });
    expect(view.getByText("0.250–0.500 s")).toBeTruthy();
    expect(view.getByText("1.33 ms")).toBeTruthy();
    expect(view.getByText("5.33 ms")).toBeTruthy();
    expect(view.getByRole("columnheader", { name: "File time (s)" })).toBeTruthy();
    expect(view.getByText("0.3618")).toBeTruthy();
    expect(view.getByText("0.3499")).toBeTruthy();
    expect(view.getByRole("button", { name: "Load audio preview" })).toBeTruthy();
    expect(ipc).not.toHaveBeenCalled();
  });

  it("draws note events on a piano roll", () => {
    const view = mount("analyze_notes", {
      path: "/tmp/vocal.wav", start_s: 0, end_s: 2, tempo_bpm: 120, time_signature: "4/4",
      notes: [
        { name: "C4", midi: 60, start_s: 0, end_s: 0.5, confidence: 0.9 },
        { name: "E4", midi: 64, start_s: 0.5, end_s: 1, confidence: 0.8 },
      ],
      polyphonic: false, warnings: [],
    });
    expect(view.container.querySelectorAll(".ai-tool-roll-note")).toHaveLength(2);
    expect(view.container.querySelector(".ai-tool-roll")?.getAttribute("aria-label")).toContain("2");
  });

  it("lets a long note-analysis card preview later in its analyzed interval", async () => {
    const ipc = vi.mocked(invoke);
    ipc.mockClear();
    ipc.mockResolvedValueOnce({
      mime: "audio/wav", start_s: 68, end_s: 70, clip_duration_s: 2, truncated: false, base64: testWav(),
    });
    const view = mount("analyze_notes", { path: "song.wav", start_s: 12, end_s: 70, notes: [] });
    const start = view.getByRole("spinbutton", { name: "Source start (s)" });
    expect(start).toHaveValue(12);
    fireEvent.input(start, { target: { value: "68" } });
    fireEvent.click(view.getByRole("button", { name: "Load audio preview" }));
    await waitFor(() => expect(view.getByText("Source 68.000–70.000 s")).toBeTruthy());
    expect(ipc).toHaveBeenCalledWith("audio_file_preview", {
      path: "song.wav", startSeconds: 68, durationSeconds: 2,
    });
  });

  it("renders a diff result instead of raw JSON", () => {
    const view = mount("project_edit", {
      snapshot: { track_count: 2, revision: 7, dirty: true, file_path: "/tmp/a.ocp" },
      diff: { hunks: [{ lines: [{ kind: "Removed", text: "gain 0" }, { kind: "Added", text: "gain -3" }] }] },
    });
    expect(view.container.querySelectorAll(".ai-diff-line.removed")).toHaveLength(1);
    expect(view.container.querySelectorAll(".ai-diff-line.added")).toHaveLength(1);
  });

  it("falls back to an automatic table for an unknown tool", () => {
    const view = mount("some_new_tool", { items: [{ name: "a", value: 1 }, { name: "b", value: 2 }] });
    expect(view.container.querySelectorAll(".ai-tool-table tbody tr")).toHaveLength(2);
  });

  it("recovers a bounded snapshot result from this session's full copy", () => {
    const full = {
      project: {
        name: "Session", properties: { tempo: 120, time_signature: { numerator: 4, denominator: 4 } },
        tracks: [{ name: "pad", preset: "Warm", instrument: { category: "synth", name: "pad" }, mix: { volume_db: -2, pan: 0 } }],
        presets: [], sections: [],
      },
      dsl_source: '@project "Session"\n tempo: 120\n', file_path: "/tmp/session.ocp",
      dirty: true, track_count: 1, revision: 9, duration_seconds: 12,
    };
    rememberToolResult("call-snapshot", JSON.stringify(full));
    const view = mount("tracks_add", {
      result_truncated: true, tool_call_id: "call-snapshot", original_characters: 12000,
      preview: '{"project":{"tracks":[{"name":"pad"', tool_failed: false,
    });
    expect(view.container.querySelector(".ai-tool-table tbody tr")).toBeTruthy();
    expect(view.getByText("pad")).toBeTruthy();
  });

  it("keeps a bounded result's preview when the full copy is gone", () => {
    const view = mount("tracks_list", {
      result_truncated: true, tool_call_id: "evicted", original_characters: 12000,
      preview: '{"tracks":[{"name":"pad"', tool_failed: false,
    });
    expect(view.getByText(i18n.t("ai.tool.truncated"))).toBeTruthy();
    expect(view.container.querySelector("pre")?.textContent).toContain('"tracks"');
  });

  it("shows snapshot metadata without an internal serialization notice", () => {
    const view = mount("tracks_mix", { project_ast_omitted: true, track_count: 3, revision: 11, dirty: true, duration_seconds: 20 });
    expect(view.container.querySelector(".ai-tool-warning")).toBeNull();
    expect(view.container.querySelectorAll(".ai-tool-stat")).toHaveLength(4);
    fireEvent.click(view.getByRole("button", { name: "Raw JSON" }));
    expect(view.container.querySelector("pre")?.textContent).toContain('"project_ast_omitted": true');
  });

  it("recovers a bounded top-level array from this session's full copy", () => {
    rememberToolResult("call-params", JSON.stringify([{ id: 0, name: "Gain", current: -6, display: "-6 dB" }]));
    const view = mount("plugins_params", {
      result_truncated: true, tool_call_id: "call-params", original_characters: 12000,
      preview: '[{"id":0,"name":"Gain"', tool_failed: false,
    });
    expect(view.container.querySelectorAll(".ai-tool-table tbody tr")).toHaveLength(1);
    expect(view.getByText("Gain")).toBeTruthy();
  });

  it("renders a top-level array as a table", () => {
    const view = mount("plugins_params", [{ id: 0, name: "Gain", min: -60, max: 12, current: -6, display: "-6 dB" }]);
    expect(view.container.querySelectorAll(".ai-tool-table tbody tr")).toHaveLength(1);
    expect(view.getByText("Gain")).toBeTruthy();
  });

  it("formats preset parameter pairs without dumping JSON", () => {
    const view = mount("presets_list", {
      presets: [{ name: "Warm", source: "synth", params: [["cutoff", { Number: 1200 }], ["resonance", { Number: 0.2 }]] }],
      soundfonts: [], soundfonts_dir: "/tmp",
    });
    expect(view.getByText("Warm")).toBeTruthy();
    expect(view.container.querySelector(".ai-tool-table")?.textContent).not.toContain("cutoff");
  });

  it("renders a project snapshot as a dashboard with a track table", () => {
    const view = mount("project_get", {
      project: {
        name: "Song", properties: { tempo: 96, time_signature: { numerator: 3, denominator: 4 } },
        tracks: [{ name: "keys", preset: "Piano", instrument: { category: "piano", name: "grand" }, mix: { volume_db: -4, pan: 0.2, mute: false, solo: false, output: "master" } }],
        presets: [{ name: "Piano" }], sections: [{ name: "verse", time_range: { start: { bar: 1, beat: 1 }, end: { bar: 9, beat: 1 } }, repeat: 2 }],
      },
      dsl_source: '@project "Song"', file_path: "/tmp/song.ocp", dirty: false,
      track_count: 1, revision: 3, duration_seconds: 30,
    });
    expect(view.getByText("Song")).toBeTruthy();
    expect(view.getByText("96 BPM")).toBeTruthy();
    expect(view.container.querySelector(".ai-tool-table tbody tr")).toBeTruthy();
    expect(view.container.querySelectorAll(".ai-tool-timeline-span")).toHaveLength(1);
  });

  it("opens a listening delivery by default but leaves renders folded", () => {
    expect(autoOpenResult("audition_range")).toBe(true);
    expect(autoOpenResult("render_range")).toBe(false);
    expect(autoOpenResult("render_offline")).toBe(false);
  });

  it("plays an audition clip with a custom player and its waveform", () => {
    i18n.setLocale("en");
    const view = mount("audition_range", {
      path: "/tmp/audition.wav", start_s: 0, end_s: 4, duration_s: 4, tracks: [0],
      audio: { mime: "audio/wav", base64: testWav() },
    });
    expect(view.container.querySelector(".ai-tool-waveform")).toBeTruthy();
    const audio = view.container.querySelector("audio");
    expect(audio).toBeTruthy();
    expect(audio!.hasAttribute("controls")).toBe(false);
    expect(view.container.querySelector(".ai-tool-audio-toggle")?.getAttribute("aria-label")).toBe("Play");
    expect(view.container.querySelector(".ai-tool-audio-seek")?.getAttribute("type")).toBe("range");
    expect(view.container.querySelector(".ai-tool-audio-seek")?.getAttribute("max")).toBe("2");
    expect(view.container.querySelector(".ai-tool-waveform path")?.getAttribute("d")).toContain("M0.5,-0.500");
  });

  it("applies the latest seek chosen before audio metadata is ready", () => {
    const view = mount("audition_range", {
      path: "/tmp/audition.wav", start_s: 0, end_s: 4, duration_s: 4, tracks: [0],
      audio: { mime: "audio/wav", base64: testWav() },
    });
    const audio = view.container.querySelector("audio")!;
    const slider = view.container.querySelector<HTMLInputElement>(".ai-tool-audio-seek")!;
    let ready = 0;
    Object.defineProperty(audio, "readyState", { get: () => ready });
    Object.defineProperty(audio, "duration", { value: 1.5 });
    fireEvent.input(slider, { target: { value: "1.25" } });
    fireEvent.input(slider, { target: { value: "1.9" } });
    fireEvent.timeUpdate(audio);
    expect(audio.currentTime).toBe(0);
    expect(slider.value).toBe("1.9");
    ready = 1;
    fireEvent.loadedMetadata(audio);
    expect(audio.currentTime).toBe(1.5);
    expect(slider.value).toBe("1.5");
  });

  it("loads a path-only excerpt on request, supports retry and uses the excerpt clock", async () => {
    const ipc = vi.mocked(invoke);
    ipc.mockClear();
    ipc.mockRejectedValueOnce(new Error("missing")).mockResolvedValueOnce({
      mime: "audio/wav", start_s: 0, end_s: 2, clip_duration_s: 2, truncated: true, base64: testWav(),
    });
    const view = mount("render_range", { path: "/tmp/mix.wav", start_s: 120, duration_s: 120 });
    expect(ipc).not.toHaveBeenCalled();
    expect(view.container.querySelector("audio, .ai-tool-waveform")).toBeNull();

    fireEvent.click(view.getByRole("button", { name: "Load audio preview" }));
    await waitFor(() => expect(view.getByRole("status")).toHaveTextContent("Audio preview unavailable"));
    fireEvent.click(view.getByRole("button", { name: "Load audio preview" }));
    await waitFor(() => expect(view.getByText("Source 0.000–2.000 s")).toBeTruthy());
    expect(ipc.mock.calls).toEqual([
      ["audio_file_preview", { path: "/tmp/mix.wav", startSeconds: 0, durationSeconds: undefined }],
      ["audio_file_preview", { path: "/tmp/mix.wav", startSeconds: 0, durationSeconds: undefined }],
    ]);
    expect(view.container.querySelector(".ai-tool-audio-seek")?.getAttribute("max")).toBe("2");
    expect(view.container.querySelector(".ai-tool-waveform path")?.getAttribute("d")).toContain("M0.5,-0.500");
    const firstPlayer = view.container.querySelector("audio");
    ipc.mockResolvedValueOnce({
      mime: "audio/wav", start_s: 75.5, end_s: 77.5, clip_duration_s: 2, truncated: true, base64: testWav(),
    });
    fireEvent.input(view.getByRole("spinbutton", { name: "Source start (s)" }), { target: { value: "75.5" } });
    fireEvent.click(view.getByRole("button", { name: "Load audio preview" }));
    await waitFor(() => expect(view.getByText("Source 75.500–77.500 s")).toBeTruthy());
    expect(ipc).toHaveBeenLastCalledWith("audio_file_preview", {
      path: "/tmp/mix.wav", startSeconds: 75.5, durationSeconds: undefined,
    });
    expect(view.container.querySelector("audio")).not.toBe(firstPlayer);
    expect(view.container.querySelector("audio")?.paused).toBe(true);
  });

  it("shows a refused edit's errors instead of a diff", () => {
    const view = mount("project_edit", { ok: false, valid: false, errors: ["track 'lead' does not exist"], parsed_dsl: null, error: "track 'lead' does not exist" });
    expect(view.getByText("track 'lead' does not exist")).toBeTruthy();
    expect(view.container.querySelector(".ai-diff")).toBeNull();
  });

  it("keeps malformed results as the existing code view", () => {
    const view = render(() => <AiToolResult tool="tracks_list" content={"not json"} />);
    expect(view.getByText("Text")).toBeTruthy();
  });
});


describe("localized result presentation", () => {
  it.each([
    ["en", "Playing", "Paused", "No"],
    ["zh-Hans", "播放中", "已暂停", "否"],
    ["zh-Hant", "播放中", "已暫停", "否"],
    ["ja", "再生中", "一時停止中", "いいえ"],
    ["es", "En reproducción", "En pausa", "No"],
  ] as const)("renders transport labels in %s without rewriting the raw result", (locale, playing, paused, no) => {
    i18n.setLocale(locale);
    const view = mount("transport_state", { status: "paused", playing: false });
    const stat = [...view.container.querySelectorAll(".ai-tool-stat")].find(row =>
      row.querySelector(".ai-tool-stat-label")?.textContent === playing,
    );
    expect(stat?.querySelector(".ai-tool-stat-value")).toHaveTextContent(no);
    expect(view.getByText(paused)).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: i18n.t("ai.tool.raw") }));
    expect(view.container.querySelector("pre")?.textContent).toContain('"status": "paused"');
    expect(view.container.querySelector("pre")?.textContent).toContain('"playing": false');
  });

  it("updates track headings on locale change without translating preset or track names", () => {
    const view = mount("tracks_list", { tracks: [{ name: "Track One", preset: "Warm Lead", volume_db: -3 }] });
    expect(view.getByRole("columnheader", { name: "Volume (dB)" })).toBeTruthy();
    i18n.setLocale("zh-Hans");
    expect(view.getByRole("columnheader", { name: "轨道" })).toBeTruthy();
    expect(view.getByRole("columnheader", { name: "音量（dB）" })).toBeTruthy();
    expect(view.getByText("Track One")).toBeTruthy();
    expect(view.getByText("Warm Lead")).toBeTruthy();
  });

  it("localizes render warnings and reuse state without rewriting the audio path", () => {
    i18n.setLocale("zh-Hans");
    const view = mount("render_range", {
      path: "/tmp/Warm Lead.wav", start_s: 0, end_s: 5, duration_s: 4,
      cache_hit: true, truncated: true,
      audio: { mime: "audio/wav", base64: "AAAA" },
    });
    expect(view.getByText("片段短于请求的范围。")).toBeTruthy();
    expect(view.getByText("已复用")).toBeTruthy();
    expect(view.getByText("/tmp/Warm Lead.wav")).toBeTruthy();
  });

  it("keeps unknown tool fields and status text readable", () => {
    i18n.setLocale("zh-Hans");
    const view = mount("third_party_tool", { status: "custom_state", third_party_parameter: "Keep this value" });
    expect(view.getByText("third party parameter")).toBeTruthy();
    expect(view.getByText("Keep this value")).toBeTruthy();
    expect(view.getAllByText("custom_state").length).toBeGreaterThan(0);
  });
});
