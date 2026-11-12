import { i18n, type LocaleName } from "./i18n";
import { presetCopy, type PresetCopyKey } from "./locales/presetCopy";

/** Keep program/category IDs stable; only the label follows the interface language. */
export function presetCategoryLabel(category: string, locale?: LocaleName): string {
  const key = `preset.category.${category}` as PresetCopyKey;
  return Object.hasOwn(presetCopy.en, key) ? (locale ? presetCopy[locale][key] : i18n.t(key)) : category;
}

export function presetProgramLabel(program: number, locale?: LocaleName): string {
  const key = `preset.program.${program}` as PresetCopyKey;
  return Object.hasOwn(presetCopy.en, key) ? (locale ? presetCopy[locale][key] : i18n.t(key)) : i18n.t("preset.programNumber", { number: program });
}
