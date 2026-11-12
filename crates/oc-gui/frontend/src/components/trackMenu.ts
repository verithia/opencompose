import { store } from "../stores/project";
import { panelStore } from "../stores/panels";
import { i18n } from "../stores/i18n";
import { bounceStore } from "../stores/bounce";
import { openMidiExportDialog, openMidiImportDialog } from "../stores/midiUi";
import type { ContextMenuItem } from "./ContextMenu";
import { COLOUR_PALETTE } from "../utils/colourPalette";
import { openContextMenu } from "./ContextMenu";
import { ask } from "@tauri-apps/plugin-dialog";
import type { Track } from "../types/project";

export interface TrackMenuOptions {
  x: number;
  y: number;
  trackIndex: number;
  onRename: () => void;
  onCreatePattern?: () => void;
  /** Whether this track's automation rows are drawn right now. */
  automationShown?: boolean;
  onToggleAutomation?: () => void;
  /** The track's record arm, which lives here rather than on the header. */
  armed?: boolean;
  armDisabledReason?: string | null;
  onToggleArm?: () => void;
  includeAddTrack?: boolean;
  includeOpenMixer?: boolean;
  includeMidi?: boolean;
}

/**
 * Duplicate a track and give the copy its own preset. The backend repeats the
 * track and forks the patch in one undo entry, so editing the copy stays local.
 * Mix and clips are not copied.
 */
export async function duplicateTrack(trackIndex: number): Promise<number> {
  return store.duplicateTrack(trackIndex);
}

/** A track that carries clips takes them with it, so it asks first. */
async function deleteTrack(track: Track, trackIndex: number): Promise<void> {
  const clips = track.clips.length + track.phrases.length + track.patterns.length
    + track.inline_patterns.length;
  if (clips === 0) {
    void store.removeTrack(trackIndex);
    return;
  }
  const confirmed = await ask(
    i18n.t("menu.track.deleteConfirm", { name: track.name, count: clips }),
    { title: i18n.t("menu.track.deleteTitle"), kind: "warning" },
  );
  if (confirmed) void store.removeTrack(trackIndex);
}

/** Right-click menus are read-only: they name the track and never move the selection. */
export function openTrackContextMenu(options: TrackMenuOptions) {
  const track = store.project.tracks[options.trackIndex];
  if (!track) return;

  const items: ContextMenuItem[] = [{ type: "heading", label: track.name }];
  if (options.onCreatePattern) {
    items.push(
      { label: i18n.t("menu.track.createPattern"), onSelect: options.onCreatePattern },
      { type: "separator" },
    );
  }
  if (options.onToggleAutomation) {
    items.push({
      label: options.automationShown
        ? i18n.t("timeline.automation.hide")
        : i18n.t("timeline.automation.show"),
      onSelect: options.onToggleAutomation,
    });
  }
  if (options.onToggleArm) {
    items.push({
      label: options.armed ? i18n.t("menu.track.disarm") : i18n.t("menu.track.arm"),
      disabled: !options.armed && Boolean(options.armDisabledReason),
      hint: !options.armed ? options.armDisabledReason ?? undefined : undefined,
      onSelect: options.onToggleArm,
    });
  }
  items.push(
    { label: i18n.t("menu.track.rename"), onSelect: options.onRename },
    {
      // A chip row and a picker keep eight colours from burying the actions.
      type: "palette",
      label: i18n.t("menu.track.colour"),
      colours: COLOUR_PALETTE.map((colour) => ({
        hex: colour.hex,
        label: i18n.t(colour.key),
      })),
      value: track.mix.overrides?.color ?? null,
      onPick: (hex) => void store.setTrackChannelOverrides(options.trackIndex, { color: hex }),
      onCustom: (hex) => void store.setTrackChannelOverrides(options.trackIndex, { color: hex }),
      resetLabel: i18n.t("menu.track.colourDefault"),
      onReset: () => void store.setTrackChannelOverrides(options.trackIndex, { color: null }),
      resetDisabled: !track.mix.overrides?.color,
    },
    {
      label: track.mix.mute ? i18n.t("menu.track.unmute") : i18n.t("menu.track.mute"),
      onSelect: () => void store.toggleMute(options.trackIndex),
    },
    {
      label: track.mix.solo ? i18n.t("menu.track.unsolo") : i18n.t("menu.track.solo"),
      onSelect: () => void store.toggleSolo(options.trackIndex),
    },
    { type: "separator" },
    {
      label: i18n.t("menu.track.duplicate"),
      onSelect: () => void duplicateTrack(options.trackIndex),
    },
    {
      label: i18n.t("menu.track.savePresetAsNew"),
      disabled: !track.preset,
      hint: track.preset ? undefined : i18n.t("menu.track.presetHint"),
      onSelect: () => void store.saveTrackPresetAsNew(options.trackIndex),
    },
    { type: "separator" },
    {
      label: i18n.t("menu.track.bounce"),
      onSelect: () => bounceStore.openBounce(track.name),
    },
    { type: "separator" },
    {
      label: i18n.t("menu.track.delete"),
      danger: true,
      onSelect: () => void deleteTrack(track, options.trackIndex),
    },
  );

  // MIDI, adding a track and opening the mixer are occasional: one group keeps
  // the actions above them readable.
  const tools: ContextMenuItem[] = [];
  if (options.includeMidi) {
    tools.push(
      {
        label: i18n.t("timeline.importMidi"),
        onSelect: () => void openMidiImportDialog(options.trackIndex),
      },
      {
        label: i18n.t("timeline.exportMidi"),
        onSelect: () => openMidiExportDialog(options.trackIndex),
      },
    );
  }
  if (options.includeAddTrack) {
    tools.push({
      label: i18n.t("menu.track.addAtEnd"),
      onSelect: () => void store.addTrack(
        `Track ${store.project.tracks.length + 1}`,
        "piano",
        "acoustic_grand",
      ),
    });
  }
  if (options.includeOpenMixer) {
    tools.push({
      label: i18n.t("menu.track.openMixer"),
      onSelect: () => {
        store.setSelectedTrackIndex(options.trackIndex);
        panelStore.openBottom("mixer");
      },
    });
  }
  if (tools.length > 0) {
    items.push({ type: "separator" }, {
      type: "group",
      label: i18n.t("menu.track.tools"),
      items: tools,
    });
  }

  openContextMenu({
    x: options.x,
    y: options.y,
    className: "track-context-menu",
    ariaLabel: i18n.t("menu.actions"),
    items,
  });
}
