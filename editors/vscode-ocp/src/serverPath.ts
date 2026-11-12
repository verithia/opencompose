import * as fs from "fs";
import * as path from "path";

/** Resolve an explicit executable, a local workspace build, or the PATH command. */
export function resolveServerPath(
  configured: string | undefined,
  extensionPath: string,
  platform: NodeJS.Platform = process.platform,
): string {
  if (configured?.trim()) return configured.trim();
  const executable = platform === "win32" ? "ocp-lsp.exe" : "ocp-lsp";
  const root = path.resolve(extensionPath, "..", "..");
  if (fs.existsSync(path.join(root, "Cargo.toml"))) {
    for (const profile of ["debug", "release"]) {
      const candidate = path.join(root, "target", profile, executable);
      try {
        if (!fs.statSync(candidate).isFile()) continue;
        fs.accessSync(candidate, platform === "win32" ? fs.constants.F_OK : fs.constants.X_OK);
        return candidate;
      } catch {
        // A missing or nonexecutable build must not hide the PATH fallback.
      }
    }
  }
  return executable;
}
