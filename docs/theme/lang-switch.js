// Adds an English / 简体中文 link to the menu bar of both books.
//
// The English book is published at the site root and the Simplified Chinese
// book under zh/. A page links to its counterpart when one exists and to the
// other book's front page otherwise. TRANSLATED is the list of pages the
// Chinese book carries; scripts/check-docs.sh keeps it equal to
// docs-zh/src/SUMMARY.md.
(function () {
    "use strict";

    const TRANSLATED = [
        "index.html",
        "manual/index.html",
        "manual/getting-started.html",
        "first-run.html",
        "manual/troubleshooting.html",
        "manual/dsl.html",
        "ornaments.html",
        "manual/cli.html",
        "manual/gui.html",
        "storage-layout.html",
        "manual/markers.html",
        "manual/devices.html",
        "manual/synth.html",
        "factory-presets.html",
        "manual/plugins.html",
        "agent-surface.html",
        "manual/agents.html",
        "ai-and-plugins.html",
        "sampler.html",
        "drum-kit.html",
        "arrangement.html",
        "autosave.html",
        "export-format.html",
        "timing.html",
        "automation.html",
        "mixer-routing.html",
        "midi-import-export.html",
        "midi-live.html",
        "audio-devices.html",
        "preferences.html",
        "samples.html",
    ];

    // mdBook defines path_to_root on every page; it leads to the book's root.
    const bookRoot = new URL(typeof path_to_root === "string" ? path_to_root : "", window.location.href);
    const inChinese = document.documentElement.lang === "zh-CN";
    let page = window.location.href.slice(bookRoot.href.length).split("#")[0].split("?")[0];
    if (page === "" || page.endsWith("/")) page += "index.html";

    const otherRoot = new URL(inChinese ? "../" : "zh/", bookRoot);
    const target = new URL(TRANSLATED.includes(page) ? page : "index.html", otherRoot);

    const link = document.createElement("a");
    link.className = "oc-lang-switch";
    link.href = target.href;
    link.hreflang = inChinese ? "en" : "zh-CN";
    link.lang = inChinese ? "en" : "zh-CN";
    link.textContent = inChinese ? "English" : "简体中文";
    link.title = inChinese ? "Read this page in English" : "阅读简体中文版";

    // scripts/docs.sh fails the build when the pinned mdBook stops emitting
    // this menu bar, so a template change cannot hide the switch silently.
    const buttons = document.querySelector("#mdbook-menu-bar .right-buttons");
    if (buttons) buttons.prepend(link);
})();
