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

/** What one market answered, or why it could not. */
export interface SampleMarketPage {
  provider: string;
  provider_name: string;
  count: number;
  total?: number | null;
  next_offset?: number | null;
  error?: string | null;
}

export interface SampleMarketSearchResult {
  status: "ok";
  query: string;
  kind: string | null;
  offset: number;
  limit: number;
  providers: SampleProviderAvailability[];
  pages: SampleMarketPage[];
  count: number;
  next_offset: number | null;
  items: SampleMarketItem[];
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
}) => invoke<SampleMarketSearchResult>("sample_market_search", { args });

/** Play one result through the audition stream; `cancelled` means a Stop or a
 * newer preview replaced it while it downloaded. */
export const samplePreview = (provider: string, id: string, label: string, startPaused: boolean) =>
  startNativeAudio(() => invoke<{
    status: "ok" | "cancelled";
    provider: string;
    id: string;
    name: string;
    seconds: number;
  }>("sample_preview", { provider, id, label, startPaused }));

/** Download one result and place it at the playhead. */
export const sampleMarketAdd = (
  provider: string,
  id: string,
  track: string | null,
  acceptLicence = false,
) =>
  invoke<SampleMarketAddResult>("sample_market_add", {
    provider,
    id,
    track,
    acceptLicence,
  });

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
