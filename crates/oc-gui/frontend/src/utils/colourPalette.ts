/**
 * The colours a track or clip can take, with the name the menu shows.
 *
 * Both palettes draw from this list so a colour means the same thing wherever
 * it is chosen, and the rows carry names instead of hex strings.
 */
import type { I18nKey } from "../stores/i18n";

export interface PaletteColour {
  hex: string;
  key: I18nKey;
}

export const COLOUR_PALETTE: PaletteColour[] = [
  { hex: "#6bc0e8", key: "menu.colour.sky" },
  { hex: "#5cc9a0", key: "menu.colour.mint" },
  { hex: "#e0c050", key: "menu.colour.amber" },
  { hex: "#a89dd8", key: "menu.colour.lavender" },
  { hex: "#d98e8e", key: "menu.colour.rose" },
  { hex: "#a4cc5a", key: "menu.colour.lime" },
  { hex: "#52c9b8", key: "menu.colour.teal" },
  { hex: "#d98eb8", key: "menu.colour.pink" },
];
