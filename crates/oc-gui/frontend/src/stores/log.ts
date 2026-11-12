/**
 * Frontend logging system — stores recent log entries for debugging.
 * Visible via console and an optional in-app log panel.
 */
import { createSignal } from "solid-js";

export type LogLevel = "debug" | "info" | "warn" | "error";

export interface LogEntry {
  time: number;
  level: LogLevel;
  source: string;
  message: string;
}

const MAX_ENTRIES = 200;
const [entries, setEntries] = createSignal<LogEntry[]>([]);

function addEntry(level: LogLevel, source: string, message: string) {
  const entry: LogEntry = { time: Date.now(), level, source, message };

  // Console output
  const prefix = `[${source}]`;
  switch (level) {
    case "debug": console.debug(prefix, message); break;
    case "info": console.info(prefix, message); break;
    case "warn": console.warn(prefix, message); break;
    case "error": console.error(prefix, message); break;
  }

  setEntries((prev) => {
    const next = [...prev, entry];
    return next.length > MAX_ENTRIES ? next.slice(-MAX_ENTRIES) : next;
  });
}

export const log = {
  entries,
  debug: (source: string, msg: string) => addEntry("debug", source, msg),
  info: (source: string, msg: string) => addEntry("info", source, msg),
  warn: (source: string, msg: string) => addEntry("warn", source, msg),
  error: (source: string, msg: string) => addEntry("error", source, msg),
  clear: () => setEntries([]),
};
