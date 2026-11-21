/**
 * Sample-market and Freesound commands.
 *
 * This module owns the marketplace wire types; `api.ts` re-exports them so
 * callers keep their existing import path.
 */
import { createSignal, type Accessor } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { ProjectSnapshot } from "../types/project";
import { startNativeAudio } from "./webAudioFocus";

// ============================================================================
// Sample marketplaces
// ============================================================================

/** One external market as the panel's chips show it. */
export interface SampleProviderAvailability {
  id: string;
  label: string;
  requires_key: boolean;
  kinds: string[];
  license: string;
  search: boolean;
  download: boolean;
  available: boolean;
  /** Whether a search that names no market includes this provider. */
  default_search: boolean;
  /** Why it cannot be searched: page-only, no key, or not wired. */
  reason: string | null;
}

/** One result. `license_ok` is the licence rule's verdict, not the source's. */
export interface SampleMarketItem {
  id: string;
  provider: string;
  provider_name?: string;
  name: string;
  kind: string;
  duration_seconds: number | null;
  channels: number | null;
  sample_rate: number | null;
  license: {
    id: string;
    name: string;
    url: string | null;
    attribution_required: boolean;
    share_alike: boolean;
    non_commercial: boolean;
    no_derivatives: boolean;
  };
  attribution: string;
  source_url: string;
  preview_url: string | null;
  download_url: string;
  quality: "original" | "preview_mp3";
  tags: string[];
  license_ok: boolean;
  /** Set when the licence is not free-to-reuse; the pull needs acceptance. */
  license_notice?: string | null;
}

/** Why a market returned no results; read this instead of `error`. */
export type SampleMarketErrorKind =
  | "needs_key"
  | "provider_unavailable"
  | "skipped"
  | "rate_limited"
  | "not_found"
  | "unavailable"
  | "timeout"
  | "network"
  | "parse"
  | "other";

/** Why a market was not asked this time. */
export type SampleMarketSkipReason = "rate_limited" | "circuit_open";

/** What one market answered, or why it could not. */
export interface SampleMarketPage {
  provider: string;
  provider_name: string;
  /** The provider offset this market was asked at; null when it was not. */
  offset?: number | null;
  count: number;
  total?: number | null;
  next_offset?: number | null;
  error?: string | null;
  error_kind?: SampleMarketErrorKind | null;
  skipped?: boolean;
  skip_reason?: SampleMarketSkipReason | null;
  /** Seconds until the market is asked again, when known. */
  retry_after_secs?: number | null;
  http_status?: number | null;
}

export interface SampleMarketSearchResult {
  status: "ok";
  query: string;
  kind: string | null;
  commercial_ok: boolean;
  offset: number;
  limit: number;
  providers: SampleProviderAvailability[];
  pages: SampleMarketPage[];
  count: number;
  /** Results dropped because an earlier market listed the same sound. */
  duplicates: number;
  /** Shared offset for older callers; null for a cursor search. */
  next_offset: number | null;
  /** Opaque continuation; pass it back as `cursor`. Null on the last page. */
  cursor: string | null;
  items: SampleMarketItem[];
}

/**
 * A rejected sample-market command. `code` is the backend's stable refusal
 * code (`licence_notice`, `key_required`, `invalid_cursor`, ...) or null for a
 * failure that is not a refusal. `String(error)` is the message, so existing
 * status lines keep reading it.
 */
export class SampleMarketError extends Error {
  readonly code: string | null;

  constructor(code: string | null, message: string) {
    super(message);
    this.name = "SampleMarketError";
    this.code = code;
  }

  override toString() {
    return this.message;
  }
}

/** Normalize what a sample-market command rejected with. */
export function sampleMarketError(error: unknown): SampleMarketError {
  if (error instanceof SampleMarketError) return error;
  if (error && typeof error === "object" && "message" in error) {
    const { code, message } = error as { code?: unknown; message?: unknown };
    return new SampleMarketError(typeof code === "string" ? code : null, String(message));
  }
  return new SampleMarketError(null, String(error));
}

async function sampleCommand<T>(run: () => Promise<T>): Promise<T> {
  try {
    return await run();
  } catch (error) {
    throw sampleMarketError(error);
  }
}

export interface SampleMarketAddResult {
  status: "ok";
  provider: string;
  provider_name: string;
  track: string;
  receipt: {
    id: string;
    name: string;
    file: string;
    path: string;
    bytes: number;
    license: SampleMarketItem["license"];
    attribution: string;
    quality: string;
  };
  snapshot: ProjectSnapshot;
}

/** Whether Freesound can serve original files. No credential is included. */
export interface FreesoundOAuthStatus {
  state: "connected" | "expired" | "needs_reauth";
  expires_at: number | null;
  has_refresh_token: boolean;
  client_configured: boolean;
}

/** Markets and their availability, without searching anything. */
export const sampleMarketProviders = () =>
  invoke<SampleProviderAvailability[]>("sample_market_providers");

/** Search every named market, or every available one. */
export const sampleMarketSearch = (args: {
  query: string;
  kind?: string;
  min_seconds?: number;
  max_seconds?: number;
  channels?: number;
  tags?: string[];
  providers?: string[];
  offset?: number;
  limit?: number;
  commercial_ok?: boolean;
  cursor?: string;
}) => sampleCommand(() => invoke<SampleMarketSearchResult>("sample_market_search", { args }));

type SampleMarketSearchArgs = Parameters<typeof sampleMarketSearch>[0];

/** The search the panel last submitted; load-more continues exactly this. */
export interface SampleMarketSearchFilters {
  query: string;
  kind?: string;
  min_seconds?: number;
  max_seconds?: number;
  commercial_ok?: boolean;
  providers?: string[];
}

function searchArguments(
  filters: SampleMarketSearchFilters,
  cursor?: string,
): SampleMarketSearchArgs {
  const args: SampleMarketSearchArgs = { query: filters.query };
  if (filters.kind) args.kind = filters.kind;
  if (filters.commercial_ok) args.commercial_ok = true;
  if (filters.min_seconds !== undefined) args.min_seconds = filters.min_seconds;
  if (filters.max_seconds !== undefined) args.max_seconds = filters.max_seconds;
  if (filters.providers) args.providers = filters.providers;
  if (cursor) args.cursor = cursor;
  return args;
}

function normalizedSourceUrl(value: string): string {
  const text = value.trim();
  if (!text) return "";
  try {
    const url = new URL(text);
    url.hash = "";
    const path = url.pathname.replace(/\/+$/, "");
    return `${url.protocol}//${url.host.toLowerCase()}${path}${url.search}`;
  } catch {
    return text;
  }
}

function itemKey(item: SampleMarketItem): string {
  return `${item.provider}\u0000${item.id}`;
}

/**
 * Append one page to the rows already shown. The backend removes duplicates
 * within a page; across pages the panel keeps each provider id and each
 * normalized source URL once.
 */
export function mergeSampleMarketItems(
  existing: SampleMarketItem[],
  incoming: SampleMarketItem[],
): SampleMarketItem[] {
  const ids = new Set(existing.map(itemKey));
  const sources = new Set(
    existing.map((item) => normalizedSourceUrl(item.source_url)).filter(Boolean),
  );
  const merged = [...existing];
  for (const item of incoming) {
    const id = itemKey(item);
    const source = normalizedSourceUrl(item.source_url);
    if (ids.has(id) || (source && sources.has(source))) continue;
    ids.add(id);
    if (source) sources.add(source);
    merged.push(item);
  }
  return merged;
}

export interface SampleMarketSearchPager {
  items: Accessor<SampleMarketItem[]>;
  cursor: Accessor<string | null>;
  loading: Accessor<boolean>;
  loadingMore: Accessor<boolean>;
  /** Start a new search; a stale answer is dropped. */
  search: (filters: SampleMarketSearchFilters) => Promise<SampleMarketSearchResult | null>;
  /** Continue the current search; a stale answer is dropped. */
  loadMore: () => Promise<SampleMarketSearchResult | null>;
  /** Drop the current search and every in-flight answer. */
  reset: () => void;
}

/** A search that pages through cursors, with stale-result and duplicate guards. */
export function createSampleMarketSearchPager(
  run: (args: SampleMarketSearchArgs) => Promise<SampleMarketSearchResult> = sampleMarketSearch,
): SampleMarketSearchPager {
  const [items, setItems] = createSignal<SampleMarketItem[]>([]);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [loadingMore, setLoadingMore] = createSignal(false);
  let token = 0;
  let filters: SampleMarketSearchFilters | null = null;

  async function search(next: SampleMarketSearchFilters) {
    const mine = ++token;
    filters = next;
    setItems([]);
    setCursor(null);
    setLoading(true);
    setLoadingMore(false);
    try {
      const result = await run(searchArguments(next));
      if (mine !== token) return null;
      setItems(result.items);
      setCursor(result.cursor ?? null);
      return result;
    } catch (error) {
      if (mine !== token) return null;
      throw error;
    } finally {
      if (mine === token) setLoading(false);
    }
  }

  async function loadMore() {
    const next = cursor();
    if (!filters || next === null || loading() || loadingMore()) return null;
    const mine = token;
    setLoadingMore(true);
    try {
      const result = await run(searchArguments(filters, next));
      if (mine !== token) return null;
      setItems((previous) => mergeSampleMarketItems(previous, result.items));
      setCursor(result.cursor ?? null);
      return result;
    } catch (error) {
      if (mine !== token) return null;
      throw error;
    } finally {
      if (mine === token) setLoadingMore(false);
    }
  }

  function reset() {
    token += 1;
    filters = null;
    setItems([]);
    setCursor(null);
    setLoading(false);
    setLoadingMore(false);
  }

  return { items, cursor, loading, loadingMore, search, loadMore, reset };
}

/** Play one result through the audition stream; `cancelled` means a Stop or a
 * newer preview replaced it while it downloaded. */
export const samplePreview = (provider: string, id: string, label: string, startPaused: boolean) =>
  startNativeAudio(() => sampleCommand(() => invoke<{
    status: "ok" | "cancelled";
    provider: string;
    id: string;
    name: string;
    seconds: number;
  }>("sample_preview", { provider, id, label, startPaused })));

/** Download one result and place it at the playhead. */
export const sampleMarketAdd = (
  provider: string,
  id: string,
  track: string | null,
  acceptLicence = false,
) =>
  sampleCommand(() => invoke<SampleMarketAddResult>("sample_market_add", {
    provider,
    id,
    track,
    acceptLicence,
  }));

/** Save a market's API key in the OS keyring. */
export const sampleMarketSaveKey = (provider: string, key: string) =>
  invoke<{ status: "ok"; provider: string }>("sample_market_save_key", { provider, key });

export type SampleKeyStatus = "environment" | "saved" | "missing";

export const sampleMarketKeyStatus = (provider: string) =>
  invoke<SampleKeyStatus>("sample_market_key_status", { provider });

export const sampleMarketRemoveKey = (provider: string) =>
  invoke<void>("sample_market_remove_key", { provider });

/** The stored Freesound connection, without the token. */
export const freesoundOAuthStatus = () =>
  invoke<FreesoundOAuthStatus>("freesound_oauth_status");

/**
 * Run Freesound's authorization-code flow and wait for the browser callback.
 * Empty credentials reuse the ones already stored.
 */
export const freesoundOAuthBegin = (clientId: string, clientSecret: string) =>
  invoke<FreesoundOAuthStatus>("freesound_oauth_begin", { clientId, clientSecret });

/** Remove the stored token and client credentials. */
export const freesoundOAuthSignOut = () =>
  invoke<FreesoundOAuthStatus>("freesound_oauth_sign_out");

// ============================================================================
// Samples on this computer
// ============================================================================

/** One audio file in the app's sound folders. */
export interface LocalSample {
  /** Absolute path; preview and add pass it back. */
  path: string;
  name: string;
  /** The sound folder it was found in. */
  folder: string;
  /** The path below that folder, which the search matched. */
  relative: string;
  format: string;
  kind: string;
  /** Read from the header only when a duration bound asked for it. */
  duration_seconds: number | null;
}

export interface LocalSampleSearchResult {
  items: LocalSample[];
  /** More files match than `items` lists. */
  truncated: boolean;
  /** Sound folders that exist and were searched. */
  folders: number;
}

export interface LocalSampleAddResult {
  status: "ok";
  track: string;
  file: string;
  snapshot: ProjectSnapshot;
}

/** Search the sound folders with the panel's text, kind and duration. */
export const localSampleSearch = (filters: SampleMarketSearchFilters) =>
  sampleCommand(() => invoke<LocalSampleSearchResult>("local_sample_search", {
    args: {
      query: filters.query,
      kind: filters.kind,
      min_seconds: filters.min_seconds,
      max_seconds: filters.max_seconds,
    },
  }));

/** Play one local file through the audition stream. */
export const localSamplePreview = (path: string, label: string, startPaused: boolean) =>
  startNativeAudio(() => sampleCommand(() => invoke<{
    status: "ok" | "cancelled";
    provider: string;
    id: string;
    name: string;
    seconds: number;
  }>("local_sample_preview", { path, label, startPaused })));

/** Copy one local file into the project and place it at the playhead. */
export const localSampleAdd = (path: string, track: string | null) =>
  sampleCommand(() => invoke<LocalSampleAddResult>("local_sample_add", { path, track }));
