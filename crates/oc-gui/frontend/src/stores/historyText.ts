/** How the footer and the History tab name an edit-history entry. */
import type { HistoryAuthor, HistoryBrief } from "../types/undo";
import { changeSetText, type ChangeSetOptions } from "./changeText";
import { i18n } from "./i18n";
import { sessionTitle } from "./sessionTitles";

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

/** What one history level did. */
export function entryText(
  entry: { changes: HistoryBrief["changes"] },
  options: ChangeSetOptions = {},
): string {
  return changeSetText(entry.changes, options);
}

/** "Change tempo from 120 to 128 BPM (Agent - Drum ideas)". */
export function briefText(brief: HistoryBrief, options: ChangeSetOptions = {}): string {
  const what = changeSetText(brief.changes, options);
  return `${what} (${authorName(brief.author)})`;
}

/** A byte count as the History tab and Storage show it. */
export function sizeText(bytes: number): string {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}
