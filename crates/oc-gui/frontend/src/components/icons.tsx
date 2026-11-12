/**
 * OpenCompose line icons — a 16px set drawn in a 24×24 box with `currentColor`
 * strokes, used instead of emoji for navigation.
 *
 * Store cards show each site's own bundled icon; these glyphs are the fallback
 * when that icon cannot be loaded (see StoreBrowser).
 */
import type { JSX } from "solid-js";

function Icon(props: { children: JSX.Element }) {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {props.children}
    </svg>
  );
}

/** DSL editor. */
export function CodeIcon() {
  return (
    <Icon>
      <path d="m10 6-6 6 6 6" />
      <path d="m14 6 6 6-6 6" />
    </Icon>
  );
}

/** Plug-in browser. */
export function PlugIcon() {
  return (
    <Icon>
      <path d="M8 3v5" />
      <path d="M16 3v5" />
      <path d="M5 8h14v4a7 7 0 0 1-14 0Z" />
      <path d="M12 19v2" />
    </Icon>
  );
}

/** Store browser: a shopping bag. */
export function StoreIcon() {
  return (
    <Icon>
      <path d="M6 2 3 6v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6l-3-4Z" />
      <path d="M3 6h18" />
      <path d="M16 10a4 4 0 0 1-8 0" />
    </Icon>
  );
}

/** AI assistant. */
export function SparkIcon() {
  return (
    <Icon>
      <path d="M12 3Q13.5 9 19 12 13.5 15 12 21 10.5 15 5 12 10.5 9 12 3Z" />
    </Icon>
  );
}

/** Instrument plug-ins and sampler assets. */
export function KeyboardIcon() {
  return (
    <Icon>
      <rect width="20" height="16" x="2" y="4" rx="2" />
      <path d="M6 8h.01" />
      <path d="M10 8h.01" />
      <path d="M14 8h.01" />
      <path d="M18 8h.01" />
      <path d="M8 12h.01" />
      <path d="M12 12h.01" />
      <path d="M16 12h.01" />
      <path d="M7 16h10" />
    </Icon>
  );
}

/** Effect plug-ins. */
export function SliderIcon() {
  return (
    <Icon>
      <path d="M5 21v-7" />
      <path d="M5 10V3" />
      <path d="M12 21v-9" />
      <path d="M12 8V3" />
      <path d="M19 21v-5" />
      <path d="M19 12V3" />
      <path d="M2 14h6" />
      <path d="M9 8h6" />
      <path d="M16 16h6" />
    </Icon>
  );
}

/** Sampler and other audio assets. */
export function WaveIcon() {
  return (
    <Icon>
      <path d="M2 10v4" />
      <path d="M6 6v12" />
      <path d="M10 3v18" />
      <path d="M14 8v8" />
      <path d="M18 5v14" />
      <path d="M22 10v4" />
    </Icon>
  );
}

/** Presets. */
export function FolderIcon() {
  return (
    <Icon>
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </Icon>
  );
}

/** SoundFonts. */
export function DiskIcon() {
  return (
    <Icon>
      <path d="M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Z" />
      <path d="M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7" />
      <path d="M7 3v4a1 1 0 0 0 1 1h7" />
    </Icon>
  );
}
