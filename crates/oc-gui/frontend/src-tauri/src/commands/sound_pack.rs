//! Starter sound packs: the signed catalog's archives and the managed sounds
//! folder. Installing downloads, verifies and unpacks one pack; the plugin
//! browser and the onboarding guide both read the same list.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, State};

use crate::plugin_index::IndexClient;
use crate::sound_pack_store::{self, InstallPhase, InstalledSoundPack, SoundPackListing};

/// Payload of `sound_pack_install_progress`; `total` 0 means not known yet.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SoundPackProgress {
    pub id: String,
    pub phase: InstallPhase,
    pub done: u64,
    pub total: u64,
}

/// List what the index offers and what is installed locally.
#[tauri::command]
pub async fn sound_packs_list(client: State<'_, IndexClient>) -> Result<SoundPackListing, String> {
    let view = client.view()?;
    sound_pack_store::listing(&sound_pack_store::dir(), view.sound_packs.as_slice())
}

/// Download, verify and unpack one pack. Progress rides
/// `sound_pack_install_progress` as `{id, phase, done, total}`.
#[tauri::command]
pub async fn sound_pack_install(
    app: AppHandle,
    client: State<'_, IndexClient>,
    id: String,
) -> Result<InstalledSoundPack, String> {
    let view = client.view()?;
    let dir = sound_pack_store::dir();
    let event_id = id.clone();
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut last_phase = None;
    sound_pack_store::install(
        &view.authority,
        &dir,
        view.sound_packs.as_slice(),
        &id,
        |phase, done, total| {
            let now = Instant::now();
            let changed = last_phase != Some(phase);
            if !changed && done != total && now.duration_since(last) < Duration::from_millis(100) {
                return;
            }
            last = now;
            last_phase = Some(phase);
            let _ = app.emit(
                "sound_pack_install_progress",
                SoundPackProgress {
                    id: event_id.clone(),
                    phase,
                    done,
                    total,
                },
            );
        },
    )
    .await
}

/// Ask a running pack install to stop at its next checkpoint.
#[tauri::command]
pub fn sound_pack_cancel(id: String) -> Result<(), String> {
    crate::plugin_store::request_cancel(&id);
    Ok(())
}

/// Delete an installed pack from the managed sounds folder.
#[tauri::command]
pub fn sound_pack_remove(id: String) -> Result<InstalledSoundPack, String> {
    sound_pack_store::remove(&sound_pack_store::dir(), &id)
}
