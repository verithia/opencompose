/**
 * Token-count grammar: plain integers and decimal suffixes (`128K`, `1.5M`).
 * The formatter only emits a suffix the value represents exactly, so a value
 * shown by a limit field parses back to the same number.
 */
import { describe, expect, it } from "vitest";
import { MAX_TOKEN_COUNT, formatTokenCount, parseTokenCount } from "../ai/tokens";

describe("parseTokenCount", () => {
  const accepted: [string, number][] = [
    ["1M", 1000000],
    ["1m", 1000000],
    ["128K", 128000],
    ["128k", 128000],
    ["1.5M", 1500000],
    ["0.25M", 250000],
    ["1 M", 1000000],
    ["  64K  ", 64000],
    ["1000000", 1000000],
    ["1048576", 1048576],
    ["32000", 32000],
    ["1G", 1000000000],
    ["3.3K", 3300],
    ["0.5K", 500],
    ["7", 7],
    [String(MAX_TOKEN_COUNT), MAX_TOKEN_COUNT],
  ];

  it.each(accepted)("parses %s as %d", (text, expected) => {
    expect(parseTokenCount(text)).toBe(expected);
  });

  const rejected = [
    "",
    "   ",
    "0",
    "0K",
    "0.0001K",
    "-5",
    "-1M",
    "abc",
    "K",
    "1.2.3M",
    "1.5",
    "64_000",
    "1,000",
    "1e6",
    "+5",
    "12KK",
    String(MAX_TOKEN_COUNT + 1),
  ];

  it.each(rejected)("rejects %j", (text) => {
    expect(parseTokenCount(text)).toBeNull();
  });
});

describe("formatTokenCount", () => {
  const cases: [number, string][] = [
    [1000000, "1M"],
    [128000, "128K"],
    [1500000, "1.5M"],
    [32000, "32K"],
    [1000000000, "1G"],
    [1048576, "1048576"],
    [256000, "256K"],
    [2000000, "2M"],
    [4294967295, "4294967295"],
  ];

  it.each(cases)("formats %d as %s", (value, expected) => {
    expect(formatTokenCount(value)).toBe(expected);
  });

  it("round-trips every suffixed form", () => {
    for (const text of ["1M", "128K", "1.5M", "1G", "256K"]) {
      const value = parseTokenCount(text)!;
      expect(formatTokenCount(value)).toBe(text);
      expect(parseTokenCount(formatTokenCount(value))).toBe(value);
    }
  });

  it("never loses precision for values with no exact suffix", () => {
    for (const value of [32000, 1000000, 1050000, 4294967295]) {
      expect(parseTokenCount(formatTokenCount(value))).toBe(value);
    }
  });
});
