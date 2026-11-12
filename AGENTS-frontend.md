# AGENTS-frontend.md

Rules for the SolidJS UI in `crates/oc-gui/frontend/` and the VS Code extension
in `editors/vscode-ocp/`. The hard rules in [AGENTS.md](AGENTS.md) still apply.

## Stack

- SolidJS 1.9, Vite and TypeScript. Use signals, stores and `onCleanup`, not
  React hooks. Do not destructure `props`, because that breaks reactivity.
- Never start the dev server with a window or launch the app. Verify with
  `npx tsc -b --noEmit` and Vitest.

## Backend calls

- Every Tauri call goes through a typed wrapper in `src/stores/` (exported from
  `src/stores/api.ts`). Components never import `@tauri-apps/api/core` or call
  `invoke()` directly.
- Give each wrapper explicit argument and result types that match the Rust
  command. Turn backend errors into tagged codes at the wrapper. Never branch on
  English error text.

## Text and presentation

- Route every user-visible string through `i18n.t(...)`, including titles,
  `aria-label`s, placeholders, fallback states and errors. Dictionaries live in
  `src/stores/locales/`. Add each key to every locale.
- Do not show engine or DSL jargon in UI copy: `AST`, `@kit`,
  `@control_source`, thread or worker counts, PRNG seeds.
- Take device names, ranges, units and choices from the generated catalogs
  (`src/data/*.gen.ts`, produced from `oc-device-schema`). Never hard-code them.
- Use the shared parameter and pointer-gesture controls. Support keyboard
  access, visible focus, cancellation and release outside the control. Respect
  editing contexts for Space, Undo and other global shortcuts.
- Check long translations, narrow panels, every theme and supported UI scales.

## Hygiene

- No `console.log`, `debugger` or debug styling in committed code. Use
  `src/stores/log.ts`.
- Asynchronous UI work captures its target and checks it is still current
  before applying a response.

## Tests

- Vitest tests sit beside the code in `src/__tests__/`. The shared IPC mock in
  `src/__tests__/setup.ts` rejects unknown commands. Keep it that way: a test
  that needs a fallback states it explicitly.
- Test behavior, not just presence. A test that only checks `toBeTruthy()` on
  an element proves little.

## VS Code extension

- `editors/vscode-ocp/` is MIT OR Apache-2.0 and depends only on `ocp-lsp`.
- Never commit build output (`out/`, `*.vsix`).
