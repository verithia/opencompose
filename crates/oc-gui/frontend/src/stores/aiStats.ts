/** A bounded projection of the backend's durable session accounting authority. */
import { createMemo, createSignal } from "solid-js";
import { observeUsage } from "./aiContext";
import { listen } from "@tauri-apps/api/event";
import { i18n } from "./i18n";
import type { AccountingError, AccountingUpdate, SessionUsageSnapshot, StatsEvent } from "../ai/sessions";

export interface StatsUsage {
  input: number;
  /** `null` when the provider sent no cache field at all. */
  cached_input: number | null;
  output: number;
  reasoning: number;
}

/** What one turn cost, in the currency its price is published in. */
export interface TurnCost {
  amount: number;
  currency: string;
}

export interface TurnStats {
  model: string;
  wire: string;
  request_id: string | null;
  retries: number;
  ttft_ms: number | null;
  decode_ms: number | null;
  wall_ms: number;
  decode_tps: number | null;
  wall_tps: number | null;
  usage: StatsUsage;
  failed: boolean;
  /** `null` when no price is known for the model: the row is omitted, not zero. */
  cost: TurnCost | null;
}

/** Presentation projection only; totals are never accumulated in the browser. */
export interface SessionStats {
  turns: number; attempts: number; input: number; output: number; reasoning: number;
  cacheInput: number; cachedInput: number; ttftSum: number; ttftCount: number;
  tpsSum: number; tpsCount: number;
  costs: Record<string, { amount: number; attempts: number }>;
  unknownPrice: number; unreported: number; interrupted: number; legacyUnknown: boolean;
  pending: number; known: boolean;
}

export interface FooterStats {
  ttftMs: number | null;
  decodeTps: number | null;
  cachePct: number | null;
  input: number | null;
  output: number | null;
}

const [lastTurn, setLastTurn] = createSignal<TurnStats | null>(null);
const [snapshot, setSnapshot] = createSignal<SessionUsageSnapshot | null>(null);
const [warning, setWarning] = createSignal<AccountingError | null>(null);
const [activeSession, setActiveSession] = createSignal<string | null>(null);
let eventEpoch = 0;
// A successful read at the failed write's revision proves only the old ledger.
let pendingWriteWarning: { incarnation: string; revision: number; error: AccountingError } | null = null;
interface Estimate { requestId: string; sessionId: string | null; tokens: number; consumed: boolean }
let estimate: Estimate | null = null;

const session = createMemo<SessionStats>(() => {
  const saved = snapshot();
  const total = saved?.totals;
  return {
    turns: total?.requests ?? 0, attempts: total?.attempts ?? 0,
    input: total?.input ?? 0, output: total?.output ?? 0, reasoning: total?.reasoning ?? 0,
    cacheInput: total?.cache_input ?? 0, cachedInput: total?.cached_input ?? 0,
    ttftSum: total?.ttft_ms_sum ?? 0, ttftCount: total?.ttft_count ?? 0,
    tpsSum: total?.decode_tps_sum ?? 0, tpsCount: total?.decode_tps_count ?? 0,
    costs: total?.costs ?? {}, unknownPrice: total?.unknown_price_attempts ?? 0,
    unreported: total?.unreported_usage_attempts ?? 0, interrupted: total?.interrupted_requests ?? 0,
    legacyUnknown: total?.legacy_unknown ?? activeSession() !== null,
    pending: saved?.pending_requests ?? 0, known: saved !== null,
  };
});

export function selectStatsSession(id: string | null): void {
  if (activeSession() === id) return;
  setActiveSession(id); setSnapshot(null); setLastTurn(null); setWarning(null);
  pendingWriteWarning = null;
  resetSentEstimate(); eventEpoch += 1;
}

export interface UsageHydration { sessionId: string; epoch: number }
// At most one paint is pending. Its update is not a displayed session or a receipt ledger.
let pendingHydration: { ticket: UsageHydration; update: AccountingUpdate | null } | null = null;
export function beginUsageHydration(sessionId: string): UsageHydration {
  const ticket = { sessionId, epoch: eventEpoch };
  pendingHydration = { ticket, update: null };
  return ticket;
}
export function cancelUsageHydration(ticket: UsageHydration): void {
  if (pendingHydration?.ticket === ticket) pendingHydration = null;
}
export function usageHydrationNeedsReconcile(ticket: UsageHydration, usage?: SessionUsageSnapshot): boolean {
  const pending = pendingHydration?.ticket === ticket ? pendingHydration.update : null;
  return pending !== null && (!usage || pending.incarnation !== usage.incarnation);
}
function retainHydrationUpdate(update: AccountingUpdate): void {
  if (pendingHydration?.ticket.sessionId !== update.session_id) return;
  const previous = pendingHydration.update;
  if (!previous || previous.incarnation !== update.incarnation || update.revision > previous.revision
      || (update.revision === previous.revision && !update.saved)) pendingHydration.update = update;
}

/** Called only after the panel's existing scope/paint acceptance gates. */
export function hydrateSessionUsage(
  id: string, usage?: SessionUsageSnapshot, error?: AccountingError, started?: UsageHydration,
): void {
  if (activeSession() !== id || (started && started.sessionId !== id)) return;
  const current = snapshot();
  const raced = current !== null && started !== undefined && started.epoch !== eventEpoch;
  if (usage && usage.session_id !== id) return;
  if (current && usage && current.incarnation !== usage.incarnation && raced) return;
  if (current && !usage) {
    if (!raced && error) setWarning(error);
    return;
  }
  if (current && usage && current.incarnation === usage.incarnation && usage.revision < current.revision) return;
  const newerDurable = current !== null && usage !== undefined
    && current.incarnation === usage.incarnation && usage.revision > current.revision;
  const replaced = current !== null && usage !== undefined && current.incarnation !== usage.incarnation;
  if (replaced) resetSentEstimate();
  if (usage && (!current || current.incarnation !== usage.incarnation || usage.revision > current.revision)) {
    setSnapshot(usage); setLastTurn(usage.last_turn ?? null);
  }
  if (usage && pendingWriteWarning && (usage.incarnation !== pendingWriteWarning.incarnation
      || usage.revision > pendingWriteWarning.revision)) pendingWriteWarning = null;
  if (!raced || (newerDurable && !error)) setWarning(error ?? pendingWriteWarning?.error ?? null);
  const held = started && pendingHydration?.ticket === started ? pendingHydration.update : null;
  if (started) cancelUsageHydration(started);
  // Only the accepted durable read establishes this incarnation's identity.
  if (held && usage?.incarnation === held.incarnation) acceptAccounting(held);
}

/** Persistence status is processed independently of totals revision deduplication. */
export function acceptAccounting(update: AccountingUpdate, admitted = false): boolean {
  if (update.snapshot.session_id !== update.session_id || update.snapshot.incarnation !== update.incarnation
      || update.snapshot.revision !== update.revision) return false;
  if (update.session_id !== activeSession()) { retainHydrationUpdate(update); return false; }
  const current = snapshot();
  if (!current && !admitted) { retainHydrationUpdate(update); return false; }
  // Request admission can establish an empty projection, never replace an
  // incarnation already established by an accepted session read.
  if (current && current.incarnation !== update.incarnation) return false;
  if (current?.incarnation === update.incarnation && update.revision < current.revision) return false;
  eventEpoch += 1;
  if (!update.saved) {
    const error = update.error ?? { code: "accounting_unsaved", message: "" };
    pendingWriteWarning = { incarnation: update.incarnation, revision: update.revision, error };
    setWarning(error);
  }
  if (!current || current.incarnation !== update.incarnation || update.revision > current.revision) {
    setSnapshot(update.snapshot);
    setLastTurn(update.snapshot.last_turn ?? null);
    if (update.saved) { pendingWriteWarning = null; setWarning(null); }
  }
  return true;
}

export function recordTurn(event: StatsEvent): void {
  const own = estimate !== null && estimate.requestId === event.client_request_id
    && estimate.sessionId === activeSession();
  if (event.accounting && !acceptAccounting(event.accounting, own)) return;
  if (event.purpose !== "chat" || !own) return;
  if (event.accounting && event.accounting.session_id !== activeSession()) return;
  if (estimate && !estimate.consumed) {
    // Durable acknowledgements may concern an older request than the ledger's
    // last chat. Only the backend snapshot owns their footer ordering.
    if (!event.accounting?.saved) setLastTurn(event);
    if (!event.accounting) setWarning({ code: "accounting_ephemeral", message: "" });
    estimate.consumed = true;
    // Cached input is already included in the provider's inclusive input count.
    observeUsage(event.usage.input, estimate.tokens);
  }
}

/** One reachable provider request, replaced by the next local dispatch. */
export function noteSentEstimate(tokens: number, requestId: string, sessionId: string | null): void {
  estimate = { tokens, requestId, sessionId, consumed: false };
}
export function resetSentEstimate(): void { estimate = null; }
export function resetStats(): void {
  resetSentEstimate(); pendingHydration = null; pendingWriteWarning = null; setActiveSession(null); setLastTurn(null); setSnapshot(null); setWarning(null); eventEpoch += 1;
}

let listenerUsers = 0;
let listenerGeneration = 0;
let unlisten: (() => void) | null = null;
/** The panel owns the listener; presentation remounts never clear accounting. */
export function startAiStatsSession(): () => void {
  listenerUsers += 1;
  if (listenerUsers === 1) {
    const generation = ++listenerGeneration;
    void listen<StatsEvent>("ai_stats", event => recordTurn(event.payload)).then(stop => {
      if (listenerUsers === 0 || generation !== listenerGeneration) stop();
      else unlisten = stop;
    }).catch(() => { /* A browser preview has no backend event bridge. */ });
  }
  let closed = false;
  return () => {
    if (closed) return;
    closed = true; listenerUsers -= 1;
    if (listenerUsers === 0) { listenerGeneration += 1; unlisten?.(); unlisten = null; }
  };
}

/** < 1000 ms stays in ms; longer values switch to one decimal of seconds. */
export function formatDuration(ms: number): string {
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/** 1.2k / 12.3k / 1.2M; anything below 1000 stays an integer. */
export function formatTokens(tokens: number): string {
  if (tokens < 1000) return `${tokens}`;
  if (tokens < 1_000_000) return `${(tokens / 1000).toFixed(1)}k`;
  return `${(tokens / 1_000_000).toFixed(1)}M`;
}

export function formatTps(tps: number): string {
  return tps.toFixed(1);
}

/** A request is usually worth a fraction of a cent, so small amounts keep four
 * decimals. The currency comes from the price the cost was computed with. */
export function formatCost(amount: number, currency: string): string {
  const digits = Math.abs(amount) < 0.01 ? 4 : 2;
  try {
    return new Intl.NumberFormat(i18n.locale(), {
      style: "currency",
      currency,
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    }).format(amount);
  } catch {
    // An unknown code still prints as a prefix rather than throwing.
    return `${currency} ${amount.toFixed(digits)}`;
  }
}

/** `null` unless the provider reported a cache field and the input is known. */
export function cacheHitPercent(usage: StatsUsage): number | null {
  if (usage.cached_input === null || usage.input <= 0) return null;
  return Math.min(100, Math.round((usage.cached_input / usage.input) * 100));
}

/** What the footer may show for one turn; absent measurements stay `null`. */
export function footerStats(turn: TurnStats | null): FooterStats {
  if (!turn) {
    return { ttftMs: null, decodeTps: null, cachePct: null, input: null, output: null };
  }
  const hasTokens = turn.usage.input > 0 || turn.usage.output > 0;
  return {
    ttftMs: turn.ttft_ms,
    decodeTps: turn.decode_tps,
    cachePct: cacheHitPercent(turn.usage),
    input: hasTokens ? turn.usage.input : null,
    output: hasTokens ? turn.usage.output : null,
  };
}

export function averageTtft(stats: SessionStats): number | null {
  return stats.ttftCount > 0 ? stats.ttftSum / stats.ttftCount : null;
}

export function averageTps(stats: SessionStats): number | null {
  return stats.tpsCount > 0 ? stats.tpsSum / stats.tpsCount : null;
}

export function sessionCachePercent(stats: SessionStats): number | null {
  return stats.cacheInput > 0 ? Math.min(100, Math.round((stats.cachedInput / stats.cacheInput) * 100)) : null;
}

export const aiStats = {
  turn: lastTurn,
  session,
  recordTurn,
  reset: resetStats,
  snapshot, warning, activeSession,
};
