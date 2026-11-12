import { afterEach, describe, expect, it } from "vitest";
import { i18n } from "../stores/i18n";
import { accountingErrorMessage, errorKind, errorMessage } from "../ai/errors";

describe("AI command errors", () => {
  it("reads the serialized AiError without exposing anything else", () => {
    const error = { kind: "auth_expired", message: "Authentication expired. Sign in again.", reauth: true };
    expect(errorMessage(error)).toBe("Authentication expired. Sign in again.");
    expect(errorKind(error)).toBe("auth_expired");
    expect(errorMessage("plain error")).toBe("plain error");
    expect(errorKind("plain error")).toBeUndefined();
    expect(errorMessage(undefined)).toBe("undefined");
  });
});

const accountingCodes = [
  "accounting_storage", "accounting_session_missing", "accounting_entropy", "accounting_overflow",
  "accounting_capacity", "accounting_currency_capacity", "accounting_attempt_capacity", "accounting_format", "accounting_request", "accounting_busy",
  "accounting_session_changed", "accounting_owner_closed", "accounting_owner_changed", "accounting_conflict", "accounting_cost",
] as const;

afterEach(() => i18n.setLocale("en"));
describe("structured accounting errors", () => {
  it.each(["en", "zh-Hans", "zh-Hant", "ja", "es"] as const)("localizes every owning code and kind in %s", locale => {
    i18n.setLocale(locale);
    for (const code of accountingCodes) {
      const fromCode = accountingErrorMessage({ code, message: "RAW OS DIAGNOSTIC" });
      expect(fromCode).toBeTruthy();
      expect(fromCode).not.toContain("RAW OS DIAGNOSTIC");
      expect(fromCode).not.toContain("ai.accounting.");
      expect(errorMessage({ kind: code, message: "RAW OS DIAGNOSTIC" })).toBe(fromCode);
      expect(errorMessage({ code, message: "RAW OS DIAGNOSTIC" })).toBe(fromCode);
    }
  });
  it("distinguishes inaccessible storage, unsupported data, currencies, attempts and busy receipts", () => {
    i18n.setLocale("en");
    expect(errorMessage({ code: "accounting_storage" })).toContain("could not be accessed");
    expect(errorMessage({ code: "accounting_format" })).toContain("storage limit");
    expect(errorMessage({ code: "accounting_currency_capacity" })).toContain("too many currencies");
    expect(errorMessage({ code: "accounting_attempt_capacity" })).toContain("attempt limit");
    expect(errorMessage({ code: "accounting_busy" })).toContain("Wait and try again");
  });
  it("preserves nonaccounting errors without guessing from strings or prefixes", () => {
    expect(errorMessage("accounting_storage: RAW OS DIAGNOSTIC")).toBe("accounting_storage: RAW OS DIAGNOSTIC");
    expect(errorMessage({ kind: "config", message: "accounting_storage: RAW OS DIAGNOSTIC" })).toBe("accounting_storage: RAW OS DIAGNOSTIC");
    expect(errorMessage({ kind: "accounting_storage_extra", message: "unchanged" })).toBe("unchanged");
    expect(errorMessage({ code: "toString", message: "unchanged" })).toBe("unchanged");
    expect(accountingErrorMessage({ code: "other", message: "unknown" })).toBeNull();
  });
});
