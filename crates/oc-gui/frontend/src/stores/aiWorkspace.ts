/**
 * The AI settings the panel and the Preferences page share.
 *
 * Everything that belongs to the open conversation — the edit target, its
 * permission, the enabled toolsets — lives here rather than in the panel, so
 * closing the sidebar does not reset it and the Preferences page can edit the
 * same value the next turn reads.
 */
import { createSignal } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import type { WireMessage } from "../ai/wire";
import { errorMessage } from "../ai/errors";
import { loadCatalog, type Catalog } from "../ai/catalog";
import { getActiveProvider } from "../ai/config";
import { defaultSelection, loadToolsets, type ToolsetDef } from "../ai/toolsets";
import { DEFAULT_PERMISSION, permissionOf, type PermissionMode } from "../ai/permission";
import { aiSession } from "./aiSession";
import { i18n } from "./i18n";

const TOOLSETS_KEY = "opencompose.ai.toolsets.v1";
const TARGET_KEY = "opencompose.ai.target.v1";

function readString(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeString(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // A full or unavailable store keeps the in-memory value for this run.
  }
}

function readSelection(): string[] | null {
  const raw = readString(TOOLSETS_KEY);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((entry): entry is string => typeof entry === "string") : null;
  } catch {
    return null;
  }
}

const [catalog, setCatalog] = createSignal<Catalog | null>(null);
const [catalogError, setCatalogError] = createSignal<string | null>(null);
const [toolsets, setToolsets] = createSignal<ToolsetDef[]>([]);
const [selectedSets, setSelectedSets] = createSignal<string[] | null>(readSelection());
const [toolDescriptions, setToolDescriptions] = createSignal<ReadonlyMap<string, string>>(new Map());
const [targetPath, setTargetPathSignal] = createSignal(readString(TARGET_KEY) ?? "project");
const [permission, setPermissionSignal] = createSignal<PermissionMode>(DEFAULT_PERMISSION);
const [panelBusy, setPanelBusy] = createSignal(false);
const [settingsBusy, setSettingsBusy] = createSignal(false);
const [testing, setTesting] = createSignal(false);
const [contextEstimate, setContextEstimate] = createSignal(0);
const [notice, setNotice] = createSignal<{ text: string | null; tone: "idle" | "success" | "error" }>({
  text: null,
  tone: "idle",
});

let catalogRequest: Promise<Catalog | null> | null = null;
/** Bumped by every catalog read, so a superseded reply cannot publish. */
let catalogGeneration = 0;
let toolsetsRequest: Promise<void> | null = null;

export const aiWorkspace = {
  catalog,
  catalogError,
  toolsets,
  selectedSets,
  toolDescriptions,
  targetPath,
  permission,
  contextEstimate,
  notice,
  busy: () => panelBusy() || testing(),
  testing,
  settingsBusy,
  setSettingsBusy,
  /** Loads the catalog once; `force` re-reads it after a provider is added. */
  async ensureCatalog(force = false): Promise<Catalog | null> {
    if (!force && catalog()) return catalog();
    if (!force && catalogRequest) return catalogRequest;
    // A forced read supersedes one still in flight. Only the newest request may
    // publish its answer or clear the shared slot, so the older one cannot make
    // the newer read look settled.
    const generation = ++catalogGeneration;
    const request = loadCatalog()
      .then((value) => {
        if (generation === catalogGeneration) {
          setCatalog(value);
          setCatalogError(null);
        }
        return value;
      })
      .catch((error) => {
        if (generation === catalogGeneration) setCatalogError(errorMessage(error));
        return null;
      })
      .finally(() => {
        if (generation === catalogGeneration) catalogRequest = null;
      });
    catalogRequest = request;
    return request;
  },
  /** Loads the toolset definitions once and keeps a saved selection valid. */
  async ensureToolsets(force = false): Promise<void> {
    if (toolsetsRequest) return toolsetsRequest;
    if (!force && toolsets().length) return;
    toolsetsRequest = (async () => {
      const sets = await loadToolsets();
      setToolsets(sets);
      if (!sets.length) {
        setSelectedSets(null);
      } else {
        const saved = readSelection();
        if (saved) {
          const available = new Set(sets.map((set) => set.id));
          const kept = saved.filter((id) => available.has(id));
          setSelectedSets(kept);
          writeString(TOOLSETS_KEY, JSON.stringify(kept));
        } else {
          const defaults = defaultSelection(sets);
          setSelectedSets(defaults);
          writeString(TOOLSETS_KEY, JSON.stringify(defaults));
        }
      }
      // The descriptions are the same catalog even when the backend exposes no
      // toolsets; the toolset tooltips read them.
      try {
        const tools = await invoke<{ name: string; description?: string }[]>("ai_tools", {});
        if (Array.isArray(tools)) {
          setToolDescriptions(new Map(tools.map((tool) => [tool.name, tool.description ?? ""])));
        }
      } catch {
        setToolDescriptions(new Map());
      }
    })().finally(() => {
      toolsetsRequest = null;
    });
    return toolsetsRequest;
  },
  toggleSet(id: string) {
    const current = selectedSets() ?? [];
    const next = current.includes(id) ? current.filter((item) => item !== id) : [...current, id];
    setSelectedSets(next);
    writeString(TOOLSETS_KEY, JSON.stringify(next));
  },
  setTargetPath(path: string) {
    setTargetPathSignal(path);
    writeString(TARGET_KEY, path);
  },
  /** Sets the permission and stores it on the open conversation. */
  async choosePermission(mode: PermissionMode): Promise<void> {
    setPermissionSignal(mode);
    const id = aiSession.open();
    if (!id) return;
    await invoke("sessions_set_permission", { id, permission: mode });
  },
  /** Adopts a loaded conversation's stored mode without writing it back. */
  adoptPermission(value: unknown) {
    setPermissionSignal(permissionOf(value));
  },
  setPanelBusy(value: boolean) {
    setPanelBusy(value);
  },
  setContextEstimate(value: number) {
    setContextEstimate(value);
  },
  notify(text: string | null, tone: "idle" | "success" | "error" = "idle") {
    setNotice({ text, tone });
  },
  /** One small request through the saved provider, for the page's Test button. */
  async testConnection(): Promise<void> {
    if (panelBusy() || testing()) return;
    setTesting(true);
    setNotice({ text: i18n.t("ai.settings.testing"), tone: "idle" });
    const requestId = crypto.randomUUID();
    const onDelta = new Channel<string>();
    onDelta.onmessage = () => {};
    try {
      const reply = await invoke<WireMessage>("ai_chat", {
        providerId: getActiveProvider().id,
        requestId,
        messages: [{ role: "user", content: "Say hello in one word." }],
        useTools: false,
        onDelta,
      });
      const text = (reply.content ?? "").trim();
      setNotice({ text: i18n.t("ai.settings.connected", { reply: text.slice(0, 100) }), tone: "success" });
    } catch (error) {
      setNotice({ text: errorMessage(error), tone: "error" });
    } finally {
      setTesting(false);
    }
  },
};
