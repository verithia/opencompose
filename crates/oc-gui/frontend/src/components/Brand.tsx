import { LOGO_MARK } from "../stores/branding";

/**
 * The window mark: one block in the top-left corner that spans the toolbar
 * height plus the timeline ruler, so the logo has room under the traffic
 * lights instead of being squeezed into a single row.
 */
export function Brand() {
  return (
    <div class="brand-block" data-tauri-drag-region="deep">
      <span class="brand-lights" aria-hidden="true" />
      <span class="brand-lockup">
        <span class="brand-name">OpenCompose</span>
        <span
          class="app-logo"
          role="img"
          aria-label="OpenCompose"
          style={{
            "mask-image": `url(${LOGO_MARK})`,
            "-webkit-mask-image": `url(${LOGO_MARK})`,
          }}
        />
      </span>
    </div>
  );
}
