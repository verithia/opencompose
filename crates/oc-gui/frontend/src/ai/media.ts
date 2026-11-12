/** Attachments for the AI panel: descriptors only, never inline bytes. */
import { invoke } from "@tauri-apps/api/core";
import type { MessageMeta } from "./messageProtocol";
import type { AiProvider } from "./config";
import type { CatalogModel } from "./catalog";
import type { InputModalities, ModelLimits } from "../stores/aiLimits";

export interface MediaAttachment {
  kind: "audio" | "image";
  name: string;
  path: string;
  mime: string;
  bytes: number;
  duration_secs: number | null;
  sha256: string;
  local_reference?: boolean;
  /** The file is gone; the chip says so and the next request skips it. */
  missing?: boolean;
}

/** One attachment re-hashed on send because the file changed on disk. */
export interface MediaNotice {
  name: string;
  path: string;
  sha256: string;
  /** The file was deleted; it was skipped and the text still went out. */
  missing?: boolean;
}

/** The backend accepts exactly these extensions, in this order. */
export const AUDIO_EXTENSIONS = ["wav", "mp3", "aif", "aiff", "aac", "ogg", "flac", "m4a", "opus"];
export const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "webp"];
export const MEDIA_EXTENSIONS = [...AUDIO_EXTENSIONS, ...IMAGE_EXTENSIONS];

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

/** `m:ss`; `null` when the container duration could not be read. */
export function formatDuration(seconds: number | null): string | null {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return null;
  const total = Math.round(seconds);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/** The catalog decides; a custom-endpoint model falls back to its saved flag. */
export function audioAccepted(model: CatalogModel | undefined, provider: AiProvider): boolean {
  return model ? model.capabilities.audio_in : provider.capabilities.audioIn;
}

/** Image input is opt-in through the merged model limits, like video. */
export function imageAccepted(limits: ModelLimits | null | undefined): boolean {
  return limits?.input.image === true;
}

/** The model's accepted input modalities, as the refusal names them. */
export function acceptedModalities(input: InputModalities | null | undefined): string {
  if (!input) return "text";
  const names = (["text", "image", "video", "audio"] as const).filter(name => input[name]);
  return names.length ? names.join(", ") : "text";
}

export function attachMedia(path: string): Promise<MediaAttachment> {
  return invoke<MediaAttachment>("ai_attach_media", { path });
}

/** Display only: never rename archive files or change the provider descriptor. */
export function auditionLabel(attachment: MediaAttachment, content: string, meta?: MessageMeta): { key: "ai.media.savedAuditionRange" | "ai.media.savedSourceAuditionRange" | "ai.media.savedAudition" | "ai.media.savedComparisonReference" | "ai.media.savedComparisonCandidate"; params?: Record<string, string> } | null {
  if (attachment.kind !== "audio" || !/[\\/]audition-media[\\/][a-f0-9]{64}\.(?:wav|mp3)$/i.test(attachment.path)) return null;
  if (meta) {
    if (meta.kind === "comparison_reference") return { key: "ai.media.savedComparisonReference" };
    if (meta.kind === "comparison_candidate") return { key: "ai.media.savedComparisonCandidate" };
    const range = meta.audio_range;
    if (range && Number.isFinite(range.start_seconds) && Number.isFinite(range.end_seconds) && range.end_seconds > range.start_seconds) {
      return { key: range.source ? "ai.media.savedSourceAuditionRange" : "ai.media.savedAuditionRange",
        params: { from: String(Number(range.start_seconds.toFixed(2))), to: String(Number(range.end_seconds.toFixed(2))) } };
    }
    return meta.kind === "audition" ? { key: "ai.media.savedAudition" } : null;
  }
  if (content.startsWith("A/B reference excerpt.")) return { key: "ai.media.savedComparisonReference" };
  if (content.startsWith("A/B candidate excerpt.")) return { key: "ai.media.savedComparisonCandidate" };
  const raw = /^You asked to hear source seconds ([0-9]+(?:\.[0-9]+)?)–([0-9]+(?:\.[0-9]+)?):/.exec(content);
  const match = raw ?? /^You asked to hear ([0-9]+(?:\.[0-9]+)?)–([0-9]+(?:\.[0-9]+)?) s: this is that render\./.exec(content);
  if (match && Number.isFinite(Number(match[1])) && Number.isFinite(Number(match[2])) && Number(match[2]) > Number(match[1])) {
    return {
      key: raw ? "ai.media.savedSourceAuditionRange" : "ai.media.savedAuditionRange",
      params: { from: String(Number(Number(match[1]).toFixed(2))), to: String(Number(Number(match[2]).toFixed(2))) },
    };
  }
  return { key: "ai.media.savedAudition" };
}

export function legacyAuditionText(content: string, explanation: string): string {
  return content.startsWith("You asked to hear ")
    ? content.replace("[audio clip not saved]", explanation)
    : content;
}
