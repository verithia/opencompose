import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { i18n } from "../../stores/i18n";
import { Onboarding } from "../../components/Onboarding";
import { PreferencesHost } from "../../components/PreferencesModal";
import { preferencesUi } from "../../stores/preferencesUi";
import { AUDIO_IO_EMPTY, invokeMock } from "../setup";

afterEach(() => { cleanup(); preferencesUi.close(); });
it("focuses setup and keeps Tab within it", async () => {
  render(() => <Onboarding onDone={() => {}} />);
  const skip = screen.getByRole("button", { name: "Skip setup" });
  await waitFor(() => expect(skip).toHaveFocus());
  const next = screen.getByRole("button", { name: "Continue" });
  next.focus();
  fireEvent.keyDown(next, { key: "Tab" });
  expect(skip).toHaveFocus();
  fireEvent.keyDown(skip, { key: "Tab", shiftKey: true });
  expect(next).toHaveFocus();
});
it("dismisses setup with Escape through the modal stack", () => {
  const done = vi.fn();
  render(() => <Onboarding onDone={done} />);
  fireEvent.keyDown(document, { key: "Escape" });
  expect(done).toHaveBeenCalledOnce();
});
it("lets nested Preferences own Escape and returns focus to its setup opener", async () => {
  invokeMock.mockImplementation(async command => {
    if (command === "app_info") return { version: "0.1.0", dsl_version: "0.2" };
    if (command === "get_audio_io") return AUDIO_IO_EMPTY;
    return undefined;
  });
  const done = vi.fn();
  render(() => <><Onboarding onDone={done} /><PreferencesHost /></>);
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  // Help avoids credential/config initialization: this case tests modal ownership.
  const opener = screen.getByRole("button", { name: "Open AI settings" });
  opener.focus();
  preferencesUi.openOn("help", "connection", opener);
  const close = screen.getByRole("button", { name: i18n.t("preferences.close") });
  await waitFor(() => expect(close).toHaveFocus());
  fireEvent.keyDown(close, { key: "Escape" });
  await waitFor(() => expect(preferencesUi.open()).toBe(false));
  expect(done).not.toHaveBeenCalled();
  await waitFor(() => expect(opener).toHaveFocus());
});

it("mounts Preferences above the toolbar stacking context", async () => {
  invokeMock.mockImplementation(async command => command === "app_info" ? { version: "0.1.0", dsl_version: "0.2" } : undefined);
  const { container } = render(() => <div style={{ position: "relative", "z-index": 2 }}><PreferencesHost /></div>);
  preferencesUi.openOn("help");
  const preferences = await screen.findByRole("dialog", { name: "Preferences" });
  expect(container.contains(preferences)).toBe(false);
  expect(document.body.contains(preferences)).toBe(true);
});
