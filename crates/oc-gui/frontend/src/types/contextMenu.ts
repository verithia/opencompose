/**
 * One row in a context menu. The store layer builds these arrays, while the
 * component owns rendering and positioning.
 */
export type ContextMenuItem =
  | {
    type?: "item";
    label: string;
    onSelect: () => void | Promise<void>;
    shortcut?: string;
    disabled?: boolean;
    /** Why a disabled item cannot run; shown in the row itself. */
    hint?: string;
    danger?: boolean;
    /** Renders a check mark, for menus that choose one of several values. */
    checked?: boolean;
    /** Independent toggles use checkbox; exclusive choices default to radio. */
    checkType?: "checkbox" | "radio";
  }
  | {
    type: "slider";
    label: string;
    value: number;
    min: number;
    max: number;
    step: number;
    /** Formats the live readout; the raw number when omitted. */
    format?: (value: number) => string;
    onCommit: (value: number) => void;
    disabled?: boolean;
  }
  | {
    /** A titled group that expands in place: the menu's own submenu, without a
     * flyout that can fall off the window edge. */
    type: "group";
    label: string;
    items: ContextMenuItem[];
    disabled?: boolean;
  }
  | {
    /** A labelled row of colour chips with a button that opens a picker. */
    type: "palette";
    label: string;
    colours: { hex: string; label: string }[];
    value?: string | null;
    onPick: (hex: string) => void;
    /** Receives the value the platform picker returns. */
    onCustom?: (hex: string) => void;
    /** Clears the override and returns to the theme colour. */
    resetLabel?: string;
    onReset?: () => void;
    resetDisabled?: boolean;
  }
  | { type: "info"; rows: { label: string; value: string; path?: boolean }[] }
  | { type: "separator" }
  | { type: "heading"; label: string };
