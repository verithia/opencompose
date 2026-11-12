/**
 * Components reach Tauri commands through `stores/api.ts` or `ai/api.ts`, never
 * by importing `@tauri-apps/api/core` or calling `invoke` directly. Those
 * wrappers own argument names and response types; a raw call can drift from the
 * Rust command without a type error.
 */
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(file);
    return entry.isFile() && /\.tsx?$/.test(entry.name) ? [file] : [];
  });
}

describe("component Tauri boundary", () => {
  it("keeps domain modules free of explicit any", () => {
    const roots = ["src/ai", "src/stores"];
    const offenders = roots
      .flatMap((root) => sourceFiles(path.join(process.cwd(), root)))
      .filter((file) => readFileSync(file, "utf8").split("\n").some((line) => {
        const code = line.trim();
        if (code.startsWith("//") || code.startsWith("*") || code.startsWith("/*")) return false;
        return /:\s*any\b|<any>|\bas\s+any\b/.test(line);
      }))
      .map((file) => file.slice(process.cwd().length + 1));
    expect(offenders).toEqual([]);
  });

  it("keeps stores independent of components", () => {
    const offenders = sourceFiles(path.join(process.cwd(), "src/stores"))
      .filter((file) => /from\s+["'][^"']*components\//.test(readFileSync(file, "utf8")))
      .map((file) => file.slice(process.cwd().length + 1));
    expect(offenders).toEqual([]);
  });

  it("keeps the API barrel free of command implementations", () => {
    const barrel = readFileSync(path.join(process.cwd(), "src/stores/api.ts"), "utf8");
    expect(barrel).not.toContain("@tauri-apps/api/core");
    expect(barrel).not.toMatch(/\binvoke\s*[<(]/);
  });

  it("re-exports every store API domain from the barrel", () => {
    const stores = path.join(process.cwd(), "src/stores");
    const barrel = readFileSync(path.join(stores, "api.ts"), "utf8");
    const missing = readdirSync(stores, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.endsWith("Api.ts") && entry.name !== "api.ts")
      .map((entry) => `./${entry.name.slice(0, -3)}`)
      .filter((domain) => !barrel.includes(`export * from "${domain}";`));
    expect(missing).toEqual([]);
  });

  it("routes every command through a typed API wrapper", () => {
    const offenders = sourceFiles(path.join(process.cwd(), "src/components"))
      .filter((path) => {
        const source = readFileSync(path, "utf8");
        return /from\s+["']@tauri-apps\/api\/core["']/.test(source)
          || /\binvoke\s*[<(]/.test(source);
      })
      .map((path) => path.slice(process.cwd().length + 1));
    expect(offenders).toEqual([]);
  });
});
