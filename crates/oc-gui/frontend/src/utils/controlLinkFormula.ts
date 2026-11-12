/**
 * Link formula catalog: the names, input domain and mapping of each curve.
 *
 * The same six names are the DSL and JSON wire format (`oc_dsl::ast::LinkFormula`),
 * so the menu, the project text and the engine all name one curve the same way.
 */
import type { ControlLinkSource, LinkFormula } from "../types/project";

export type FormulaInput = "normalized" | "midi";

export interface FormulaInfo {
  value: LinkFormula;
  /** ASCII technical label; the mapping is shown next to it. */
  label: string;
  input: FormulaInput;
  /** Mapping over the declared input, for the row tooltip. */
  mapping: string;
}

export const LINK_FORMULAS: FormulaInfo[] = [
  { value: "linear", label: "linear", input: "normalized", mapping: "y = x" },
  { value: "inverted", label: "inverted", input: "normalized", mapping: "y = 1 - x" },
  { value: "exponential", label: "exponential", input: "normalized", mapping: "y = x²" },
  { value: "logarithmic", label: "logarithmic", input: "normalized", mapping: "y = √x" },
  { value: "midi_unit", label: "midi unit", input: "midi", mapping: "y = raw / 127" },
  {
    value: "midi_centered",
    label: "midi centered",
    input: "midi",
    mapping: "triangle peaking at 64",
  },
];

const BY_VALUE = new Map(LINK_FORMULAS.map((formula) => [formula.value, formula]));

export function formulaInfo(formula: LinkFormula): FormulaInfo {
  return BY_VALUE.get(formula) ?? LINK_FORMULAS[0];
}

/** A macro or lane is already 0..1, so raw-MIDI curves are not offered there. */
export function formulasFor(source: ControlLinkSource): FormulaInfo[] {
  const midi = source.kind === "midi_cc";
  return LINK_FORMULAS.filter((formula) => midi || formula.input !== "midi");
}

/** One-line label for the control itself. */
export function linkSourceLabel(source: ControlLinkSource): string {
  switch (source.kind) {
    case "midi_cc":
      return source.channel === 1
        ? `CC ${source.controller}`
        : `CC ${source.controller} ch${source.channel}`;
    case "macro":
      return `Macro ${source.index}`;
    case "lane":
      return `Lane ${source.param}`;
  }
}
