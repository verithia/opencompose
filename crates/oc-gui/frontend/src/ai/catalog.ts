import { invoke } from "@tauri-apps/api/core";
import { WIRE_FOR_KIND, type ProviderKind, type Wire } from "./config";

export interface CatalogCapabilities {
  text: boolean;
  audio_in: boolean;
  audio_out: boolean;
  native_tool_calling: boolean;
  streaming: boolean;
}

/**
 * What a model's audio input accepts, from its model card. A model that says
 * nothing gets the conservative default: one channel at 16 kHz.
 */
export interface AudioInputSpec {
  channels: number;
  sample_rate: number;
  formats: string[];
}

export const DEFAULT_AUDIO_INPUT: AudioInputSpec = {
  channels: 1,
  sample_rate: 16_000,
  formats: ["wav", "mp3"],
};

export interface CatalogModel {
  id: string;
  name: string;
  context: number;
  max_output: number;
  thinking_levels?: string[];
  capabilities: CatalogCapabilities;
  audio_input?: AudioInputSpec;
  status: string;
  /** The date the vendor retires the model, for a `deprecated` row. */
  deprecated_on?: string;
}

export interface CatalogProvider {
  id: string;
  name: string;
  /** Protocol every model of this provider speaks. */
  wire?: Wire;
  /** Legacy field for catalog fixtures written before providers carried wires. */
  kind?: ProviderKind;
  /** Base URL and request path the backend builds requests from. */
  base_url?: string;
  path?: string;
  hosts: string[];
  auth_providers: string[];
  models: CatalogModel[];
}

/** The protocol a catalog entry serves. */
export function catalogWire(provider: CatalogProvider): Wire {
  return provider.wire ?? WIRE_FOR_KIND[provider.kind ?? "openai_chat_completions"];
}

export interface Catalog {
  version: string;
  providers: CatalogProvider[];
}

export function loadCatalog(): Promise<Catalog> {
  return invoke<Catalog>("ai_catalog");
}

export function modelsForWire(catalog: Catalog | null, wire: Wire): CatalogModel[] {
  if (!catalog) return [];
  return catalog.providers.filter(p => catalogWire(p) === wire).flatMap(p => p.models);
}

export function modelFor(
  catalog: Catalog | null,
  wire: Wire,
  id: string,
): CatalogModel | undefined {
  return modelsForWire(catalog, wire).find(model => model.id === id);
}

export function endpointHost(endpoint: string): string | undefined {
  try {
    return new URL(endpoint).hostname.toLowerCase() || undefined;
  } catch {
    return undefined;
  }
}

/** True when the endpoint is not one of the catalog's vendor hosts, so the
 * model id is the user's responsibility and free text is allowed. */
export function isCustomEndpoint(catalog: Catalog | null, endpoint: string): boolean {
  const host = endpointHost(endpoint);
  if (!host || !catalog) return true;
  if (host === "localhost" || host === "127.0.0.1" || host === "::1" || host === "[::1]") return true;
  return !catalog.providers.some(p => p.hosts.some(h => h.toLowerCase() === host));
}

interface ProviderAddress {
  wire?: Wire;
  kind?: ProviderKind;
  endpoint: string;
}

/** The catalog entry that serves this saved provider: the same protocol on a
 * declared host. Custom endpoints have none. */
export function catalogProviderFor(catalog: Catalog | null, provider: ProviderAddress): CatalogProvider | undefined {
  const host = endpointHost(provider.endpoint);
  if (!host || !catalog) return undefined;
  const wire = provider.wire ?? WIRE_FOR_KIND[provider.kind ?? "openai_chat_completions"];
  return catalog.providers.find(p =>
    catalogWire(p) === wire && p.hosts.some(h => h.toLowerCase() === host));
}
