#!/usr/bin/env node
// One version, every manifest. Fails the build when any release-facing
// manifest drifts from crates/oc-gui/frontend/package.json.
//
//   node scripts/check-version.mjs            # manifests only
//   node scripts/check-version.mjs --bundle <OpenCompose.app>
//
// The bundle check reads the built Info.plist, so it is run by the smoke test
// after `tauri build` rather than by the frontend build itself.

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const FRONTEND = resolve(HERE, "..");
const ROOT = resolve(FRONTEND, "../../..");

const problems = [];
const ok = [];

function read(path) {
  return readFileSync(path, "utf8");
}

function tomlSectionValue(text, section, key) {
  let current = "";
  for (const raw of text.split("\n")) {
    const line = raw.replace(/#.*$/, "").trim();
    const header = line.match(/^\[([^\]]+)\]$/);
    if (header) {
      current = header[1].trim();
      continue;
    }
    if (current !== section) continue;
    const assignment = line.match(/^([^=]+)=(.*)$/);
    if (assignment?.[1].trim() === key) return assignment[2].trim();
  }
  return null;
}

function tomlSectionString(text, section, key) {
  return tomlSectionValue(text, section, key)?.match(/^"([^"]+)"$/)?.[1] ?? null;
}

function packageVersion(text, workspaceVersion) {
  const literal = tomlSectionString(text, "package", "version");
  if (literal !== null) return literal;
  return tomlSectionValue(text, "package", "version.workspace") === "true"
    ? workspaceVersion
    : null;
}

function record(label, actual, expected) {
  if (actual === expected) {
    ok.push(`${label}: ${actual}`);
  } else {
    problems.push(`${label}: expected ${expected}, found ${actual ?? "<missing>"}`);
  }
}

function expectFile(label, path, actual, expected) {
  if (!existsSync(path)) {
    problems.push(`${label}: missing file ${path}`);
    return;
  }
  record(label, actual, expected);
}

const pkgPath = join(FRONTEND, "package.json");
const pkg = JSON.parse(read(pkgPath));
const version = pkg.version;
if (!version) {
  console.error(`No version in ${pkgPath}`);
  process.exit(1);
}

// package-lock mirrors package.json twice: the root entry and packages[""].
const lockPath = join(FRONTEND, "package-lock.json");
if (existsSync(lockPath)) {
  const lock = JSON.parse(read(lockPath));
  expectFile("package-lock.json", lockPath, lock.version, version);
  record('package-lock.json packages[""]', lock.packages?.[""]?.version, version);
}

// Tauri reads the bundle version from here; Info.plist gets it from the CLI.
const confPath = join(FRONTEND, "src-tauri/tauri.conf.json");
const conf = JSON.parse(read(confPath));
expectFile("tauri.conf.json", confPath, conf.version, version);

// Cargo package versions can be declared locally or inherited from the workspace.
const rootManifestPath = join(ROOT, "Cargo.toml");
const rootManifest = read(rootManifestPath);
const workspaceVersion = tomlSectionString(rootManifest, "workspace.package", "version");
expectFile("Cargo.toml [workspace.package]", rootManifestPath, workspaceVersion, version);

const guiManifestPath = join(FRONTEND, "src-tauri/Cargo.toml");
const guiManifest = read(guiManifestPath);
expectFile("src-tauri/Cargo.toml", guiManifestPath, packageVersion(guiManifest, workspaceVersion), version);

const memberNames = [];
const cratesDir = join(ROOT, "crates");
for (const entry of readdirSync(cratesDir, { withFileTypes: true })) {
  if (!entry.isDirectory()) continue;
  const manifestPath = join(cratesDir, entry.name, "Cargo.toml");
  if (!existsSync(manifestPath)) continue;
  const text = read(manifestPath);
  const name = tomlSectionString(text, "package", "name");
  if (name) memberNames.push(name);
  record(`crates/${entry.name}/Cargo.toml`, packageVersion(text, workspaceVersion), version);
}
// The Tauri app manifest is nested beneath the frontend.
memberNames.push(tomlSectionString(guiManifest, "package", "name") ?? "oc-gui");

// Cargo.lock must agree with the manifests, otherwise a stale lock silently
// ships an old version in the built binary's package info.
const lockfilePath = join(ROOT, "Cargo.lock");
const lockVersions = new Map();
for (const block of read(lockfilePath).split("[[package]]")) {
  const name = block.match(/^\s*name\s*=\s*"([^"]+)"/m)?.[1];
  const blockVersion = block.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  if (name && blockVersion) lockVersions.set(name, blockVersion);
}
for (const name of [...new Set(memberNames)]) {
  const locked = lockVersions.get(name);
  if (locked === undefined) {
    problems.push(`Cargo.lock: no entry for workspace crate ${name}`);
    continue;
  }
  record(`Cargo.lock ${name}`, locked, version);
}

// Optional: the version a user actually sees in Finder and in the About panel.
const bundleIndex = process.argv.indexOf("--bundle");
if (bundleIndex !== -1) {
  const appPath = process.argv[bundleIndex + 1];
  if (!appPath) {
    console.error("--bundle needs the path to OpenCompose.app");
    process.exit(2);
  }
  if (process.platform !== "darwin") {
    console.error("--bundle is only supported on macOS");
    process.exit(2);
  }
  const plistPath = join(appPath, "Contents/Info.plist");
  if (!existsSync(plistPath)) {
    problems.push(`bundle: missing ${plistPath}`);
  } else {
    for (const key of ["CFBundleShortVersionString", "CFBundleVersion"]) {
      const result = spawnSync(
        "plutil",
        ["-extract", key, "raw", "-o", "-", plistPath],
        { encoding: "utf8" },
      );
      if (result.status !== 0) {
        problems.push(`bundle: ${key} unreadable (${result.stderr.trim()})`);
      } else {
        record(`bundle Info.plist ${key}`, result.stdout.trim(), version);
      }
    }
  }
}

if (problems.length > 0) {
  console.error(`Version check failed (source of truth: ${pkgPath} = ${version})`);
  for (const problem of problems) console.error(`  FAIL ${problem}`);
  process.exit(1);
}

console.log(`Version check: ${version} in ${ok.length} manifests`);
for (const line of ok) console.log(`  ok ${line}`);
