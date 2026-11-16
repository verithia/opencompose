/** How the footer and the History tab name an edit-history entry. */
import type { HistoryAuthor, HistoryBrief } from "../types/undo";
import { i18n } from "./i18n";
import { sessionTitle } from "./sessionTitles";

const BATCH_PREFIX = "Transaction: ";

/** The operation's name: a batch names the turn, the rest name the edit. */
export function entryName(label: string): string {
  if (label.startsWith(BATCH_PREFIX)) {
    return i18n.t("history.op.batch", { label: label.slice(BATCH_PREFIX.length) });
  }
  if (label === "external change") return i18n.t("history.op.externalChange");
  return label.charAt(0).toUpperCase() + label.slice(1);
}

/** Who made the entry. */
export function authorName(author: HistoryAuthor): string {
  switch (author.kind) {
    case "person": return i18n.t("history.author.person");
    case "agent": {
      const title = author.session ? sessionTitle(author.session) : undefined;
      return title
        ? i18n.t("history.author.agentSession", { session: title })
        : i18n.t("history.author.agent");
    }
    case "client": return i18n.t("history.author.client", { owner: author.owner });
    case "outside": return i18n.t("history.author.outside");
  }
}

/** "Set tempo: 128 BPM (Agent - Drum ideas)". */
export function briefText(brief: HistoryBrief): string {
  const name = entryName(brief.label);
  const what = brief.subject ? `${name}: ${brief.subject}` : name;
  return `${what} (${authorName(brief.author)})`;
}

/** A byte count as the History tab and Storage show it. */
export function sizeText(bytes: number): string {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}
