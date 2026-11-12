import type { ProjectSnapshot } from "./project";

/** One changed clip: the index it holds on each side of the preview. */
export interface PreviewClipRef {
  track: string;
  /** `pattern`, `audio` or `automation`. */
  kind: string;
  /** Pattern name, media name or automation parameter. */
  name: string;
  base_index?: number;
  proposal_index?: number;
}

/** One changed effect, by its owner and its position in the chain. */
export interface PreviewEffectRef {
  /** `track:<name>`, `bus:<name>`, `preset:<name>` or `master`. */
  target: string;
  index: number;
}

/** The bars of one pattern the proposal changed; 1-based and inclusive. */
export interface PreviewNoteSpan {
  /** Global pattern name, or a track-inline pattern's own name. */
  pattern: string;
  track?: string;
  start_bar: number;
  end_bar: number;
}

/** Every subtree the proposal changes, by the name a user sees. */
export interface PreviewHighlight {
  tracks: string[];
  patterns: string[];
  inline_patterns: string[];
  presets: string[];
  clips: PreviewClipRef[];
  effects: PreviewEffectRef[];
  notes: PreviewNoteSpan[];
}

/** Which document the session is showing, and what changed. */
export interface PreviewState {
  side: "base" | "proposal";
  target_path: string;
  highlight: PreviewHighlight;
}

/** A project snapshot with the preview the panel paints beside it. */
export interface PreviewSnapshot extends ProjectSnapshot {
  preview: PreviewState;
}
