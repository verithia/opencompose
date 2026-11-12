/**
 * StoreBrowser — built-in browser for discovering plugins and samples.
 *
 * Opens external sites in a separate Tauri WebviewWindow so they load
 * with full browser functionality (login, download, etc.) without
 * iframe restrictions.
 */
import { For, Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import { siteIcon } from "../stores/storeIcons";
import { PluginStore } from "./PluginStore";
import { useTooltip } from "./Tooltip";
import { DiskIcon, FolderIcon, PlugIcon, WaveIcon } from "./icons";

interface StoreSite {
  id: string;
  name: string;
  url: string;
  category: "plugins" | "samples" | "soundfonts" | "presets";
  /** Dictionary key for the card blurb; the site name stays a brand name. */
  descriptionKey: I18nKey;
}

const STORE_SITES: StoreSite[] = [
  // Free Plugins
  {
    id: "kvraudio",
    name: "KVR Audio",
    url: "https://www.kvraudio.com/plugins/newest",
    category: "plugins",
    descriptionKey: "store.site.kvraudio",
  },
  {
    id: "plugins4free",
    name: "Plugins4Free",
    url: "https://plugins4free.com/",
    category: "plugins",
    descriptionKey: "store.site.plugins4free",
  },
  {
    id: "bedroom-producers",
    name: "BPB Free Plugins",
    url: "https://bedroomproducersblog.com/free-vst-plugins/",
    category: "plugins",
    descriptionKey: "store.site.bedroom-producers",
  },
  {
    id: "surge-synth",
    name: "Surge XT",
    url: "https://surge-synthesizer.github.io/",
    category: "plugins",
    descriptionKey: "store.site.surge-synth",
  },
  {
    id: "vital-synth",
    name: "Vital Synth",
    url: "https://vital.audio/",
    category: "plugins",
    descriptionKey: "store.site.vital-synth",
  },
  {
    id: "airwindows",
    name: "Airwindows",
    url: "https://www.airwindows.com/",
    category: "plugins",
    descriptionKey: "store.site.airwindows",
  },
  // Samples
  {
    id: "freesound",
    name: "Freesound",
    url: "https://freesound.org/",
    category: "samples",
    descriptionKey: "store.site.freesound",
  },
  {
    id: "looperman",
    name: "Looperman",
    url: "https://www.looperman.com/loops",
    category: "samples",
    descriptionKey: "store.site.looperman",
  },
  {
    id: "samplefocus",
    name: "Sample Focus",
    url: "https://samplefocus.com/",
    category: "samples",
    descriptionKey: "store.site.samplefocus",
  },
  {
    id: "cymatics",
    name: "Cymatics",
    url: "https://cymatics.fm/pages/free-download-vault",
    category: "samples",
    descriptionKey: "store.site.cymatics",
  },
  {
    id: "splice",
    name: "Splice Sounds",
    url: "https://splice.com/sounds/free",
    category: "samples",
    descriptionKey: "store.site.splice",
  },
  // SoundFonts
  {
    id: "musical-artifacts",
    name: "Musical Artifacts",
    url: "https://musical-artifacts.com/artifacts?formats=sf2",
    category: "soundfonts",
    descriptionKey: "store.site.musical-artifacts",
  },
  {
    id: "woolyss",
    name: "Woolyss SoundFonts",
    url: "https://woolyss.com/chipmusic-soundfonts.php",
    category: "soundfonts",
    descriptionKey: "store.site.woolyss",
  },
  // Presets
  {
    id: "patchstorage",
    name: "Patchstorage",
    url: "https://patchstorage.com/",
    category: "presets",
    descriptionKey: "store.site.patchstorage",
  },
];

const CATEGORIES: { id: string; labelKey: I18nKey }[] = [
  { id: "all", labelKey: "store.site.category.all" },
  { id: "plugins", labelKey: "store.site.category.plugins" },
  { id: "samples", labelKey: "store.site.category.samples" },
  { id: "soundfonts", labelKey: "store.site.category.soundfonts" },
  { id: "presets", labelKey: "store.site.category.presets" },
];

/** Our own glyph per category; the repository ships no third-party logos. */
function CategoryGlyph(props: { category: StoreSite["category"] }) {
  switch (props.category) {
    case "plugins": return <PlugIcon />;
    case "samples": return <WaveIcon />;
    case "soundfonts": return <DiskIcon />;
    case "presets": return <FolderIcon />;
  }
}

/**
 * The site's own favicon once the backend has it, otherwise our glyph. The
 * glyph shows while the request runs, so a slow or blocked site never leaves
 * an empty card.
 */
function StoreCardIcon(props: { site: StoreSite }) {
  const [src, setSrc] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal(false);
  createEffect(on(() => props.site.url, (url) => {
    let live = true;
    setSrc(null);
    setFailed(false);
    void siteIcon(url).then((value) => {
      if (live) setSrc(value);
    });
    onCleanup(() => {
      live = false;
    });
  }));
  return (
    <Show
      when={!failed() && src()}
      fallback={
        <span class="store-card-icon store-card-icon-fallback" data-category={props.site.category} aria-hidden="true">
          <CategoryGlyph category={props.site.category} />
        </span>
      }
    >
      {(icon) => (
        <img
          class="store-card-icon"
          src={icon()}
          alt=""
          aria-hidden="true"
          onError={() => setFailed(true)}
        />
      )}
    </Show>
  );
}

export function StoreBrowser() {
  const [tab, setTab] = createSignal<"sites" | "plugins">("sites");
  const [filter, setFilter] = createSignal("");
  const [category, setCategory] = createSignal("all");
  const [openWindows, setOpenWindows] = createSignal<Set<string>>(new Set());

  const filteredSites = () => {
    const q = filter().toLowerCase();
    const cat = category();
    return STORE_SITES.filter((site) => {
      if (cat !== "all" && site.category !== cat) return false;
      if (q && !site.name.toLowerCase().includes(q) && !i18n.t(site.descriptionKey).toLowerCase().includes(q)) return false;
      return true;
    });
  };

  const openSite = async (site: StoreSite) => {
    try {
      const { WebviewWindow } = await import("@tauri-apps/api/webviewWindow");
      const label = `store-${site.id}`;

      // Check if window already open
      const existing = await WebviewWindow.getByLabel(label);
      if (existing) {
        await existing.setFocus();
        return;
      }

      // Create new browser window
      const win = new WebviewWindow(label, {
        url: site.url,
        title: i18n.t("store.site.windowTitle", { name: site.name }),
        width: 1100,
        height: 750,
        center: true,
      });

      win.once("tauri://created", () => {
        setOpenWindows((prev) => { const next = new Set(prev); next.add(site.id); return next; });
      });

      win.once("tauri://destroyed", () => {
        setOpenWindows((prev) => { const next = new Set(prev); next.delete(site.id); return next; });
      });
    } catch (e) {
      console.error("Failed to open store window:", e);
    }
  };

  return (
    <div class="store-browser">
      <div class="store-header">
        <div class="store-header-row">
          <span class="store-title">{i18n.t("store.site.title")}</span>
          <Show when={tab() === "sites"}>
            <span class="store-count">{filteredSites().length}/{STORE_SITES.length}</span>
          </Show>
        </div>
        <div class="store-tabs" role="tablist">
          <button
            type="button"
            class="store-tab"
            classList={{ active: tab() === "sites" }}
            role="tab"
            aria-selected={tab() === "sites"}
            onClick={() => setTab("sites")}
          >
            {i18n.t("store.tab.sites")}
          </button>
          <button
            type="button"
            class="store-tab"
            classList={{ active: tab() === "plugins" }}
            role="tab"
            aria-selected={tab() === "plugins"}
            onClick={() => setTab("plugins")}
          >
            {i18n.t("store.tab.plugins")}
          </button>
        </div>
        <Show when={tab() === "sites"}>
        <div class="store-filters">
          <input
            type="text"
            class="store-search"
            placeholder={i18n.t("store.site.search")}
            aria-label={i18n.t("store.site.searchLabel")}
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
          <select
            class="store-category"
            aria-label={i18n.t("store.site.category")}
            value={category()}
            onChange={(e) => setCategory(e.currentTarget.value)}
          >
            <For each={CATEGORIES}>
              {(cat) => <option value={cat.id}>{i18n.t(cat.labelKey)}</option>}
            </For>
          </select>
        </div>
        </Show>
      </div>

      <Show when={tab() === "sites"} fallback={<PluginStore />}>
      <div class="store-scroll">
        <div class="store-grid">
          <For each={filteredSites()}>
            {(site) => {
              const isOpen = () => openWindows().has(site.id);
              return (
                <button
                  type="button"
                  class="store-card"
                  classList={{ open: isOpen() }}
                  onClick={() => openSite(site)}
                  {...useTooltip(() => i18n.t("store.site.openTooltip", { name: site.name }))}
                >
                  <StoreCardIcon site={site} />
                  <span class="store-card-body">
                    <span class="store-card-name">
                      {site.name}
                      <Show when={isOpen()}>
                        <span class="store-card-badge">{i18n.t("store.site.open")}</span>
                      </Show>
                    </span>
                    <span class="store-card-desc" {...useTooltip(() => i18n.t(site.descriptionKey))}>{i18n.t(site.descriptionKey)}</span>
                    <span class="store-card-host">
                      {site.url.replace(/^https?:\/\/(www\.)?/, "").split("/")[0]}
                    </span>
                  </span>
                </button>
              );
            }}
          </For>
        </div>

        <Show when={filteredSites().length === 0}>
          <div class="empty-state store-empty">
            <div class="empty-state-text">{i18n.t("store.site.empty")}</div>
            <div class="empty-state-hint">{i18n.t("store.site.emptyHint")}</div>
          </div>
        </Show>
      </div>
      </Show>

      <div class="store-footer">
        <Show
          when={tab() === "sites"}
          fallback={<span>{i18n.t("store.plugins.footer")}</span>}
        >
          <span>{i18n.t("store.site.footer")}</span>
          <span class="store-footer-muted">{i18n.t("store.site.footerMuted")}</span>
        </Show>
      </div>
    </div>
  );
}
