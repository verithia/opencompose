import { i18n, type I18nKey } from "../stores/i18n";

const ACCOUNTING_MESSAGES = {
  accounting_storage: "ai.accounting.storage",
  accounting_session_missing: "ai.accounting.sessionMissing",
  accounting_entropy: "ai.accounting.entropy",
  accounting_overflow: "ai.accounting.overflow",
  accounting_capacity: "ai.accounting.capacity",
  accounting_currency_capacity: "ai.accounting.currencyCapacity",
  accounting_attempt_capacity: "ai.accounting.attemptCapacity",
  accounting_format: "ai.accounting.format",
  accounting_request: "ai.accounting.request",
  accounting_busy: "ai.accounting.busy",
  accounting_session_changed: "ai.accounting.sessionChanged",
  accounting_owner_closed: "ai.accounting.ownerClosed",
  accounting_owner_changed: "ai.accounting.ownerChanged",
  accounting_conflict: "ai.accounting.conflict",
  accounting_cost: "ai.accounting.cost",
} as const satisfies Record<string, I18nKey>;

/** Only documented structured accounting codes enter this localization boundary. */
export function accountingErrorMessage(error: unknown): string | null {
  if (!error || typeof error !== "object") return null;
  const fields = error as { kind?: unknown; code?: unknown };
  for (const code of [fields.kind, fields.code]) {
    if (typeof code === "string" && Object.hasOwn(ACCOUNTING_MESSAGES, code)) {
      return i18n.t(ACCOUNTING_MESSAGES[code as keyof typeof ACCOUNTING_MESSAGES]);
    }
  }
  return null;
}

/** Tauri rejects with a serialized AiError object; normalize it for display
 * without ever rendering token material. */
export function errorMessage(error: unknown): string {
  const accounting = accountingErrorMessage(error);
  if (accounting !== null) return accounting;
  if (typeof error === "string") return error;
  if (error && typeof error === "object") {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return String(error);
}

export function errorKind(error: unknown): string | undefined {
  if (error && typeof error === "object") {
    const kind = (error as { kind?: unknown }).kind;
    if (typeof kind === "string") return kind;
  }
  return undefined;
}

/**
 * The provider layer's verdict on resending the turn. `undefined` when the
 * error did not carry one, which keeps Retry available for local failures.
 */
export function errorRetryable(error: unknown): boolean | undefined {
  if (error && typeof error === "object") {
    const retryable = (error as { retryable?: unknown }).retryable;
    if (typeof retryable === "boolean") return retryable;
  }
  return undefined;
}
