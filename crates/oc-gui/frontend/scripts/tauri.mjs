import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const workspace = fileURLToPath(new URL("../../../../", import.meta.url));
const frontend = fileURLToPath(new URL("../", import.meta.url));
const intel = "x86_64-apple-darwin";
const silicon = "aarch64-apple-darwin";
const universal = "universal-apple-darwin";
const helper = "opencompose-vst2-bridge-x86_64";

function cargo(args, localEnv, options = {}) {
  return execFileSync("cargo", args, { cwd: workspace, env: localEnv, ...options });
}

function targetTriple(options, localEnv) {
  for (const [index, arg] of options.entries()) {
    if (arg === "--target" || arg === "-t") return options[index + 1];
    if (arg.startsWith("--target=") || arg.startsWith("-t=")) return arg.split("=")[1];
  }
  return localEnv.CARGO_BUILD_TARGET
    || execFileSync("rustc", ["-vV"], { env: localEnv, encoding: "utf8" }).match(/^host: (.+)$/m)?.[1];
}

function stageHelper(target, debug, localEnv) {
  const profile = debug ? "debug" : "release";
  const { target_directory: targetDir } = JSON.parse(cargo(
    ["metadata", "--locked", "--format-version", "1", "--no-deps"],
    localEnv,
    { encoding: "utf8" },
  ));
  cargo([
    "build", "--locked", "-p", "oc-host", "--bin", helper, "--target", intel,
    ...(debug ? [] : ["--release"]),
  ], localEnv, { stdio: "inherit" });

  const directory = join(frontend, "src-tauri", "target", "plugin-sidecars", profile);
  mkdirSync(directory, { recursive: true });
  const source = join(targetDir, intel, profile, helper);
  const binary = readFileSync(source);
  // Under a universal build Cargo checks both slices and the bundler uses
  // its own target suffix. Each resolves to the same Intel executable.
  for (const triple of target === universal ? [silicon, intel, universal] : [target]) {
    const destination = join(directory, `${helper}-${triple}`);
    if (existsSync(destination) && readFileSync(destination).equals(binary)) continue;
    const staging = mkdtempSync(join(directory, ".stage-"));
    try {
      const temporary = join(staging, helper);
      copyFileSync(source, temporary);
      renameSync(temporary, destination);
    } finally {
      rmSync(staging, { recursive: true, force: true });
    }
  }
  return { bundle: { externalBin: [join(directory, helper)] } };
}

const args = process.argv.slice(2);
const env = { ...process.env };
try {
  const cargoSeparator = args.indexOf("--");
  const options = args.slice(1, cargoSeparator < 0 ? args.length : cargoSeparator);
  if (process.platform === "darwin"
    && ["build", "bundle"].includes(args[0])
    && !options.some((arg) => ["--help", "-h", "--no-bundle"].includes(arg))) {
    env.CXX ||= "clang++";
    const target = targetTriple(options, env);
    if (target === silicon || target === universal) {
      const debug = options.includes("--debug") || options.includes("-d");
      const config = stageHelper(target, debug, env);
      // Pass the sidecar config only to packaging. Plain Cargo builds and
      // tests remain independent of the Intel toolchain.
      args.splice(1, 0, "--config", JSON.stringify(config));
    }
  }
  const child = spawnSync(process.execPath, [require.resolve("@tauri-apps/cli/tauri.js"), ...args], {
    env,
    stdio: "inherit",
  });
  if (child.error) throw child.error;
  process.exitCode = child.status ?? 1;
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
