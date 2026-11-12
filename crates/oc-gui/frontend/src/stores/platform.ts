/** Platform facts the chrome layout needs; the window frame differs per OS. */
export function isMac(): boolean {
  return /mac/i.test(navigator.userAgent);
}

export function isWindows(): boolean {
  return /windows/i.test(navigator.userAgent);
}

/**
 * Tag the document so CSS can reserve the traffic-light gutter on macOS and
 * leave the Windows frame alone. Called once at boot.
 */
export function applyPlatform(): void {
  if (typeof document === "undefined") return;
  const platform = isMac() ? "macos" : isWindows() ? "windows" : "linux";
  document.documentElement.dataset.platform = platform;
}
