import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";

/**
 * Embed the license texts the Help & About page shows, without exposing the
 * repository root through Vite. The third-party notices are large, so they
 * live in their own module that the page loads only when the reader opens them.
 */
export function bundledLicenses(): Plugin {
  const modules: Record<string, Record<string, URL>> = {
    "virtual:opencompose-licenses": {
      agplLicense: new URL("../../../../LICENSE", import.meta.url),
      mitLicense: new URL("../../../../LICENSE-MIT", import.meta.url),
      apacheLicense: new URL("../../../../LICENSE-APACHE", import.meta.url),
    },
    "virtual:opencompose-third-party": {
      thirdPartyNotices: new URL("../src-tauri/resources/THIRD_PARTY_LICENSES.txt", import.meta.url),
    },
  };
  return {
    name: "opencompose-bundled-licenses",
    resolveId(source) { return source in modules ? `\0${source}` : null; },
    load(source) {
      const files = source.startsWith("\0") ? modules[source.slice(1)] : undefined;
      if (!files) return null;
      return Object.entries(files).map(([name, path]) => {
        this.addWatchFile(fileURLToPath(path));
        return `export const ${name} = ${JSON.stringify(readFileSync(path, "utf8"))};`;
      }).join("\n");
    },
  };
}
