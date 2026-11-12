/**
 * Help & About page: the build version and the supported DSL version come from
 * the backend, the repository link is published, and the licence line is
 * present. No marketing copy.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { invokeMock } from "./setup";
import { HelpPage } from "../components/preferences/HelpPage";
import { i18n } from "../stores/i18n";
import { onboardingStore } from "../stores/onboarding";
import { preferencesUi } from "../stores/preferencesUi";
import { commandById } from "../stores/commands";
import { diagnosticsStore } from "../stores/diagnosticsUi";

beforeEach(() => {
  i18n.setLocale("en");
  diagnosticsStore.reset();
  preferencesUi.close();
  preferencesUi.show("display");
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "app_info") return { version: "9.9.9", dsl_version: "0.2", source_revision:"abc123", build_profile:"release", os:"linux", arch:"x86_64" };
    return undefined;
  });
});

afterEach(() => cleanup());

describe("the Help & About page", () => {
  it("reports the version and the DSL version the backend answers", async () => {
    const { container } = render(() => <HelpPage />);

    await vi.waitFor(() =>
      expect(container.querySelector(".preferences-app-version")?.textContent).toBe("9.9.9"),
    );
    expect(container.querySelector(".preferences-dsl-version")?.textContent).toBe("0.2");
    expect(
      container.querySelector(".preferences-fact-label")?.textContent,
    ).toBe(i18n.t("preferences.help.version"));
  });

  it("opens the public repository and states the licence", async () => {
    const { container } = render(() => <HelpPage />);

    expect(container.querySelector(".preferences-repository")).toHaveTextContent("github.com/verithia/opencompose");
    const link = container.querySelector(".preferences-repository-link");
    expect(link).toBeTruthy();
    fireEvent.click(link!);
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("open_url", { url: "https://github.com/verithia/opencompose" }),
    );
    expect(screen.queryByRole("button", { name: "Report an issue" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Release notes" })).toBeNull();
    expect(container.querySelector(".preferences-licence")?.textContent).toBe("AGPL-3.0-or-later");
    expect(container.querySelector(".diagnostics-btn")).toBeTruthy();
  });

  it("reopens setup without clearing the current project", async () => {
    onboardingStore.complete();
    preferencesUi.openOn("help");
    render(() => <HelpPage />);
    fireEvent.click(screen.getByRole("button", { name: "Getting started" }));
    await vi.waitFor(() => expect(onboardingStore.pending()).toBe(true));
    expect(preferencesUi.open()).toBe(false);
    expect(invokeMock.mock.calls.map(([command]) => command)).not.toContain("new_project");
  });

  it("opens the bundled manual without network access", async () => {
    await commandById("help.manual")!.run();
    expect(preferencesUi.page()).toBe("help");
    expect(preferencesUi.open()).toBe(true);
    render(() => <HelpPage />);
    expect(screen.getByRole("heading", { name: "The window" })).toBeVisible();
    expect(invokeMock.mock.calls.map(([command]) => command)).not.toContain("open_url");
  });

  it("hands sound and shortcut setup to the existing settings pages", () => {
    render(() => <HelpPage />);
    fireEvent.click(screen.getByRole("button", { name: i18n.t("preferences.page.audio") }));
    expect(preferencesUi.page()).toBe("audio");
    fireEvent.click(screen.getByRole("button", { name: i18n.t("preferences.page.keymap") }));
    expect(preferencesUi.page()).toBe("keymap");
  });

  it.each(["en", "zh-Hans", "zh-Hant", "ja", "es"] as const)("localizes help controls in %s", (locale) => {
    i18n.setLocale(locale);
    render(() => <HelpPage />);
    expect(screen.getByRole("button", { name: i18n.t("preferences.help.gettingStarted") })).toBeVisible();
    expect(screen.getByText(i18n.t("preferences.help.offlineManual"))).toBeVisible();
    expect(screen.getByText("github.com/verithia/opencompose")).toBeVisible();
  });

  it("explains what diagnostics contain before exporting", () => {
    render(() => <HelpPage />);
    const button = screen.getByRole("button", { name: i18n.t("toolbar.diagnostics") });
    const description = document.getElementById(button.getAttribute("aria-describedby") ?? "");
    expect(description?.textContent).toContain("current project source");
    expect(description?.textContent).toContain("saved locally");
    expect(invokeMock.mock.calls.map(([command]) => command)).not.toContain("export_diagnostics");
  });

  it("shows build identity and readable offline license texts", async () => {
    const { container } = render(() => <HelpPage />);
    await vi.waitFor(() => expect(container.querySelector(".preferences-build-identity")).toHaveTextContent("abc123 · release · linux/x86_64"));
    expect(container.querySelector(".preferences-help-credits")?.textContent).toContain("GNU AFFERO GENERAL PUBLIC LICENSE");
    expect(container.querySelector(".preferences-help-credits")?.textContent).toContain("Permission is hereby granted");
    expect(container.querySelector(".preferences-help-credits")?.textContent).toContain("Apache License");
  });

  it("loads the third-party notices, including bundled assets, when opened", async () => {
    const { container } = render(() => <HelpPage />);
    const details = container.querySelector<HTMLDetailsElement>(".preferences-third-party")!;
    expect(details.textContent).toContain(i18n.t("preferences.help.thirdParty"));
    expect(details.textContent).not.toContain("symphonia");

    details.open = true;
    fireEvent(details, new Event("toggle"));

    await vi.waitFor(() => expect(details.textContent).toContain("crate symphonia-core"));
    expect(details.textContent).toContain("npm solid-js");
    expect(details.textContent).toContain("MS_General");
    expect(details.textContent).toContain("SIL OPEN FONT LICENSE");
  });

  it("shows a dash when the backend cannot answer", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "app_info") throw new Error("no bridge");
      return undefined;
    });
    const { container } = render(() => <HelpPage />);

    await vi.waitFor(() =>
      expect(container.querySelector(".preferences-app-version")?.textContent).toBe("—"),
    );
    expect(container.querySelector(".preferences-error")?.textContent).toContain("no bridge");
  });
});
