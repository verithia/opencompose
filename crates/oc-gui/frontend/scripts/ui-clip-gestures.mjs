// Run against a Vite server with Playwright's Chromium and WebKit installed.
// The fixture records native commands. Device pixel ratio and the persisted
// interface setting vary here; Tauri's native webview zoom is outside the mock.
import { chromium, expect, webkit } from "@playwright/test";
import { baseUrl, createSmokeSnapshot, installTauriMock } from "./ui-fixture.mjs";

const snapshot = createSmokeSnapshot();
snapshot.revision = 7;
snapshot.project.properties.tempo = 120;
snapshot.project.tracks = [{
  ...snapshot.project.tracks[0],
  name: "Tape",
  instrument: { category: "audio", name: "clip" },
  preset: null,
  clips: [],
  phrases: [],
}];
snapshot.track_count = 1;
snapshot.project.properties.audio_clips = [{
  name: "Gesture fixture",
  file: "media/gesture.wav",
  track: snapshot.project.tracks[0].name,
  time_range: {
    start: { bar: 1, beat: 1, subdivision: 0 },
    end: { bar: 3, beat: 1, subdivision: 0 },
  },
  offset_seconds: 0,
  gain_db: 0,
  fade_in_seconds: 0,
  fade_out_seconds: 0,
  stretch: 1,
  stretch_mode: "musical",
  warp_markers: [{ source_seconds: 0.5, target_seconds: 0.512 }],
}];

async function editCalls(page) {
  return page.evaluate(() => window.__OCP_INVOKES__
    .filter(({ cmd }) => ["set_audio_clip_playback", "trim_audio_clip", "set_audio_clip_envelope"].includes(cmd)));
}

async function drag(page, control, offsetX, dx, editCount) {
  const rect = await control.boundingBox();
  expect(rect).not.toBeNull();
  const x = rect.x + offsetX(rect.width);
  const y = rect.y + rect.height / 2;
  expect(await control.evaluate((element, { x, y }) => element.contains(document.elementFromPoint(x, y)), { x, y }))
    .toBe(true);
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y, { steps: 6 });
  expect(await editCalls(page)).toHaveLength(editCount);
  await page.mouse.up();
  await expect.poll(() => editCalls(page)).toHaveLength(editCount + 1);
  return (await editCalls(page)).at(-1);
}

for (const engine of [webkit, chromium]) {
  const browser = await engine.launch({ headless: true });
  try {
    for (const [scale, deviceScaleFactor] of [[0.7, 1], [1, 2], [1.5, 2]]) {
      const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, deviceScaleFactor });
      page.setDefaultTimeout(7_000);
      try {
        await page.addInitScript((scale) => {
          localStorage.setItem("opencompose.uiScale", String(scale));
          localStorage.setItem("opencompose.locale", "en");
          localStorage.setItem("opencompose.onboarding.v1", "{}");
        }, scale);
        await installTauriMock(page, snapshot);
        await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
        await expect.poll(() => page.evaluate(() => document.documentElement.style.getPropertyValue("--ui-scale")))
          .toBe(String(scale));

        const clip = page.locator(".audio-clip-block");
        await expect(clip).toBeVisible();
        const before = await clip.boundingBox();
        await clip.click({ position: { x: 60, y: 20 } });
        const handle = clip.locator(".audio-clip-stretch-handle");
        await expect(handle).toBeVisible();

        // The handle is inset from the clip boundary. Drag two beats from
        // the actual grab point and expect the original end plus two beats.
        const stretch = await drag(page, handle, (width) => width / 2, before.width / 4, 0);
        expect(stretch).toMatchObject({
          cmd: "set_audio_clip_playback",
          args: { clipIndex: 0, expectedRevision: 7, playback: { stretch_to: { bar: 3, beat: 3, subdivision: 0 } } },
        });

        // The mock records edits without rewriting this fixture. Grab the
        // marker off-centre and move it one beat on the same timeline.
        const marker = clip.locator(".audio-clip-warp-marker");
        await expect(marker).toBeVisible();
        const markerBox = await marker.boundingBox();
        const markerX = markerBox.x + markerBox.width / 2;
        const markerY = markerBox.y + markerBox.height / 2;
        const initialLeft = await marker.evaluate((element) => element.style.left);
        await page.mouse.move(markerX, markerY);
        await page.mouse.down();
        await page.mouse.move(markerX, markerY + 12);
        await page.mouse.up();
        expect(await marker.evaluate((element) => element.style.left)).toBe(initialLeft);
        expect(await editCalls(page)).toHaveLength(1);

        await page.mouse.move(markerX, markerY);
        await page.mouse.down();
        await page.mouse.move(markerX + before.width / 8, markerY);
        await expect.poll(() => marker.evaluate((element) => element.style.left)).not.toBe(initialLeft);
        await page.keyboard.press("Escape");
        await page.mouse.up();
        expect(await marker.evaluate((element) => element.style.left)).toBe(initialLeft);
        expect(await editCalls(page)).toHaveLength(1);

        const warp = await drag(page, marker, (width) => width * 0.75, before.width / 8, 1);
        expect(warp).toMatchObject({
          cmd: "set_audio_clip_playback",
          args: { clipIndex: 0, expectedRevision: 7, playback: { warp_markers: [{ source_seconds: 0.5, target_seconds: 1 }] } },
        });

        console.log(`${engine.name()} passed at interface setting ${scale}, DPR ${deviceScaleFactor}`);
      } finally {
        await page.close();
      }
    }
  } finally {
    await browser.close();
  }
}
