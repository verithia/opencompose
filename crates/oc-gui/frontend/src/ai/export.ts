/**
 * Transcript export: the conversation as a Markdown document a person can read,
 * and as the stored wire plus its session meta. Both take the conversation the
 * panel already holds, so exporting neither posts a request nor writes a file
 * the user did not ask for.
 */
import type { WireMessage } from "./agent";
import { formatBytes } from "./media";
import type { SessionMeta } from "./sessions";
import type { ChatMessage } from "./transcript";

/** A single tool result larger than this is clipped, with a visible marker. */
export const RESULT_LIMIT_BYTES = 8 * 1024;

/**
 * An attachment is named, never inlined: a data URI and a bare base64 field are
 * replaced by a marker, so an export stays readable and small.
 */
const DATA_URI = /data:[\w.+-]+\/[\w.+-]+;base64,[A-Za-z0-9+/=]{8,}/g;
const BASE64_FIELD = /("base64"\s*:\s*")[A-Za-z0-9+/=]{120,}(")/g;

function scrubBlobs(text: string): string {
  return text.replace(DATA_URI, "[inline data omitted]").replace(BASE64_FIELD, "$1[omitted]$2");
}

/** The result body, clipped to {@link RESULT_LIMIT_BYTES} with a marker. */
export function boundedResult(text: string): string {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length <= RESULT_LIMIT_BYTES) return text;
  return `${decodeHead(bytes)}\n[truncated: ${formatBytes(bytes.length)} result, first ${formatBytes(RESULT_LIMIT_BYTES)} shown]`;
}

/**
 * The bytes that end on a character boundary at the cut. Slicing raw bytes can
 * land inside a multi-byte sequence, and decoding half a character writes a
 * replacement glyph into the export.
 */
function decodeHead(bytes: Uint8Array): string {
  let end = Math.min(RESULT_LIMIT_BYTES, bytes.length);
  // Walk back to the lead byte of the character the cut may sit inside.
  let start = end;
  while (start > 0 && (bytes[start - 1] & 0xc0) === 0x80) start -= 1;
  const lead = start > 0 ? bytes[start - 1] : 0;
  const width = lead >= 0xf0 ? 4 : lead >= 0xe0 ? 3 : lead >= 0xc0 ? 2 : 1;
  if (lead >= 0xc0 && start - 1 + width > end) end = start - 1;
  return new TextDecoder().decode(bytes.subarray(0, end));
}

function fence(text: string, language: string): string {
  return `\`\`\`${language}\n${text}\n\`\`\``;
}

function languageOf(text: string): string {
  try {
    JSON.parse(text);
    return "json";
  } catch {
    return "";
  }
}

/** A row's recorded time; a restored row has none and prints without one. */
function heading(label: string, row: ChatMessage): string {
  const time = row.timestamp ? new Date(row.timestamp) : null;
  const at = time && !Number.isNaN(time.getTime()) ? time.toISOString() : null;
  return at ? `## ${label} · ${at}` : `## ${label}`;
}

function attachmentBlock(row: ChatMessage): string {
  const media = row.media ?? [];
  if (!media.length) return "";
  const lines = media.map((item) => {
    const line = `- ${item.name} (${item.mime}, ${formatBytes(item.bytes)})`;
    return item.missing ? `${line} — the file is missing` : line;
  });
  return ["**Attachments**", ...lines].join("\n");
}

function diffBlock(row: ChatMessage): string {
  const diff = row.diff;
  if (!diff) return "";
  const lines: string[] = [];
  for (const hunk of diff.hunks) {
    for (const line of hunk.lines) {
      lines.push(`${line.kind === "Added" ? "+" : line.kind === "Removed" ? "-" : " "} ${line.text}`);
    }
  }
  return lines.length ? `**Proposed change**\n\n${fence(lines.join("\n"), "diff")}` : "";
}

function toolBlock(row: ChatMessage): string {
  const tool = row.tool;
  const parts = [heading(`Tool · ${tool?.name ?? "tool"}`, row)];
  if (tool?.args) parts.push("Arguments:", fence(scrubBlobs(tool.args), languageOf(tool.args)));
  parts.push(row.error ? "Result (failed):" : "Result:", fence(boundedResult(scrubBlobs(row.content)), languageOf(row.content)));
  return parts.join("\n\n");
}

function rowBlock(row: ChatMessage): string {
  if (row.role === "tool") return toolBlock(row);
  const label = row.checkpoint ? "Checkpoint"
    : row.context === "audition" ? "Audition"
    : row.role === "system" ? "System"
    : row.role === "assistant" ? "Assistant"
    : row.role === "context" ? "Context"
    : "User";
  const parts = [heading(label, row), row.content];
  const attachments = attachmentBlock(row);
  if (attachments) parts.push(attachments);
  const diff = diffBlock(row);
  if (diff) parts.push(diff);
  return parts.join("\n\n");
}

function header(meta: SessionMeta, rows: ChatMessage[]): string {
  const lines = [
    `# ${meta.title || "Session"}`,
    "",
    `- Workspace: ${meta.workspace ?? "detached"}`,
  ];
  if (meta.project_path) lines.push(`- Project: ${meta.project_path}`);
  lines.push(`- Provider: ${meta.provider || "unknown"} · ${meta.model || "unknown"}`);
  lines.push(`- Created: ${meta.created}`);
  lines.push(`- Updated: ${meta.updated}`);
  lines.push(`- Rows: ${rows.length}`);
  if (rows.some((row) => !row.timestamp)) {
    lines.push(
      "",
      "A row without a time was loaded from the saved conversation, which records time per turn rather than per message.",
    );
  }
  return lines.join("\n");
}

/**
 * The transcript as Markdown: every row in order, its time, each tool call with
 * its arguments and each result, and the compaction checkpoints between them.
 */
export function exportMarkdown(meta: SessionMeta, rows: ChatMessage[]): string {
  const blocks = [header(meta, rows), ...rows.map(rowBlock)];
  return `${scrubBlobs(blocks.join("\n\n"))}\n`;
}

/** The bytes a base64 string encodes, counted without decoding it. */
function base64Bytes(data: string): number {
  const padding = data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((data.length * 3) / 4) - padding);
}

/**
 * One stored message without its inline clip. The wire carries audition audio
 * as base64, and a JSON export is a document, not an archive: the clip is
 * replaced by its size so the file stays readable and small.
 */
function scrubAudio(message: WireMessage): WireMessage {
  const audio = message.input_audio;
  if (!audio || typeof audio.data !== "string") return message;
  return { ...message, input_audio: { ...audio, data: `[omitted: ${formatBytes(base64Bytes(audio.data))} of audio]` } };
}

/** The raw conversation the store holds, plus the session meta. */
export function exportJson(meta: SessionMeta, messages: WireMessage[]): string {
  return `${JSON.stringify({ version: 1, meta, messages: messages.map(scrubAudio) }, null, 2)}\n`;
}

/** A file named after the conversation, safe on every platform. */
export function exportFileName(meta: SessionMeta, extension: "md" | "json"): string {
  const title = (meta.title ?? "").replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ").replace(/\s+/g, " ").trim().slice(0, 80);
  return `${title || "session"}.${extension}`;
}

/** Puts the document on the clipboard; the browser reports a refusal. */
export async function copyToClipboard(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}

/** Downloads the document under `name` and releases the blob URL. */
export function downloadText(name: string, text: string, mime: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: mime }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  anchor.rel = "noopener";
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}
