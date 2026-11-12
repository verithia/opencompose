/**
 * The AI page: provider connection, the active model and its defaults, what the
 * open conversation may change, and the tools it may call.
 *
 * The page is the AI panel's settings surface. The panel's Settings button
 * opens this page; a provider or key edit reaches the next turn through the
 * same config store the composer reads.
 */
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { AiSettingsModal } from "../AiSettingsModal";
import { AddProviderModal } from "../AddProviderModal";
import { aiConfigLoaded, loadConfig } from "../../ai/config";
import { errorMessage } from "../../ai/errors";
import { i18n } from "../../stores/i18n";
import { aiWorkspace } from "../../stores/aiWorkspace";
import { preferencesUi } from "../../stores/preferencesUi";

export function AiPage() {
  const [addProvider, setAddProvider] = createSignal(false);

  onMount(() => {
    aiWorkspace.setSettingsBusy(false);
    aiWorkspace.notify(null);
    void (async () => {
      if (!aiConfigLoaded()) {
        try {
          await loadConfig();
        } catch (error) {
          aiWorkspace.notify(errorMessage(error), "error");
        }
      }
      await Promise.all([aiWorkspace.ensureCatalog(), aiWorkspace.ensureToolsets()]);
    })();
  });
  onCleanup(() => aiWorkspace.setSettingsBusy(false));

  const added = (name: string) => {
    setAddProvider(false);
    void (async () => {
      try {
        await loadConfig();
        await aiWorkspace.ensureCatalog(true);
        aiWorkspace.notify(i18n.t("ai.provider.added", { name }), "success");
      } catch (error) {
        aiWorkspace.notify(errorMessage(error), "error");
      }
    })();
  };

  return (
    <>
      <p class="preferences-hint preferences-ai-save-hint">{i18n.t("preferences.ai.saveHint")}</p>
      <AiSettingsModal
        embedded
        catalog={aiWorkspace.catalog()}
        loading={aiWorkspace.busy()}
        testing={aiWorkspace.testing()}
        contextEstimate={aiWorkspace.contextEstimate()}
        testResult={aiWorkspace.notice().text}
        noticeTone={aiWorkspace.notice().tone}
        targetPath={aiWorkspace.targetPath()}
        permission={aiWorkspace.permission()}
        onPermission={(mode) => {
          void aiWorkspace.choosePermission(mode).catch((error) =>
            aiWorkspace.notify(errorMessage(error), "error"));
        }}
        toolsets={aiWorkspace.toolsets()}
        selectedSets={aiWorkspace.selectedSets()}
        toolDescriptions={aiWorkspace.toolDescriptions()}
        focus={preferencesUi.focus()}
        onClose={() => void preferencesUi.requestClose(() => preferencesUi.close())}
        onTestResult={(text, tone) => aiWorkspace.notify(text, tone)}
        onTargetPath={(path) => aiWorkspace.setTargetPath(path)}
        onToggleSet={(id) => aiWorkspace.toggleSet(id)}
        onTestConnection={() => void aiWorkspace.testConnection()}
        onCatalogReload={() => void aiWorkspace.ensureCatalog(true)}
        onAddProvider={() => setAddProvider(true)}
        onBusyChange={(busy) => aiWorkspace.setSettingsBusy(busy)}
      />
      <div class="ai-settings-page-foot">
        <button
          type="button"
          class="toolbar-btn text-btn"
          onClick={() => void preferencesUi.requestClose(() => preferencesUi.close())}
        >{i18n.t("ai.settings.done")}</button>
      </div>
      <Show when={addProvider()}>
        <AddProviderModal
          catalog={aiWorkspace.catalog()}
          disabled={aiWorkspace.busy()}
          onClose={() => setAddProvider(false)}
          onAdded={added}
        />
      </Show>
    </>
  );
}
