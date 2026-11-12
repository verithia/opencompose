/**
 * The shared catalog read: a forced reload supersedes one still in flight, and
 * only the newest read may publish its answer or free the slot the next
 * non-forced caller reuses.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "./setup";
import type { Catalog } from "../ai/catalog";
import { aiWorkspace } from "../stores/aiWorkspace";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

const reads: ReturnType<typeof deferred<Catalog>>[] = [];

function catalog(version: string): Catalog {
  return { version, providers: [] };
}

beforeEach(() => {
  reads.length = 0;
  invokeMock.mockReset();
  invokeMock.mockImplementation((command: string) => {
    if (command === "ai_catalog") return reads.shift()!.promise;
    return Promise.resolve(null);
  });
});

describe("the catalog read", () => {
  it("does not let a superseded read free the slot or publish its answer", async () => {
    const first = deferred<Catalog>();
    const second = deferred<Catalog>();
    reads.push(first, second, deferred<Catalog>());

    // Two forced reads overlap: the panel reloaded the catalog while the
    // previous read was still on its way.
    const stale = aiWorkspace.ensureCatalog(true);
    const newest = aiWorkspace.ensureCatalog(true);

    // The older read lands first. Its answer describes a catalog that the
    // second read is already replacing, so nothing is published.
    first.resolve(catalog("stale"));
    await stale;
    expect(aiWorkspace.catalog()).toBeNull();

    // The slot still holds the second read: a non-forced caller joins it
    // instead of starting a third request.
    const joined = aiWorkspace.ensureCatalog();
    expect(reads).toHaveLength(1);
    second.resolve(catalog("newest"));
    await Promise.all([newest, joined]);
    expect(aiWorkspace.catalog()?.version).toBe("newest");
  });

  it("keeps the newest answer when the older one lands last", async () => {
    // The first case left a catalog loaded, so both reads here are forced.
    const older = deferred<Catalog>();
    const newer = deferred<Catalog>();
    reads.push(older, newer);

    const olderRead = aiWorkspace.ensureCatalog(true);
    const newerRead = aiWorkspace.ensureCatalog(true);
    newer.resolve(catalog("newer"));
    await newerRead;
    older.resolve(catalog("older"));
    await olderRead;

    expect(aiWorkspace.catalog()?.version).toBe("newer");
  });
});
