/**
 * Sample markets — the bottom panel's client for other people's catalogues.
 *
 * The panel never hosts a catalogue: it searches the markets the user includes,
 * shows each result's provider and licence, previews a selection through the
 * audition stream, and adds one file to the project at the playhead.
 *
 * Provider chips are the availability map: an unavailable market stays visible
 * with its reason. Credentials are also available in External Sources.
 */
import { For, Show, createEffect, createMemo, createSignal, createUniqueId, on, onCleanup, onMount } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import { store } from "../stores/project";
import * as api from "../stores/api";
import type { SampleMarketItem, SampleMarketPage, SampleProviderAvailability } from "../stores/api";
import { preferencesUi } from "../stores/preferencesUi";
import { sampleCredentialsChanged } from "../stores/sampleCredentials";
import { useTooltip } from "./Tooltip";
import { openContextMenu } from "./ContextMenu";
import "./SampleMarketPanel.css";

function isPreviewOnly(item: SampleMarketItem) {
  // Freesound OAuth can supply an original independently of the preview URL.
  return item.provider !== "freesound" && !item.download_url?.trim();
}

type KindFilter = "all" | "drum" | "loop" | "vocal" | "fx" | "instrument";

const KIND_CHIPS: { id: KindFilter; key: I18nKey }[] = [
  { id: "all", key: "sample.kind.all" },
  { id: "drum", key: "sample.kind.drum_one_shot" },
  { id: "loop", key: "sample.kind.loop" },
  { id: "vocal", key: "sample.kind.vocal" },
  { id: "fx", key: "sample.kind.fx" },
  { id: "instrument", key: "sample.kind.instrument_phrase" },
];

type DurationFilter = "any" | "short" | "medium" | "long";

const DURATION_OPTIONS: { id: DurationFilter; key: I18nKey }[] = [
  { id: "any", key: "sample.duration.any" },
  { id: "short", key: "sample.duration.short" },
  { id: "medium", key: "sample.duration.medium" },
  { id: "long", key: "sample.duration.long" },
];

const DURATION_BOUNDS: Record<DurationFilter, { min?: number; max?: number }> = {
  any: {},
  short: { max: 2 },
  medium: { min: 2, max: 10 },
  long: { min: 10 },
};

/** Refusal codes the panel can name without reading the backend sentence. */
const COMMAND_ERROR_KEYS: Record<string, I18nKey> = {
  licence_notice: "sample.error.licenceNotice",
  key_required: "sample.error.keyRequired",
  invalid_cursor: "sample.error.invalidCursor",
  checksum_mismatch: "sample.error.checksumMismatch",
  size_mismatch: "sample.error.sizeMismatch",
  project_unsaved: "sample.error.projectUnsaved",
};

interface MarketNotice {
  text: string;
  detail: string | null;
}

const KIND_LABELS: Record<string, I18nKey> = {
  drum_one_shot: "sample.kind.drum_one_shot",
  loop: "sample.kind.loop",
  vocal: "sample.kind.vocal",
  fx: "sample.kind.fx",
  instrument_phrase: "sample.kind.instrument_phrase",
};

/** Grey rows that stand in for results until the markets answer. */
const SKELETON_ROWS = [0, 1, 2, 3];

export function SampleMarketPanel() {
  /** Copy a credit, link or download URL; attribution has to travel with the
   * sample, and a page-only result has no file to add. */
  const copyText = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch (error) {
      setStatus(String(error));
    }
  };
  const [query, setQuery] = createSignal("");
  const [kind, setKind] = createSignal<KindFilter>("all");
  const [duration, setDuration] = createSignal<DurationFilter>("any");
  const [commercial, setCommercial] = createSignal(false);
  const [providers, setProviders] = createSignal<SampleProviderAvailability[]>([]);
  const [providersLoading, setProvidersLoading] = createSignal(true);
  const [selectedProviders, setSelectedProviders] = createSignal<string[]>([]);
  const [providersTouched, setProvidersTouched] = createSignal(false);
  const pager = api.createSampleMarketSearchPager();
  const items = pager.items;
  const loading = pager.loading;
  const [pages, setPages] = createSignal<SampleMarketPage[]>([]);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [searched, setSearched] = createSignal(false);
  const [status, setStatus] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal<string | null>(null);
  // A pull whose licence needs acceptance waits here until the user confirms.
  const [licencePrompt, setLicencePrompt] = createSignal<{
    item: SampleMarketItem;
    message: string;
  } | null>(null);
  const [keyDraft, setKeyDraft] = createSignal("");
  const previewOwner = createUniqueId();
  let previewItem: SampleMarketItem | undefined;
  const itemKey = (item: SampleMarketItem) => JSON.stringify([item.provider, item.id]);
  const action = (item: SampleMarketItem) => store.marketSampleAction(previewOwner, item.provider, item.id);
  const previewLabel = (item: SampleMarketItem) => {
    const next = action(item);
    return i18n.t(next === "pause" ? "tooltip.previewPause" : next === "resume" ? "tooltip.previewResume" : "sample.preview");
  };

  /** The keyed market the panel is asking a key for, if one is included. */
  const keyedProvider = createMemo(() =>
    providers().find(
      (provider) =>
        provider.requires_key && provider.search &&
        !provider.available &&
        selectedProviders().includes(provider.id),
    ),
  );

  /** Nothing to show yet: the markets are still answering, or the search is. */
  const pending = createMemo(
    () => items().length === 0 && (providersLoading() || loading()),
  );

  /** One line per market that could not answer, read from its typed fields. */
  const notices = createMemo(() =>
    pages().flatMap((page) => {
      const notice = pageNotice(page);
      return notice ? [notice] : [];
    }),
  );

  function searchFilters(text: string): api.SampleMarketSearchFilters {
    const bounds = DURATION_BOUNDS[duration()];
    return {
      query: text,
      kind: kind() === "all" ? undefined : kind(),
      min_seconds: bounds.min,
      max_seconds: bounds.max,
      commercial_ok: commercial(),
      providers: selectedProviders(),
    };
  }

  /** A filter change starts over: old rows and any in-flight page are dropped. */
  function resetResults() {
    pager.reset();
    setPages([]);
    setSelectedId(null);
    setSearched(false);
    setStatus(null);
  }

  function changeKind(next: KindFilter) {
    if (kind() === next) return;
    setKind(next);
    resetResults();
  }

  function changeDuration(next: DurationFilter) {
    if (duration() === next) return;
    setDuration(next);
    resetResults();
  }

  function changeCommercial(next: boolean) {
    if (commercial() === next) return;
    setCommercial(next);
    resetResults();
  }

  function applyProviders(list: SampleProviderAvailability[]) {
    setProviders(list);
    // A reachable market that is kept out of the default fan-out (Openverse:
    // its search endpoints answer 504 here) is still offered as a chip.
    const available = list
      .filter((provider) => provider.available && provider.default_search !== false)
      .map((provider) => provider.id);
    setSelectedProviders((previous) => {
      if (!providersTouched()) return available;
      return previous.filter((id) => list.some((provider) =>
        provider.id === id && (provider.available || (provider.search && provider.requires_key)),
      ));
    });
  }

  async function loadProviders() {
    setProvidersLoading(true);
    try {
      applyProviders(await api.sampleMarketProviders());
    } catch (error) {
      setStatus(i18n.t("sample.failed", { message: commandErrorText(error) }));
    } finally {
      setProvidersLoading(false);
    }
  }

  createEffect(on(sampleCredentialsChanged, () => void loadProviders(), { defer: true }));

  onMount(() => {
    void loadProviders();
  });

  onCleanup(() => {
    if (previewItem) void store.stopMarketSample(previewOwner, previewItem.provider, previewItem.id);
  });

  function toggleProvider(id: string) {
    setProvidersTouched(true);
    setSelectedProviders((previous) =>
      previous.includes(id) ? previous.filter((entry) => entry !== id) : [...previous, id],
    );
    resetResults();
  }

  async function runSearch() {
    const text = query().trim();
    if (!text) return;
    if (selectedProviders().length === 0) {
      setStatus(i18n.t("sample.marketHint"));
      return;
    }
    setStatus(null);
    setPages([]);
    setSearched(true);
    try {
      const result = await pager.search(searchFilters(text));
      if (!result) return;
      applyProviders(result.providers);
      setPages(result.pages);
    } catch (error) {
      setPages([]);
      setStatus(i18n.t("sample.failed", { message: commandErrorText(error) }));
    }
  }

  async function loadMore() {
    setStatus(null);
    try {
      const result = await pager.loadMore();
      if (!result) return;
      applyProviders(result.providers);
      setPages(result.pages);
    } catch (error) {
      setStatus(i18n.t("sample.failed", { message: commandErrorText(error) }));
    }
  }

  function selectItem(item: SampleMarketItem) {
    previewItem = item;
    setSelectedId(itemKey(item));
    void store.previewMarketSample(previewOwner, item.provider, item.id, item.name, error => {
      setStatus(i18n.t("sample.previewFailed", { message: commandErrorText(error) }));
    });
  }

  async function addItem(item: SampleMarketItem, acceptLicence = false) {
    setAdding(item.id);
    setStatus(null);
    try {
      const result = await api.sampleMarketAdd(
        item.provider,
        item.id,
        store.selectedTrack()?.name ?? null,
        acceptLicence,
      );
      store.applySampleSnapshot(result.snapshot);
      setLicencePrompt(null);
      setStatus(i18n.t("sample.added", { track: result.track }));
    } catch (error) {
      const refusal = api.sampleMarketError(error);
      // The backend states the licence; the user decides, and the file keeps
      // its licence and credit either way.
      if (refusal.code === "licence_notice") {
        setLicencePrompt({ item, message: refusal.message });
      } else {
        setStatus(commandErrorText(error));
      }
    } finally {
      setAdding(null);
    }
  }

  async function saveKey(provider: SampleProviderAvailability) {
    const key = keyDraft().trim();
    if (!key) return;
    try {
      await api.sampleMarketSaveKey(provider.id, key);
      setKeyDraft("");
      setStatus(i18n.t("sample.keySaved"));
      await loadProviders();
      setSelectedProviders((previous) =>
        previous.includes(provider.id) ? previous : [...previous, provider.id],
      );
    } catch (error) {
      setStatus(i18n.t("sample.keyFailed", { message: commandErrorText(error) }));
    }
  }

  return (
    <div class="sample-market-panel">
      <form
        class="sample-market-header"
        onSubmit={(event) => {
          event.preventDefault();
          void runSearch();
        }}
      >
        <input
          class="sample-market-search"
          type="search"
          value={query()}
          placeholder={i18n.t("sample.searchPlaceholder")}
          aria-label={i18n.t("sample.searchPlaceholder")}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
        <button class="toolbar-btn text-btn" type="submit" disabled={loading()}>
          {loading() ? i18n.t("sample.searching") : i18n.t("sample.search")}
        </button>
      </form>

      <div class="sample-market-chips" role="group" aria-label={i18n.t("sample.kinds")}>
        <For each={KIND_CHIPS}>
          {(chip) => (
            <button
              type="button"
              class="sample-market-chip"
              classList={{ on: kind() === chip.id }}
              aria-pressed={kind() === chip.id}
              onClick={() => changeKind(chip.id)}
            >
              {i18n.t(chip.key)}
            </button>
          )}
        </For>
      </div>

      <div class="sample-market-filters">
        <label class="sample-market-filter">
          <span>{i18n.t("sample.duration")}</span>
          <select
            class="sample-market-select"
            value={duration()}
            aria-label={i18n.t("sample.duration")}
            onChange={(event) =>
              changeDuration(event.currentTarget.value as DurationFilter)
            }
          >
            <For each={DURATION_OPTIONS}>
              {(option) => <option value={option.id}>{i18n.t(option.key)}</option>}
            </For>
          </select>
        </label>
        <label class="sample-market-check">
          <input
            type="checkbox"
            checked={commercial()}
            aria-label={i18n.t("sample.commercialOk")}
            onChange={(event) => changeCommercial(event.currentTarget.checked)}
          />
          <span>{i18n.t("sample.commercialOk")}</span>
        </label>
      </div>

      <div class="sample-market-chips" role="group" aria-label={i18n.t("sample.markets")}>
        <Show when={providersLoading() && providers().length === 0}>
          <span class="sample-market-chip sample-market-chip-loading" role="status">
            {i18n.t("sample.loadingMarkets")}
          </span>
        </Show>
        <For each={providers()}>
          {(provider) => (
            <button
              type="button"
              class="sample-market-chip sample-market-provider"
              classList={{
                on: selectedProviders().includes(provider.id),
                unavailable: !provider.available,
              }}
              data-provider={provider.id}
              aria-pressed={selectedProviders().includes(provider.id)}
              onClick={() => toggleProvider(provider.id)}
              {...useTooltip(() =>
                provider.available
                  ? provider.license
                  : `${i18n.t("sample.unavailable")}: ${provider.reason ?? ""}`,
              )}
            >
              <span
                class="sample-market-dot"
                classList={{ off: !provider.available }}
                aria-hidden="true"
              />
              {provider.label}
            </button>
          )}
        </For>
      </div>

      <button
        type="button"
        class="toolbar-btn text-btn"
        onClick={(event) => preferencesUi.openOn("sources", undefined, event.currentTarget)}
      >
        {i18n.t("preferences.page.sources")}
      </button>

      <Show when={keyedProvider()}>
        {(provider) => (
          <form
            class="sample-market-key"
            onSubmit={(event) => {
              event.preventDefault();
              void saveKey(provider());
            }}
          >
            <span class="sample-market-key-label">
              {i18n.t("sample.keyPrompt", { provider: provider().label })}
            </span>
            <input
              class="sample-market-search"
              type="password"
              value={keyDraft()}
              placeholder={i18n.t("sample.keyPlaceholder")}
              aria-label={i18n.t("sample.keyPlaceholder")}
              onInput={(event) => setKeyDraft(event.currentTarget.value)}
            />
            <button class="toolbar-btn text-btn" type="submit">
              {i18n.t("sample.keySave")}
            </button>
          </form>
        )}
      </Show>

      <Show when={notices().length > 0}>
        <div class="sample-market-notices" role="status">
          <For each={notices()}>
            {(notice) => (
              <div class="sample-market-notice" title={notice.detail ?? undefined}>
                {notice.text}
              </div>
            )}
          </For>
        </div>
      </Show>

      <div class="sample-market-list">
        <Show when={pending()}>
          <div class="sample-market-skeleton" role="status" aria-busy="true">
            <div class="sample-market-empty">
              {i18n.t(loading() ? "sample.searching" : "sample.loadingMarkets")}
            </div>
            <For each={SKELETON_ROWS}>
              {() => (
                <div class="sample-market-row sample-market-row-skeleton" aria-hidden="true">
                  <span class="skeleton-line skeleton-line-title" />
                  <span class="skeleton-line skeleton-line-meta" />
                </div>
              )}
            </For>
          </div>
        </Show>
        <Show when={!pending()}>
          <For each={items()}>
            {(item) => (
              <div
                class="sample-market-row"
                classList={{ selected: selectedId() === itemKey(item) }}
                data-provider={item.provider}
                onClick={() => void selectItem(item)}
                onContextMenu={(event) => {
                  event.preventDefault();
                  openContextMenu({
                    x: event.clientX,
                    y: event.clientY,
                    items: [
                      { type: "heading", label: item.name },
                      {
                        label: i18n.t("samples.copyCredit"),
                        disabled: !item.attribution,
                        onSelect: () => void copyText(item.attribution),
                      },
                      {
                        label: i18n.t("samples.copyLink"),
                        disabled: !item.source_url,
                        onSelect: () => void copyText(item.source_url),
                      },
                      { type: "separator" },
                      {
                        label: i18n.t("samples.copyDownload"),
                        disabled: !item.download_url || isPreviewOnly(item),
                        onSelect: () => void copyText(item.download_url),
                      },
                    ],
                  });
                }}
              >
                <div class="sample-market-row-main">
                  <span class="sample-market-name">{item.name}</span>
                  <span class="sample-market-meta">
                    <span class="sample-market-tag">{item.provider_name ?? item.provider}</span>
                    <span>{kindLabel(item.kind)}</span>
                    <span>{durationLabel(item.duration_seconds)}</span>
                  </span>
                  <span class="sample-market-credit" title={item.attribution}>
                    {item.attribution}
                  </span>
                </div>
                <span
                  class="sample-market-license"
                  classList={{ ok: item.license_ok, notice: !item.license_ok }}
                  title={item.license.url ?? item.license.name}
                >
                  {item.license.name}
                </span>
                <div class="sample-market-actions">
                  <button
                    type="button"
                    class="toolbar-btn"
                    aria-label={previewLabel(item)}
                    {...useTooltip(() => previewLabel(item))}
                    onClick={(event) => {
                      event.stopPropagation();
                      void selectItem(item);
                    }}
                  >
                    {action(item) === "pause" ? "Ⅱ" : "▶"}
                  </button>
                  <button
                    type="button"
                    class="toolbar-btn text-btn"
                    disabled={adding() === item.id || isPreviewOnly(item)}
                    onClick={(event) => {
                      event.stopPropagation();
                      void addItem(item);
                    }}
                    {...useTooltip(() =>
                      isPreviewOnly(item)
                        ? i18n.t("sources.previewOnly")
                        : item.license_ok
                        ? i18n.t("sample.licenseOk")
                        : i18n.t("sample.licenseNoticeHint"),
                    )}
                  >
                    {isPreviewOnly(item)
                      ? i18n.t("sources.previewOnly")
                      : i18n.t(adding() === item.id ? "sample.adding" : "sample.add")}
                  </button>
                </div>
              </div>
            )}
          </For>
          <Show when={items().length === 0}>
            <div class="sample-market-empty">
              {i18n.t(searched() ? "sample.noResults" : "sample.empty")}
            </div>
          </Show>
        </Show>
      </div>

      <Show when={pager.cursor() !== null}>
        <div class="sample-market-more">
          <button
            type="button"
            class="toolbar-btn text-btn"
            disabled={pager.loadingMore()}
            onClick={() => void loadMore()}
          >
            {i18n.t(pager.loadingMore() ? "sample.loadingMore" : "sample.loadMore")}
          </button>
        </div>
      </Show>

      <Show when={licencePrompt()}>
        {(prompt) => (
          <div class="sample-market-licence-prompt" role="alertdialog">
            <p class="sample-market-licence-text">
              {i18n.t("sample.licenseNotice", {
                name: prompt().item.name,
                licence: prompt().item.license.name,
              })}
            </p>
            <p class="sample-market-licence-detail">{prompt().message}</p>
            <div class="sample-market-licence-actions">
              <button
                type="button"
                class="toolbar-btn text-btn"
                onClick={() => void addItem(prompt().item, true)}
              >
                {i18n.t("sample.licenseAccept")}
              </button>
              <button
                type="button"
                class="toolbar-btn text-btn"
                onClick={() => setLicencePrompt(null)}
              >
                {i18n.t("sample.licenseCancel")}
              </button>
            </div>
          </div>
        )}
      </Show>

      <Show when={status()}>
        <div class="sample-market-status" role="status">
          {status()}
        </div>
      </Show>
    </div>
  );
}

function kindLabel(kind: string): string {
  const key = KIND_LABELS[kind];
  return i18n.t(key ?? "sample.kind.unknown");
}

function durationLabel(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) {
    return i18n.t("sample.unknownDuration");
  }
  const minutes = Math.floor(seconds / 60);
  const rest = seconds - minutes * 60;
  return `${minutes}:${rest.toFixed(1).padStart(4, "0")}`;
}

function pageNotice(page: SampleMarketPage): MarketNotice | null {
  const provider = page.provider_name || page.provider;
  const retry = (seconds: number | null | undefined) =>
    typeof seconds === "number"
      ? i18n.t("sample.notice.rateLimited", { provider, seconds })
      : i18n.t("sample.notice.rateLimitedNoDelay", { provider });
  let text: string;
  switch (page.error_kind) {
    case "needs_key":
      text = i18n.t("sample.notice.needsKey", { provider });
      break;
    case "rate_limited":
      text = retry(page.retry_after_secs);
      break;
    case "skipped":
      text = page.skip_reason === "rate_limited"
        ? retry(page.retry_after_secs)
        : i18n.t("sample.notice.skipped", { provider });
      break;
    case "unavailable":
    case "timeout":
    case "network":
      text = i18n.t("sample.notice.unreachable", { provider });
      break;
    case "not_found":
      text = i18n.t("sample.notice.noResults", { provider });
      break;
    case "provider_unavailable":
    case "parse":
    case "other":
      text = i18n.t("sample.notice.failed", { provider });
      break;
    default:
      if (page.skipped) {
        text = page.skip_reason === "rate_limited"
          ? retry(page.retry_after_secs)
          : i18n.t("sample.notice.skipped", { provider });
      } else if (page.error) {
        text = i18n.t("sample.notice.failed", { provider });
      } else {
        return null;
      }
  }
  return { text, detail: page.error ?? null };
}

function commandErrorText(error: unknown): string {
  const refusal = api.sampleMarketError(error);
  const key = refusal.code ? COMMAND_ERROR_KEYS[refusal.code] : undefined;
  return key ? i18n.t(key) : refusal.message;
}
