/**
 * One small line icon per tool family, drawn with `currentColor` so it follows
 * the theme. The transcript shows it before the activity summary; the disclosure
 * marker is hidden so the icon, not a triangle, identifies the row.
 */
import type { JSX } from "solid-js";

type IconProps = { name: string };

function Svg(props: { children: JSX.Element }) {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {props.children}
    </svg>
  );
}

const BOOK = () => <><path d="M4 5a2 2 0 0 1 2-2h12v18H6a2 2 0 0 1-2-2z" /><path d="M8 3v18" /></>;
const SEARCH = () => <><circle cx="11" cy="11" r="7" /><path d="M16.5 16.5 21 21" /></>;
const WAVE = () => <><path d="M3 12h2l2-6 3 12 3-9 2 5 2-2h4" /></>;
const DOC = () => <><path d="M6 3h8l4 4v14H6z" /><path d="M14 3v4h4" /><path d="M9 12h6M9 16h6" /></>;
const SLIDERS = () => <><path d="M4 6h16M4 12h16M4 18h16" /><circle cx="9" cy="6" r="2" /><circle cx="15" cy="12" r="2" /><circle cx="8" cy="18" r="2" /></>;
const SPEAKER = () => <><path d="M4 10v4h4l5 4V6L8 10H4z" /><path d="M17 9a4 4 0 0 1 0 6" /></>;
const PLUG = () => <><path d="M9 3v5M15 3v5" /><path d="M6 8h12v3a6 6 0 0 1-12 0z" /><path d="M12 17v4" /></>;
const LAYERS = () => <><path d="m12 3 9 5-9 5-9-5z" /><path d="m3 13 9 5 9-5" /></>;
const SPARK = () => <><path d="M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5 18 18M18 6l-2.5 2.5M8.5 15.5 6 18" /></>;

const BY_TOOL: Record<string, () => JSX.Element> = {
  read_skill: BOOK,
  list_skills: BOOK,
  sample_search: SEARCH,
  sample_pull: WAVE,
  plugin_search: SEARCH,
  plugin_pull: PLUG,
  plugins_list: PLUG,
  plugins_scan: PLUG,
  project_get: DOC,
  project_dsl: DOC,
  project_extract: DOC,
  project_validate: DOC,
  midi_import_handoff: DOC,
  midi_export_handoff: DOC,
  edit_subtree: DOC,
  presets_list: SLIDERS,
  presets_add: SLIDERS,
  presets_remove: SLIDERS,
  presets_set_param: SLIDERS,
  presets_set_params: SLIDERS,
  tracks_preset: SLIDERS,
  devices_catalog: SLIDERS,
  device_presets_list: SLIDERS,
  device_preset_apply: SLIDERS,
  render_range: SPEAKER,
  render_offline: SPEAKER,
  render_stems: SPEAKER,
  render_compat_report: SPEAKER,
  audio_analyze: WAVE,
  audio_compare: WAVE,
  audition_range: SPEAKER,
  transaction_begin: LAYERS,
  transaction_commit: LAYERS,
  transaction_rollback: LAYERS,
};

export function ToolIcon(props: IconProps) {
  const icon = BY_TOOL[props.name] ?? SPARK;
  return <Svg>{icon()}</Svg>;
}
