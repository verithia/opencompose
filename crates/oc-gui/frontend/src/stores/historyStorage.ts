/**
 * The space the open project's edit history occupies, and the size past which
 * the History tab and the status bar offer to clear older entries. Nothing is
 * ever cleared without the person's action.
 */
import { createSignal } from "solid-js";
import type { HistoryInfo } from "../types/undo";
import * as api from "./api";

const NOTICE_KEY = "opencompose.history.noticeBytes";
/** Default size of the notice: 1 GiB. */
export const DEFAULT_NOTICE_BYTES = 1024 ** 3;

function storedNoticeBytes(): number {
  const stored = Number(globalThis.localStorage?.getItem(NOTICE_KEY));
  return Number.isFinite(stored) && stored > 0 ? stored : DEFAULT_NOTICE_BYTES;
}

const [info, setInfo] = createSignal<HistoryInfo | null>(null);
const [noticeBytes, setNoticeBytesSignal] = createSignal(storedNoticeBytes());

let request = 0;

/** Read the size again, e.g. after a project opened or the history was cleared. */
async function refresh(): Promise<void> {
  const own = ++request;
  try {
    const next = await api.historyInfo();
    if (own === request && next && typeof next.bytes === "number") setInfo(next);
  } catch { /* the next refresh answers */ }
}

/** Adopt a size a command already reported, without a second read. */
function adopt(next: HistoryInfo): void {
  if (!next || typeof next.bytes !== "number") return;
  request += 1;
  setInfo(next);
}

function setNoticeBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return;
  setNoticeBytesSignal(bytes);
  globalThis.localStorage?.setItem(NOTICE_KEY, String(bytes));
}

/** Whether the history has grown past the notice size. */
function overNotice(): boolean {
  const current = info();
  return !!current && current.journaled && current.bytes > noticeBytes();
}

export const historyStorage = { info, refresh, adopt, noticeBytes, setNoticeBytes, overNotice };
