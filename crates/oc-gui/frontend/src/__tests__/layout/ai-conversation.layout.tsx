import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { render } from "@solidjs/testing-library";
import { AiMarkdown } from "../../components/AiMarkdown";
import { AiToolContent } from "../../components/AiToolContent";
import { THEME_CATALOG } from "../../stores/theme";
import { i18n } from "../../stores/i18n";

// Stress fixtures mirror the panel's DOM; real CSS and browser geometry are used.
const root = path.resolve(process.cwd(), "src/styles");
const manifest = readFileSync(path.join(root, "daw.css"), "utf8");
const css = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
  .map((match) => readFileSync(path.resolve(root, match[1]), "utf8")).join("\n") + "\n" + readFileSync(path.join(root, "catppuccin.css"), "utf8");
const payload = JSON.stringify({ file: "nested-project-directory/".repeat(40), result: "complete" });
let browser: Browser;
beforeAll(async () => { browser = await chromium.launch({ headless: true }); });
afterAll(async () => { await browser?.close(); });

describe("AI conversation geometry", () => {
  it("keeps step groups at their content height when the transcript overflows", async () => {
    const group = `<details class="ai-step-group"><summary><span class="ai-step-label">Completed in 7s · 4 tool calls</span></summary><div class="ai-message tool"><div class="ai-message-content">tracks_list</div></div></details>`;
    const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
    try {
      await page.setContent(`<style>${css}</style><div class="ai-panel" style="width:320px;height:300px"><div class="ai-messages">${group.repeat(12)}</div></div>`);
      const geometry = await page.evaluate(() => {
        const transcript = document.querySelector(".ai-messages") as HTMLElement;
        return {
          heights: [...document.querySelectorAll(".ai-step-group")].map((element) => element.getBoundingClientRect().height),
          scrollable: transcript.scrollHeight > transcript.clientHeight + 1,
        };
      });
      // A flex child that shrinks clips its own summary; every group must keep
      // its natural height and let the transcript scroll instead.
      expect(Math.min(...geometry.heights)).toBeGreaterThanOrEqual(18);
      expect(geometry.scrollable).toBe(true);
    } finally { await page.close(); }
  });

  it("paints a dropdown list above a Preferences overlay", async () => {
    const page = await browser.newPage({ viewport: { width: 900, height: 700 } });
    try {
      await page.setContent(`<style>${css}</style>
        <div class="preferences-overlay" style="position:fixed;inset:0;z-index:10000;background:rgba(0,0,0,.5)">
          <div class="oc-dropdown"><button class="oc-dropdown-trigger">Gateway</button></div>
        </div>
        <div class="oc-dropdown-menu" style="position:fixed;left:40px;top:40px;width:200px">
          <button class="oc-dropdown-option"><span class="oc-dropdown-option-label">Gateway-Google</span></button>
        </div>`);
      const hit = await page.evaluate(() => {
        const option = document.querySelector(".oc-dropdown-option") as HTMLElement;
        const box = option.getBoundingClientRect();
        return !!document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2)
          ?.closest(".oc-dropdown-menu");
      });
      // The list is portalled to the body; it must own a layer above the modal.
      expect(hit).toBe(true);
    } finally { await page.close(); }
  });

  it("keeps the custom audio controls inside a narrow result", async () => {
    const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
    try {
      await page.setContent(`<style>${css}</style><div class="ai-panel" style="width:240px;height:300px"><div class="ai-tool-audio"><canvas class="ai-tool-waveform"></canvas><div class="ai-tool-audio-controls"><button class="ai-tool-audio-toggle" aria-label="Play">▶</button><input class="ai-tool-audio-seek" type="range" min="0" max="4" step="0.01" value="0"><span class="ai-tool-audio-clock">0:00 / 0:04</span></div><audio class="ai-tool-audio-element"></audio></div></div>`);
      const geometry = await page.evaluate(() => {
        const card = document.querySelector(".ai-tool-audio") as HTMLElement;
        return {
          overflow: card.scrollWidth - card.clientWidth,
          toggle: document.querySelector(".ai-tool-audio-toggle")!.getBoundingClientRect().width,
          seek: document.querySelector(".ai-tool-audio-seek")!.getBoundingClientRect().width,
          nativeControls: getComputedStyle(document.querySelector("audio")!).display,
        };
      });
      // The player is ours: the browser's native control strip stays hidden and
      // the seek bar keeps enough room beside the button and the clock.
      expect(geometry.overflow).toBeLessThanOrEqual(1);
      expect(geometry.toggle).toBeGreaterThanOrEqual(20);
      expect(geometry.seek).toBeGreaterThan(60);
      expect(geometry.nativeControls).toBe("none");
    } finally { await page.close(); }
  });

  it("contains assistant Markdown tables and long fenced code in a narrow panel", async () => {
    const rendered = render(() => <AiMarkdown content={'## Plan\n\n**Piano**\n\n```json\n{"file":"' + "long-path/".repeat(100) + '"}\n```\n\n| Track | Detail | Value | More |\n|---|---|---|---|\n| Piano | ' + "wide".repeat(70) + ' | 76 | Done |'} />);
    const markup = rendered.container.innerHTML;
    rendered.unmount();
    const page = await browser.newPage();
    try {
      await page.setContent(`<style>${css}</style><div class="ai-panel" style="width:240px;height:600px"><div class="ai-messages"><div class="ai-message assistant"><div class="ai-message-content">${markup}</div></div></div></div>`);
      const sizes = await page.evaluate(() => {
        const panel = document.querySelector(".ai-panel") as HTMLElement;
        const pre = document.querySelector("pre") as HTMLElement;
        return { overflow: panel.scrollWidth - panel.clientWidth, whiteSpace: getComputedStyle(pre).whiteSpace, height: pre.clientHeight };
      });
      expect(sizes.overflow).toBeLessThanOrEqual(1);
      expect(sizes.whiteSpace).toBe("pre-wrap");
      expect(sizes.height).toBeLessThanOrEqual(240);
    } finally { await page.close(); }
  });

  /** The panel's DOM in the order the component renders it: session chrome,
      transcript, composer, then the composer's model footer. */
  const panelMarkup = (width: number, height: number) =>
    `<style>${css}</style><div class="ai-panel" style="width:${width}px;height:${height}px">
          <div class="ai-panel-header"><span class="ai-panel-title">AI</span><div class="ai-header-identity"><button class="ai-header-scope"><span class="ai-header-scope-name">Piano workspace</span></button><span class="ai-header-title">New piano solo conversation</span></div><button>Settings</button></div>
          <div class="ai-messages"><div class="ai-message user"><div class="ai-message-content">${payload}</div></div><div class="ai-message assistant"><div class="ai-message-content">${payload}</div></div><div class="ai-message tool"><details class="ai-tool-result"><summary><span class="ai-tool-name">content_libraries</span><span class="ai-tool-result-text">${payload}</span></summary><pre>${payload}</pre></details></div><div class="ai-thinking reasoning"><details class="ai-thinking-details"><summary class="ai-thinking-summary"><span class="ai-thinking-label">Thinking<span class="ai-thinking-preview">${payload}</span></span><span class="ai-thinking-elapsed"></span></summary><pre class="ai-thinking-content">${payload}</pre></details></div><div class="ai-message ai-pending"><div class="ai-message-content"><div class="ai-thinking"><span class="ai-thinking-mark"><span class="ai-thinking-dot"></span><span class="ai-thinking-dot"></span><span class="ai-thinking-dot"></span></span><span class="ai-thinking-label">Waiting for gateway/google/gemini-4-argon-with-a-long-provider-name</span><span class="ai-thinking-elapsed">182s</span></div></div></div></div>
          <div class="ai-input-bar"><button class="ai-attach-btn">+</button><textarea class="ai-input" rows="1">Compose a piano solo</textarea><button class="ai-send-btn">→</button></div>
          <div class="ai-model-bar"><div class="oc-dropdown ai-model-select ai-model-picker"><button class="oc-dropdown-trigger"><span class="oc-dropdown-value">gateway/google/gemini-4-argon-with-a-long-provider-name</span><span class="oc-dropdown-chevron">▾</span></button></div><div class="oc-dropdown ai-model-select ai-model-effort"><button class="oc-dropdown-trigger"><span class="oc-dropdown-value">medium</span><span class="oc-dropdown-chevron">▾</span></button></div></div></div>`;

  for (const width of [240, 400, 640]) {
    it(`contains long transcript/tool payloads and composer at ${width}px`, async () => {
      const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
      try {
        await page.setContent(panelMarkup(width, 700));
        const measure = () => page.evaluate(() => {
          const panel = document.querySelector(".ai-panel") as HTMLElement;
          const transcript = document.querySelector(".ai-messages") as HTMLElement;
          const summary = document.querySelector("summary") as HTMLElement;
          const composer = document.querySelector(".ai-input-bar") as HTMLElement;
          const input = document.querySelector("textarea")!.getBoundingClientRect();
          const send = document.querySelector(".ai-send-btn")!.getBoundingClientRect();
          const pending = document.querySelector(".ai-pending") as HTMLElement;
          const user = document.querySelector(".ai-message.user") as HTMLElement;
          const modelBar = document.querySelector(".ai-model-bar") as HTMLElement;
          const panelBox = panel.getBoundingClientRect();
          const composerBox = composer.getBoundingClientRect();
          const modelBarBox = modelBar.getBoundingClientRect();
          const transcriptBox = transcript.getBoundingClientRect();
          const transcriptStyle = getComputedStyle(transcript);
          const contentLeft = transcriptBox.left + parseFloat(transcriptStyle.borderLeftWidth) + parseFloat(transcriptStyle.paddingLeft);
          const contentWidth = transcript.clientWidth - parseFloat(transcriptStyle.paddingLeft) - parseFloat(transcriptStyle.paddingRight);
          const userBox = user.getBoundingClientRect();
          return { overflow: transcript.scrollWidth - transcript.clientWidth,
            panelOverflow: panel.scrollWidth - panel.clientWidth,
            summaryHeight: summary.getBoundingClientRect().height,
            composerOverflow: composer.scrollWidth - composer.clientWidth,
            inputWidth: input.width, overlap: input.right - send.left,
            pendingOverflow: pending.scrollWidth - pending.clientWidth,
            // The composer and its model footer are the panel's last two rows and
            // stay inside it, with a transcript above them that keeps its height.
            panelHeight: panelBox.height,
            composerBottom: composerBox.bottom,
            composerInside: composerBox.bottom <= panelBox.bottom + 1,
            modelBarOverflow: modelBar.scrollWidth - modelBar.clientWidth,
            modelBarInside: modelBarBox.bottom <= panelBox.bottom + 1,
            modelBarAfterComposer: modelBarBox.top >= composerBox.top,
            transcriptHeight: transcriptBox.height,
            // The user's block is a chat row: it ends at the transcript's right
            // edge, stops at 85 % of the width, and never loses its left margin.
            userLeftGap: userBox.left - contentLeft,
            userRightGap: contentLeft + contentWidth - userBox.right,
            userWidth: userBox.width,
            contentWidth,
            userTextAlign: getComputedStyle(user.querySelector(".ai-message-content")!).textAlign,
            userRoleCells: user.querySelectorAll(".ai-message-role").length,
            pendingRoleCells: pending.querySelectorAll(".ai-message-role").length };
        });
        const closed = await measure();
        expect(closed.overflow).toBeLessThanOrEqual(1);
        expect(closed.panelOverflow).toBeLessThanOrEqual(1);
        expect(closed.composerOverflow).toBeLessThanOrEqual(1);
        expect(closed.summaryHeight).toBeLessThanOrEqual(20);
        expect(closed.inputWidth).toBeGreaterThan(80);
        expect(closed.overlap).toBeLessThanOrEqual(0);
        // The composer and the model row under it are inside the panel, and the
        // transcript keeps most of the height.
        expect(closed.composerInside, "composer clipped").toBe(true);
        expect(closed.modelBarInside, "model row clipped").toBe(true);
        expect(closed.modelBarAfterComposer, "model row above the composer").toBe(true);
        expect(closed.modelBarOverflow, "model row overflows").toBeLessThanOrEqual(1);
        expect(closed.transcriptHeight).toBeGreaterThan(closed.panelHeight * 0.4);
        // The long provider and model wrap inside the pending row instead of
        // widening it past the transcript.
        expect(closed.pendingOverflow).toBeLessThanOrEqual(1);
        // The user's message sits on the right with a visible left margin, and
        // its text stays left-aligned inside that block.
        expect(closed.userLeftGap).toBeGreaterThanOrEqual(24);
        expect(closed.userWidth).toBeLessThanOrEqual(closed.contentWidth * 0.86);
        expect(closed.userWidth).toBeGreaterThanOrEqual(closed.contentWidth * 0.8);
        expect(Math.abs(closed.userRightGap)).toBeLessThanOrEqual(1);
        expect(closed.userTextAlign).toBe("left");
        // Neither the user's row nor the waiting row prints a role label.
        expect(closed.userRoleCells).toBe(0);
        expect(closed.pendingRoleCells).toBe(0);
        await page.locator(".ai-tool-result > summary").click();
        const opened = await measure();
        expect(opened.overflow).toBeLessThanOrEqual(1);
        expect(opened.summaryHeight).toBeGreaterThan(closed.summaryHeight);
        expect(await page.locator(".ai-tool-result pre").textContent()).toBe(payload);
        await page.locator(".ai-thinking-summary").click();
        expect((await measure()).panelOverflow).toBeLessThanOrEqual(1);
        expect(await page.locator(".ai-thinking-content").textContent()).toBe(payload);
      } finally { await page.close(); }
    });
  }

  it("keeps the composer and its model row on screen at 240 px", async () => {
    const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
    try {
      await page.setContent(panelMarkup(240, 420));
      const geometry = await page.evaluate(() => {
        const panel = document.querySelector(".ai-panel") as HTMLElement;
        const transcript = document.querySelector(".ai-messages") as HTMLElement;
        const composer = document.querySelector(".ai-input-bar") as HTMLElement;
        const modelBar = document.querySelector(".ai-model-bar") as HTMLElement;
        const input = document.querySelector(".ai-input") as HTMLElement;
        const send = document.querySelector(".ai-send-btn") as HTMLElement;
        const picker = document.querySelector(".ai-model-picker") as HTMLElement;
        const effort = document.querySelector(".ai-model-effort") as HTMLElement;
        const panelBox = panel.getBoundingClientRect();
        const composerBox = composer.getBoundingClientRect();
        const modelBox = modelBar.getBoundingClientRect();
        const inputBox = input.getBoundingClientRect();
        const sendBox = send.getBoundingClientRect();
        return {
          panelOverflow: panel.scrollWidth - panel.clientWidth,
          composerInside: composerBox.bottom <= panelBox.bottom + 1,
          composerTopInside: composerBox.top >= panelBox.top,
          modelInside: modelBox.bottom <= panelBox.bottom + 1,
          modelOverflow: modelBar.scrollWidth - modelBar.clientWidth,
          inputWidth: inputBox.width,
          inputBeforeSend: inputBox.right <= sendBox.left + 1,
          transcriptHeight: transcript.getBoundingClientRect().height,
          pickerWidth: picker.getBoundingClientRect().width,
          effortWidth: effort.getBoundingClientRect().width,
        };
      });
      expect(geometry.panelOverflow).toBeLessThanOrEqual(1);
      expect(geometry.composerInside, "composer below the panel edge").toBe(true);
      expect(geometry.composerTopInside).toBe(true);
      expect(geometry.modelInside, "model row below the panel edge").toBe(true);
      expect(geometry.modelOverflow).toBeLessThanOrEqual(1);
      expect(geometry.inputWidth).toBeGreaterThan(80);
      expect(geometry.inputBeforeSend).toBe(true);
      // The transcript keeps the rows the footer does not need.
      expect(geometry.transcriptHeight).toBeGreaterThan(120);
      // Both selectors keep enough width to read and click.
      expect(geometry.pickerWidth).toBeGreaterThan(60);
      expect(geometry.effortWidth).toBeGreaterThan(40);
    } finally { await page.close(); }
  });
  for (const family of THEME_CATALOG) for (const theme of family.variants) {
    it(`keeps code contrast and localized controls readable in ${theme.css}`, async () => {
      const page = await browser.newPage({ viewport: { width: 1000, height: 800 } });
      try {
        for (const locale of ["en", "zh-Hans", "zh-Hant", "ja", "es"] as const) {
          i18n.setLocale(locale);
          const rendered = render(() => <AiToolContent content={JSON.stringify({
            enabled: true, tempo: 76, file: "long-file-path/".repeat(900),
            dsl_source: '@project "Piano solo"\n tempo: 76\n @track piano\n C4 q // melody',
          })} />);
          const code = rendered.container.innerHTML;
          rendered.unmount();
          const waiting = i18n.t("ai.thinking.waiting", { model: "MiMo Pro / long-provider-identity" });
          const cancel = i18n.t("ai.turn.stop");
          await page.setContent(`<html data-theme="${theme.css}"><style>${css}</style><div class="ai-panel" style="width:324px;height:700px">
            <div class="ai-messages"><div class="ai-message user"><div class="ai-message-content">${i18n.t("ai.sessions.summary")}</div></div><div class="ai-message tool"><details class="ai-tool-result" open><summary>project_get</summary>${code}</details></div><div class="ai-message ai-pending"><div class="ai-message-content"><div class="ai-thinking"><span class="ai-thinking-mark"><span class="ai-thinking-dot"></span><span class="ai-thinking-dot"></span><span class="ai-thinking-dot"></span></span><span class="ai-thinking-label">${waiting}</span><span class="ai-thinking-elapsed">182s</span></div></div></div></div>
            <div class="ai-input-bar"><button class="ai-attach-btn">+</button><textarea class="ai-input" rows="1">${waiting}</textarea><button class="ai-send-btn">→</button></div>
            <div class="ai-stats"><span>${i18n.t("ai.sessions.summary")}</span><div class="ai-stats-anchor"><button>i</button><div class="ai-stats-card"><div class="ai-stats-rows"><span class="ai-stats-label">${i18n.t("ai.sessions.summary")} ${i18n.t("ai.sessions.summary")}</span><span class="ai-stats-value">200000 / 32000</span></div></div></div></div>
            <div class="ai-accept-bar"><button class="ai-accept-btn">${cancel}</button></div><div class="ai-diff-line added">+ tempo: 76</div></div></html>`);
          const metrics = await page.evaluate(() => {
            const panel = document.querySelector(".ai-panel") as HTMLElement;
            const pending = document.querySelector(".ai-pending") as HTMLElement;
            const luminance = (color: string) => {
              const values = color.match(/[\d.]+/g)!.slice(0, 3).map(Number).map(v => v / 255);
              const linear = values.map(v => v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
              return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
            };
            const bg = luminance(getComputedStyle(document.querySelector(".ai-tool-code")!).backgroundColor);
            const contrasts = [...document.querySelectorAll('.ai-tool-code [class^="syn-"]')].map(span => {
              const fg = luminance(getComputedStyle(span).color);
              return (Math.max(fg, bg) + 0.05) / (Math.min(fg, bg) + 0.05);
            });
            const panelBox = panel.getBoundingClientRect();
            const card = document.querySelector(".ai-stats-card") as HTMLElement;
            const cardBox = card.getBoundingClientRect();
            const controlContrasts = [...document.querySelectorAll(".ai-accept-btn, .ai-send-btn, .ai-diff-line")].map(e => {
              const style = getComputedStyle(e);
              const a = luminance(style.color), b = luminance(style.backgroundColor);
              return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
            });
            return {
              cardWithinPanel: cardBox.left >= panelBox.left && cardBox.right <= panelBox.right,
              cardOverflow: card.scrollWidth - card.clientWidth,
              minimumControlContrast: Math.min(...controlContrasts),
              panelOverflow: panel.scrollWidth - panel.clientWidth,
              pendingOverflow: pending.scrollWidth - pending.clientWidth,
              pagerOverflow: [...document.querySelectorAll(".ai-code-pages")].map(e => e.scrollWidth - e.clientWidth),
              textSize: parseFloat(getComputedStyle(document.querySelector(".ai-message-content")!).fontSize),
              codeSize: parseFloat(getComputedStyle(document.querySelector(".ai-tool-code pre")!).fontSize),
              minimumContrast: Math.min(...contrasts),
              radius: getComputedStyle(document.querySelector(".ai-message")!).borderTopLeftRadius,
            };
          });
          expect(metrics.cardWithinPanel, `${theme.css}/${locale} stats`).toBe(true);
          expect(metrics.cardOverflow).toBeLessThanOrEqual(1);
          expect(metrics.minimumControlContrast).toBeGreaterThanOrEqual(4.5);
          expect(metrics.panelOverflow, `${theme.css}/${locale}`).toBeLessThanOrEqual(1);
          expect(metrics.pendingOverflow, `${theme.css}/${locale} pending`).toBeLessThanOrEqual(1);
          expect(metrics.pagerOverflow.every(v => v <= 1)).toBe(true);
          expect(metrics.textSize).toBeGreaterThanOrEqual(12);
          expect(metrics.codeSize).toBeGreaterThanOrEqual(11);
          expect(metrics.minimumContrast, `${theme.css}/${locale} syntax`).toBeGreaterThanOrEqual(4.5);
          expect(metrics.radius).toBe("0px");
          const artifactDir = process.env.OCP_LAYOUT_ARTIFACT_DIR;
          if (artifactDir && ((theme.css === "ivory" && locale === "en") || (theme.css === "graphite" && locale === "zh-Hans"))) {
            mkdirSync(artifactDir, { recursive: true });
            await page.locator(".ai-panel").screenshot({ path: path.join(artifactDir, `ai-panel-fixture-${theme.css}-${locale}.png`) });
          }
        }
      } finally { i18n.setLocale("en"); await page.close(); }
    });
  }

});
