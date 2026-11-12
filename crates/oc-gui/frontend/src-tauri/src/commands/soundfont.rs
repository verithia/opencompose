//! Instrument packs: the signed index's download list and the user's own
//! soundfont folder. The plugin browser is the primary path for the user; the
//! agent surface reads the list and leaves the download to a click.

use tauri::{AppHandle, Emitter, State};

use crate::plugin_index::IndexClient;
use crate::soundfont_store::{self, InstalledPack, SoundfontListing};

/// List what the index offers and what is installed locally.
#[tauri::command]
pub async fn soundfonts_list(client: State<'_, IndexClient>) -> Result<SoundfontListing, String> {
    let view = client.view()?;
    soundfont_store::listing(&soundfont_store::dir(), view.packs.as_slice())
}

/// Download one pack: stream, verify size and sha256, then rename into place.
/// Progress rides the `soundfont_progress` event as `{id, received, total}`.
#[tauri::command]
pub async fn soundfont_install(
    app: AppHandle,
    client: State<'_, IndexClient>,
    id: String,
) -> Result<InstalledPack, String> {
    let view = client.view()?;
    let dir = soundfont_store::dir();
    let event_id = id.clone();
    soundfont_store::install(
        &view.authority,
        &dir,
        view.packs.as_slice(),
        &id,
        |received, total| {
            let _ = app.emit(
                "soundfont_progress",
                serde_json::json!({ "id": event_id, "received": received, "total": total }),
            );
        },
    )
    .await
}

/// Delete an installed pack from the user's soundfont folder.
#[tauri::command]
pub fn soundfonts_remove(id: String) -> Result<InstalledPack, String> {
    soundfont_store::remove(&soundfont_store::dir(), &id)
}
