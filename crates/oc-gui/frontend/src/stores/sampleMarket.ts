/**
 * Sample-market and Freesound commands.
 *
 * This module owns the marketplace wire types; `api.ts` re-exports them so
 * callers keep their existing import path.
 */
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
