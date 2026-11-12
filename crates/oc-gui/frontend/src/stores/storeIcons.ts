/**
 * Site icons for the Store page, fetched and cached by the backend.
 *
 * The backend returns the site's own favicon as a `data:` URL, or null when
 * the site is refused or offers nothing usable. The page never loads a remote
 * image, so opening the Store contacts no site from the webview.
 */
import { invoke } from "@tauri-apps/api/core";

/** The only replies the page displays: base64 raster images the backend sniffed. */
const DATA_IMAGE = /^data:image\/(png|x-icon|webp);base64,[A-Za-z0-9+/]+={0,2}$/;

const requests = new Map<string, Promise<string | null>>();

/** One backend request per site page for the life of the window. */
export function siteIcon(url: string): Promise<string | null> {
  let request = requests.get(url);
  if (!request) {
    request = invoke<unknown>("store_site_icon", { url })
      .then((value) => (typeof value === "string" && DATA_IMAGE.test(value) ? value : null))
      .catch(() => null);
    requests.set(url, request);
  }
  return request;
}

/** Forgets every answer, so the next card asks the backend again. */
export function clearSiteIcons(): void {
  requests.clear();
}
