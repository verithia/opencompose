# Branding

The OpenCompose mark combines an open ring, a note with a leaf-shaped flag,
and two short resonance lines. The same vector drawing supplies the in-app
mark and the desktop and web icons.

## Toolbar mark

- `src/assets/logo/mark.svg` is the source artwork. The toolbar, onboarding,
  and workspace chooser use it as an alpha mask tinted with `currentColor`,
  so it follows each surface's text color in every theme.
- The toolbar draws the mark at 30 px. The vector stays sharp at different UI
  scales and on high-density displays.
- Window chrome: the window uses an **overlay title bar** (`titleBarStyle:
  "Overlay"`, `hiddenTitle: true`, traffic lights at 16/14), so there is no
  native title strip and no boundary above the toolbar — the bar's own surface
  runs to the top edge and the traffic lights float over it. The bar reserves
  an 82 px left gutter on macOS (`:root[data-platform="macos"]`) and drags the
  window from its background and from the brand block (`data-tauri-drag-region`,
  `"deep"` on the brand so the mark and title drag too; buttons are excluded by
  the runtime's clickable-element check).
- Placement: the wordmark lives in a **top-left block that spans the toolbar
  height plus the timeline ruler** (`--toolbar-height + --ruler-height`, the
  width of the track-header column), so the mark has room under the traffic
  lights instead of being squeezed into one row. Traffic lights sit inside the
  block's top 22 px; the mark is 30 px and the wordmark is 17 px. The block is
  absolutely positioned in `.daw-root` (z above the toolbar and headers) and the
  toolbar starts after it. The `px/beat` readout floats at the bottom centre of
  the timeline as a pill in the same language as the `Grid` pill at the bottom
  right.
- Window title: `OpenCompose - <project name>` (`appTitle()` in
  `src/stores/branding.ts`) is drawn centred in the status bar.

## Wordmark type

- Face: **Quicksand** (SIL OFL 1.1), bundled as
  `src/assets/fonts/quicksand-latin.woff2` with its `OFL.txt` next to it and
  declared through `@font-face` / `--font-brand`. The OFL text ships in the
  app's third-party notices. It is used **only** for the
  wordmark; the rest of the interface keeps the platform UI grotesk. Bundling
  means the name survives offline and cannot change under us.
- Weight 500 at 17 px with 0.012em tracking, muted to `--text-secondary` so the
  mark and the wordmark read as one lockup.

## App launcher

The launcher uses the same mark in pale green on a dark evergreen plate.
`src-tauri/icons/` contains the macOS ICNS, multi-size Windows ICO, Linux PNGs,
and Windows tiles. The web assets are the favicon and Apple touch icon in
`public/`.

Run `python3 docs/brand/make-app-icons.py` from the repository root after
installing the frontend dependencies. The script uses the bundled Tauri CLI
to rasterize the vector and Pillow and macOS `iconutil` to generate the icon
set. macOS and Linux use the padded plate. Windows and web use a larger plate
to keep the mark legible at small sizes.
