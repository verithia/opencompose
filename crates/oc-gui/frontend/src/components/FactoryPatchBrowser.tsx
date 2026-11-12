import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { i18n } from "../stores/i18n";
import { listLibraryPatches, listPatchPreferences, setPatchFavorite } from "../stores/api";
import type { AbSnapshot } from "../stores/api";
import { log } from "../stores/log";
import type { Effect, ParamValue } from "../types/project";
import { placeTooltip, useTooltip } from "./Tooltip";

export interface FactoryPatchInfo {
  name: string;
  category?: string;
  /** Free-form searchable labels; user library entries carry their tags here. */
  tags?: string[];
  /** Where the row came from; the browser groups user entries apart. */
  tier?: "factory" | "user";
  /** Whole patch for user library rows, so load and audition use the stored object. */
  params?: [string, ParamValue][];
  effects?: Effect[];
  /** Browsing preference; the browser also reads them from the backend. */
  favorite?: boolean;
  /** Unix seconds of the last whole-patch load. */
  lastUsedAt?: number | null;
}

export interface FactoryPatchGroup {
  /** Section header, e.g. favourites or the user library; null for the bank. */
  section: string | null;
  category: string | null;
  patches: FactoryPatchInfo[];
}

/** Stable row identity: a name is only unique inside its own tier. */
export function patchKey(patch: FactoryPatchInfo): string {
  return `${patch.tier === "user" ? "library" : "factory"}:${patch.name}`;
}

const byName = (left: FactoryPatchInfo, right: FactoryPatchInfo) =>
  left.name.toLowerCase().localeCompare(right.name.toLowerCase());

/**
 * Rows in browser order: favourites first (alphabetical), then the rest by
 * most recent load, then the order they arrived in. The sort is stable, so
 * untouched rows keep the bank's own order.
 */
export function orderFactoryPatches(
  patches: FactoryPatchInfo[],
  isFavorite: (patch: FactoryPatchInfo) => boolean = (patch) => patch.favorite ?? false,
  lastUsed: (patch: FactoryPatchInfo) => number | null = (patch) => patch.lastUsedAt ?? null,
): FactoryPatchInfo[] {
  const indexed = patches.map((patch, index) => ({ patch, index }));
  const favorites = indexed
    .filter((row) => isFavorite(row.patch))
    .sort((left, right) => byName(left.patch, right.patch));
  const rest = indexed
    .filter((row) => !isFavorite(row.patch))
    .sort((left, right) =>
      (lastUsed(right.patch) ?? -1) - (lastUsed(left.patch) ?? -1) || left.index - right.index);
  return [...favorites, ...rest].map((row) => row.patch);
}

/**
 * Split ordered rows into the favourites section and the usual category
 * groups. Favourites are hoisted out of their category so no row appears
 * twice and every keyboard index names one row.
 */
export function groupFactoryPatches(
  patches: FactoryPatchInfo[],
  isFavorite: (patch: FactoryPatchInfo) => boolean = (patch) => patch.favorite ?? false,
): FactoryPatchGroup[] {
  const groups: FactoryPatchGroup[] = [];
  const favorites = patches.filter(isFavorite);
  if (favorites.length > 0) {
    groups.push({
      section: i18n.t("synth.patchFavorites"),
      category: null,
      patches: [...favorites].sort(byName),
    });
  }
  for (const patch of patches) {
    if (isFavorite(patch)) continue;
    const section = patch.tier === "user" ? i18n.t("synth.patchUserSection") : null;
    const category = patch.category ?? null;
    const existing = groups.find(
      (group) => group.section === section && group.category === category,
    );
    if (existing) existing.patches.push(patch);
    else groups.push({ section, category, patches: [patch] });
  }
  return groups;
}

/** Name, category or tag match; the library section is searchable by tag. */
export function matchesFactoryPatch(patch: FactoryPatchInfo, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return patch.name.toLowerCase().includes(needle)
    || (patch.category ?? "").toLowerCase().includes(needle)
    || (patch.tags ?? []).some((tag) => tag.toLowerCase().includes(needle));
}

const PANEL_WIDTH = 300;
const PANEL_HEIGHT = 392;
const GAP = 4;
const MARGIN = 8;
let nextId = 0;

/**
 * In-app factory patch picker. Renders into a portal so the compact panel's
 * scroll container cannot clip it, and floats above the tooltip layer.
 *
 * Favourite and last-used are browsing preferences, so the browser reads them
 * from the backend once per open and writes each star toggle back.
 */
export function FactoryPatchBrowser(props: {
  anchor: () => HTMLElement | undefined;
  current: string;
  /**
   * Patch the user chose whose load has not landed yet. The row paints as
   * selected immediately so the click never looks frozen.
   */
  chosen?: () => string | null;
  /** Name of the patch loaded in the editor; the A side of a comparison. */
  abA?: string;
  patches: FactoryPatchInfo[];
  /** Candidate currently sounding or being prepared, by patch name. */
  auditioning?: () => string | null;
  /** Status of the current audition, e.g. `loading` or `ready`. */
  auditionStatus?: () => string | null;
  auditionPaused?: () => boolean;
  /** A patch load is in flight, so audition clicks are ignored. */
  loading?: () => boolean;
  /** Last measured pair, when it belongs to the last auditioned candidate. */
  ab?: () => AbSnapshot | null;
  abBusy?: () => boolean;
  abError?: () => string | null;
  onCompare?: (name: string) => void;
  onSwitchAb?: () => void;
  onLoad: (name: string) => void;
  onAudition?: (name: string) => void;
  onStopAudition?: () => void;
  onClose: () => void;
}) {
  const id = `factory-patch-${++nextId}`;
  const [query, setQuery] = createSignal("");
  const [active, setActive] = createSignal(0);
  const [candidate, setCandidate] = createSignal<string | null>(null);
  const [position, setPosition] = createSignal<{ x: number; y: number } | null>(null);
  const [favorites, setFavorites] = createSignal<Record<string, boolean>>({});
  const [lastUsed, setLastUsed] = createSignal<Record<string, number | null>>({});
  let element: HTMLDivElement | undefined;
  let searchInput: HTMLInputElement | undefined;
  let disposed = false;

  const isFavorite = (patch: FactoryPatchInfo) =>
    favorites()[patchKey(patch)] ?? patch.favorite ?? false;
  const usedAt = (patch: FactoryPatchInfo) => {
    const stored = lastUsed()[patchKey(patch)];
    return stored === undefined ? patch.lastUsedAt ?? null : stored;
  };

  const filtered = createMemo(() =>
    props.patches.filter((patch) => matchesFactoryPatch(patch, query())),
  );
  const ordered = createMemo(() => orderFactoryPatches(filtered(), isFavorite, usedAt));
  const groups = createMemo(() => groupFactoryPatches(ordered(), isFavorite));
  const rows = createMemo(() => groups().flatMap((group) => group.patches));
  const rowIndex = (key: string) => rows().findIndex((patch) => patchKey(patch) === key);
  /** The loaded patch, or the row chosen while its load is still running. */
  const selectedName = (patch: FactoryPatchInfo) =>
    props.chosen?.() === patch.name || patch.name === props.current;
  const optionId = (index: number) => `${id}-option-${index}`;
  const sounding = (name: string) => props.auditioning?.() === name;
  const preparing = (name: string) => sounding(name) && props.auditionStatus?.() === "loading";
  const auditionLabel = (name: string) => {
    if (sounding(name)) return `${i18n.t(props.auditionPaused?.() ? "tooltip.previewResume" : "tooltip.previewPause")}: ${name}`;
    return i18n.t("synth.patchAudition", { name });
  };
  const patchTags = (patch: FactoryPatchInfo) =>
    (patch.tags ?? []).filter((tag) => tag.trim() && tag !== "user");

  /** The loaded patch is A; the last auditioned row is B. */
  const aName = () => props.abA ?? props.current;
  const pair = () => {
    const snapshot = props.ab?.();
    return snapshot && snapshot.b_label === candidate() ? snapshot : null;
  };
  const compareBusy = () => Boolean(props.abBusy?.());
  const compareError = () => props.abError?.() ?? null;
  const canSwitch = () => Boolean(pair()) && !compareBusy();
  const sideName = (side: "a" | "b" | null) => (side === "a" ? "A" : "B");
  const signedDb = (value: number) => {
    const rounded = Math.abs(value) < 0.05 ? 0 : value;
    return `${rounded > 0 ? "+" : rounded < 0 ? "-" : ""}${Math.abs(rounded).toFixed(1)}`;
  };
  const readout = () => {
    if (!candidate()) return i18n.t("synth.patchCompareEmpty");
    if (compareBusy()) return i18n.t("synth.patchCompareBusy");
    const message = compareError();
    if (message) return i18n.t("synth.patchCompareFailed", { message });
    const snapshot = pair();
    if (!snapshot) return i18n.t("synth.patchCompareIdle");
    if (snapshot.measurement_pending) return i18n.t("synth.patchCompareBusy");
    if (snapshot.status === "matched") {
      return i18n.t("synth.patchCompareMatched", {
        gain: signedDb(snapshot.applied_gain_db ?? 0),
      });
    }
    switch (snapshot.unmeasured) {
      case "a":
        return i18n.t("synth.patchCompareUnmeasuredA");
      case "b":
        return i18n.t("synth.patchCompareUnmeasuredB");
      case "unstable":
        return i18n.t("synth.patchCompareUnstable");
      default:
        return i18n.t("synth.patchCompareUnmeasuredBoth");
    }
  };
  const switchLabel = () => i18n.t("synth.patchAbSwitch", { side: sideName(pair()?.active ?? null) });

  const toggleAudition = (event: MouseEvent, name: string) => {
    event.preventDefault();
    event.stopPropagation();
    if (props.loading?.()) return;
    setCandidate(name);
    props.onAudition?.(name);
  };

  const toggleFavorite = (event: MouseEvent, patch: FactoryPatchInfo) => {
    event.preventDefault();
    event.stopPropagation();
    const key = patchKey(patch);
    const next = !isFavorite(patch);
    setFavorites((current) => ({ ...current, [key]: next }));
    void setPatchFavorite(patch.name, patch.tier === "user" ? "user" : "factory", next)
      .catch((error) => {
        setFavorites((current) => ({ ...current, [key]: !next }));
        log.warn("synth", `Failed to save favourite '${patch.name}': ${String(error)}`);
      });
  };

  /** Read the preference store and the library listing once per open. */
  const loadUsage = async () => {
    const nextFavorites: Record<string, boolean> = {};
    const nextLastUsed: Record<string, number | null> = {};
    try {
      const preferences = await listPatchPreferences();
      for (const [key, usage] of Object.entries(preferences?.patches ?? {})) {
        nextFavorites[key] = usage.favorite;
        nextLastUsed[key] = usage.last_used_at ?? null;
      }
    } catch (error) {
      log.warn("synth", `Preset preferences unavailable: ${String(error)}`);
    }
    try {
      const listing = await listLibraryPatches();
      for (const patch of listing?.patches ?? []) {
        const key = `library:${patch.name}`;
        nextFavorites[key] = patch.favorite ?? false;
        nextLastUsed[key] = patch.last_used_at ?? null;
      }
    } catch (error) {
      log.warn("synth", `Library usage unavailable: ${String(error)}`);
    }
    if (disposed) return;
    setFavorites(nextFavorites);
    setLastUsed(nextLastUsed);
  };

  const move = (delta: number) => {
    const count = rows().length;
    if (count === 0) return;
    const current = active();
    const next = current < 0 ? (delta > 0 ? 0 : count - 1) : (current + delta + count) % count;
    setActive(next);
  };

  const typeQuery = (value: string) => {
    setQuery(value);
    setActive(0);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    // The switch key works even while the search field has focus: a backslash
    // is not a useful character in a patch name, and the pair is already
    // measured, so an extra modifier would only hide the shortcut.
    if (event.key === "\\" && canSwitch()) {
      event.preventDefault();
      event.stopPropagation();
      props.onSwitchAb?.();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      props.onClose();
      return;
    }
    // A focused row button activates itself; Enter must not also load the row.
    if (event.key === "Enter" && (event.target as Element | null)?.closest?.("button")) {
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      move(event.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      const patch = rows()[active()];
      if (patch) props.onLoad(patch.name);
    }
  };

  const onOutsidePointer = (event: PointerEvent) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    if (element?.contains(target)) return;
    // The header field and LOAD button toggle the browser themselves.
    if (target.closest("[data-patch-trigger]")) return;
    props.onClose();
  };

  onMount(() => {
    const rect = props.anchor()?.getBoundingClientRect();
    queueMicrotask(() => {
      if (!element) return;
      const measured = element.getBoundingClientRect();
      const size = {
        width: measured.width || PANEL_WIDTH,
        height: measured.height || PANEL_HEIGHT,
      };
      const viewport = {
        width: window.innerWidth || 1024,
        height: window.innerHeight || 768,
      };
      const anchor = rect ?? { left: MARGIN, top: MARGIN, right: MARGIN, bottom: MARGIN, width: 0, height: 0 };
      const placed = placeTooltip(anchor, size, viewport, GAP, MARGIN);
      setPosition({ x: placed.x, y: placed.y });
    });
    searchInput?.focus();
    window.addEventListener("pointerdown", onOutsidePointer, true);
    const closeOnResize = () => props.onClose();
    window.addEventListener("resize", closeOnResize);
    void loadUsage();
    onCleanup(() => {
      disposed = true;
      window.removeEventListener("pointerdown", onOutsidePointer, true);
      window.removeEventListener("resize", closeOnResize);
    });
  });

  return (
    <Portal>
      <div
        ref={(el) => { element = el; }}
        class="factory-patch-layer"
        role="dialog"
        aria-label={i18n.t("synth.patchBrowserTitle")}
        style={{
          left: `${position()?.x ?? MARGIN}px`,
          top: `${position()?.y ?? MARGIN}px`,
        }}
        onKeyDown={onKeyDown}
      >
        <div class="factory-patch-search-row">
          <input
            ref={(el) => { searchInput = el; }}
            class="factory-patch-search"
            type="text"
            role="combobox"
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-autocomplete="list"
            aria-activedescendant={rows().length ? optionId(active()) : undefined}
            aria-label={i18n.t("synth.patchSearch")}
            placeholder={i18n.t("synth.patchSearch")}
            value={query()}
            onInput={(event) => typeQuery(event.currentTarget.value)}
          />
        </div>
        <div class="factory-patch-list" role="listbox" id={`${id}-list`} aria-label={i18n.t("synth.patchBrowserTitle")}>
          <For each={groups()}>
            {(group) => (
              <div class="factory-patch-group">
                <Show when={group.section}>
                  {(section) => <div class="factory-patch-group-section" role="presentation">{section()}</div>}
                </Show>
                <Show when={group.category}>
                  {(category) => <div class="factory-patch-group-label" role="presentation">{category()}</div>}
                </Show>
                <For each={group.patches}>
                  {(patch) => {
                    const key = patchKey(patch);
                    const index = () => rowIndex(key);
                    const favorite = () => isFavorite(patch);
                    const tags = () => patchTags(patch);
                    return (
                      <div
                        id={optionId(index())}
                        class="factory-patch-row"
                        classList={{
                          active: index() === active(),
                          current: patch.name === props.current,
                          chosen: selectedName(patch),
                          auditioning: sounding(patch.name),
                        }}
                        role="option"
                        aria-selected={selectedName(patch)}
                        aria-busy={selectedName(patch) && (props.loading?.() ?? false)}
                        onMouseDown={(event) => event.preventDefault()}
                        onMouseEnter={() => setActive(index())}
                        onClick={() => setActive(index())}
                        onDblClick={() => props.onLoad(patch.name)}
                      >
                        <span class="factory-patch-text">
                          <span class="factory-patch-name">{patch.name}</span>
                          <Show when={tags().length > 0}>
                            <span class="factory-patch-tags">
                              <For each={tags()}>
                                {(tag) => (
                                  <button
                                    type="button"
                                    class="factory-patch-tag"
                                    tabIndex={-1}
                                    aria-label={i18n.t("synth.patchTagFilter", { tag })}
                                    onMouseDown={(event) => event.stopPropagation()}
                                    onClick={(event) => {
                                      event.preventDefault();
                                      event.stopPropagation();
                                      typeQuery(tag);
                                      searchInput?.focus();
                                    }}
                                  >
                                    {tag}
                                  </button>
                                )}
                              </For>
                            </span>
                          </Show>
                        </span>
                        <Show when={patch.name === props.current}>
                          <span class="factory-patch-current-mark" aria-hidden="true">●</span>
                        </Show>
                        {/* Reserved while the load runs, so the row never reflows. */}
                        <Show when={selectedName(patch) && patch.name !== props.current}>
                          <span class="factory-patch-current-mark pending" aria-hidden="true">
                            {props.loading?.() ? "…" : "○"}
                          </span>
                        </Show>
                        <button
                          type="button"
                          class="factory-patch-favorite"
                          classList={{ on: favorite() }}
                          aria-label={i18n.t("synth.patchFavorite", { name: patch.name })}
                          aria-pressed={favorite()}
                          onMouseDown={(event) => event.stopPropagation()}
                          onDblClick={(event) => event.stopPropagation()}
                          onClick={(event) => toggleFavorite(event, patch)}
                        >
                          <span aria-hidden="true">{favorite() ? "★" : "☆"}</span>
                        </button>
                        <button
                          type="button"
                          class="factory-patch-audition"
                          classList={{ playing: sounding(patch.name), busy: preparing(patch.name) }}
                          aria-label={auditionLabel(patch.name)}
                          aria-pressed={sounding(patch.name)}
                          aria-busy={preparing(patch.name)}
                          disabled={props.loading?.() ?? false}
                          onMouseDown={(event) => event.stopPropagation()}
                          onDblClick={(event) => event.stopPropagation()}
                          onClick={(event) => toggleAudition(event, patch.name)}
                        >
                          <span aria-hidden="true">{preparing(patch.name) ? "…" : sounding(patch.name) && !props.auditionPaused?.() ? "Ⅱ" : "▶"}</span>
                        </button>
                      </div>
                    );
                  }}
                </For>
              </div>
            )}
          </For>
          <Show when={rows().length === 0}>
            <p class="factory-patch-empty" role="status">
              {i18n.t("synth.patchEmpty", { query: query().trim() })}
            </p>
          </Show>
        </div>
        <div
          class="factory-patch-compare"
          role="group"
          aria-label={i18n.t("synth.patchCompareGroup")}
        >
          <div class="factory-patch-compare-sides">
            <span
              class="factory-patch-compare-badge"
              classList={{ active: pair()?.active === "a" }}
              aria-hidden="true"
            >A</span>
            <span class="factory-patch-compare-name" title={aName()}>{aName()}</span>
            <span
              class="factory-patch-compare-badge"
              classList={{ active: pair()?.active === "b" }}
              aria-hidden="true"
            >B</span>
            <span class="factory-patch-compare-name" title={candidate() ?? undefined}>
              {candidate() ?? "—"}
            </span>
          </div>
          <button
            type="button"
            class="factory-patch-compare-action"
            aria-label={i18n.t("synth.patchCompareLabel", { a: aName(), b: candidate() ?? "" })}
            title={i18n.t("synth.patchCompareLabel", { a: aName(), b: candidate() ?? "" })}
            disabled={!candidate() || compareBusy() || (props.loading?.() ?? false)}
            aria-busy={compareBusy()}
            {...useTooltip(() => i18n.t("synth.patchCompareLabel", { a: aName(), b: candidate() ?? "" }))}
            onMouseDown={(event) => event.stopPropagation()}
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              const name = candidate();
              if (name && !compareBusy()) props.onCompare?.(name);
            }}
          >
            A/B
          </button>
          <button
            type="button"
            class="factory-patch-compare-action switch"
            aria-label={switchLabel()}
            title={i18n.t("synth.patchAbSwitchHint")}
            disabled={!canSwitch()}
            {...useTooltip(() => i18n.t("synth.patchAbSwitchHint"))}
            onMouseDown={(event) => event.stopPropagation()}
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              if (canSwitch()) props.onSwitchAb?.();
            }}
          >
            <span aria-hidden="true">⇄</span>
          </button>
        </div>
        <p class="factory-patch-compare-readout" role="status" aria-live="polite">
          {readout()}
        </p>
      </div>
    </Portal>
  );
}
