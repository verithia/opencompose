const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const { resolveServerPath } = require("../out/serverPath.js");

function workspace(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ocp-extension-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, "Cargo.toml"), "[workspace]\n");
  return { root, extension: path.join(root, "editors", "vscode-ocp") };
}

function binary(root, profile, name, mode = 0o755) {
  const file = path.join(root, "target", profile, name);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "fixture", { mode });
  return file;
}

test("explicit configuration takes precedence", () => {
  assert.equal(resolveServerPath("/custom location/ocp-lsp", "/extension"), "/custom location/ocp-lsp");
});
test("missing builds fall back to PATH on every platform", (t) => {
  const { extension } = workspace(t);
  for (const platform of ["darwin", "linux", "win32"]) {
    assert.equal(resolveServerPath("", extension, platform), platform === "win32" ? "ocp-lsp.exe" : "ocp-lsp");
  }
});
test("local builds resolve from the repository root", (t) => {
  const { root, extension } = workspace(t);
  const release = binary(root, "release", "ocp-lsp");
  assert.equal(resolveServerPath(undefined, extension, "linux"), release);
  const debug = binary(root, "debug", "ocp-lsp");
  assert.equal(resolveServerPath(undefined, extension, "linux"), debug);
});
test("nonexecutable files and directories do not hide PATH", (t) => {
  const { root, extension } = workspace(t);
  binary(root, "debug", "ocp-lsp", 0o644);
  fs.mkdirSync(path.join(root, "target", "release", "ocp-lsp"), { recursive: true });
  if (process.platform !== "win32") assert.equal(resolveServerPath(undefined, extension, "linux"), "ocp-lsp");
});
test("Windows build discovery uses the executable suffix", (t) => {
  const { root, extension } = workspace(t);
  const file = binary(root, "debug", "ocp-lsp.exe", 0o644);
  assert.equal(resolveServerPath(undefined, extension, "win32"), file);
});
test("directive highlighting follows the owning grammar", () => {
  const grammar = fs.readFileSync(path.resolve(__dirname, "../../../crates/oc-dsl/src/grammar.pest"), "utf8");
  const syntax = JSON.parse(fs.readFileSync(path.join(__dirname, "../syntaxes/ocp.tmLanguage.json"), "utf8"));
  const keyword = new RegExp(`^(?:${syntax.repository.keywords.match})$`);
  const directives = new Set(Array.from(grammar.matchAll(/"(@[a-z_]+)"/g), ([, token]) => token));
  for (const directive of directives) assert.match(directive, keyword, directive);
  assert.ok(directives.size >= 18);
  assert.doesNotMatch("@123", keyword);
  assert.doesNotMatch("@project_suffix", keyword);
});
