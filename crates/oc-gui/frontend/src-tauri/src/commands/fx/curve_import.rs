//! A bounded file import publishes one inline asset through the ordinary effect edit.
use super::*;
use oc_device_schema::transfer_curve::{MAX_BYTES, TransferCurve};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CurveImportRequest {
    pub target: String,
    pub index: usize,
    pub path: String,
    pub expected_revision: u64,
}
#[derive(Debug, Serialize)]
pub struct CurveImportReceipt {
    pub revision: u64,
    pub source_path: String,
    pub sha256: String,
    pub samples: usize,
    pub domain: oc_device_schema::transfer_curve::CurveDomain,
}

fn read_curve(path: &Path) -> Result<(String, TransferCurve), String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Curve input must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Curve input exceeds the byte limit".into());
    }
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let curve = TransferCurve::decode(&text).map_err(|e| e.to_string())?;
    Ok((text, curve))
}

pub(crate) fn import_curve_inner(
    state: &AppState,
    request: &CurveImportRequest,
) -> Result<CurveImportReceipt, String> {
    let (stamp, path) = {
        let inner = state.lock_inner();
        inner.refuse_in_preview()?;
        if inner.revision != request.expected_revision {
            return Err("Project revision changed before curve import".into());
        }
        let mut project = inner.project.clone();
        let effect = effect_chain_mut(&mut project, &request.target)?
            .get(request.index)
            .ok_or("Effect index out of range")?;
        if effect.effect_type != ast::EffectType::TransferShaper {
            return Err("Curve import requires a transfer shaper".into());
        }
        let path = Path::new(&request.path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            inner
                .file_path
                .as_deref()
                .and_then(Path::parent)
                .ok_or("Save the project before importing a relative path")?
                .join(path)
        };
        (
            inner
                .undo
                .parameter_target_stamp()
                .cloned()
                .ok_or("Document target unavailable")?,
            path,
        )
    };
    // File I/O and parsing do not hold the project lock or run on the audio callback.
    let (text, curve) = read_curve(&path)?;
    let mut inner = state.lock_inner();
    super::super::shared::require_parameter_admission(&inner, &stamp)?;
    if inner.revision != request.expected_revision {
        return Err("Project revision changed during curve import".into());
    }
    set_effect_param_inner(&mut inner, &request.target, request.index, "curve", &text)?;
    Ok(CurveImportReceipt {
        revision: inner.revision,
        source_path: path.to_string_lossy().into_owned(),
        sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        samples: curve.samples.len(),
        domain: curve.domain,
    })
}

#[tauri::command]
pub fn import_effect_curve(
    request: CurveImportRequest,
    state: State<AppState>,
) -> Result<CurveImportReceipt, String> {
    import_curve_inner(state.inner(), &request)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_curve_import_preserves_bytes_and_refuses_stale_or_invalid_without_history_change() {
        let root = oc_test_support::temp_dir("curve-import", "atomic");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("curve.json");
        let text = " { \"version\": 1, \"domain\": \"odd\", \"samples\": [0, 0.75, 1] } ";
        std::fs::write(&path, text).unwrap();
        let state = AppState::new();
        crate::commands::open_project(
            &mut state.lock_inner(),
            "@project\n@track t | instrument: audio/clip\n  effects:\n    - transfer_shaper\n",
            None,
        )
        .unwrap();
        let rev = state.lock_inner().revision;
        let mut request = CurveImportRequest {
            target: "track:0".into(),
            index: 0,
            path: path.display().to_string(),
            expected_revision: rev,
        };
        let receipt = import_curve_inner(&state, &request).unwrap();
        assert_eq!(receipt.samples, 3);
        let inner = state.lock_inner();
        assert_eq!(
            inner.project.tracks[0].effects[0].params[0].1,
            ast::ParamValue::String(text.into())
        );
        let undo = inner.undo.undo_entries().len();
        let before = inner.dsl_source.clone();
        drop(inner);
        assert!(import_curve_inner(&state, &request).is_err());
        request.expected_revision = receipt.revision;
        std::fs::write(&path, "{}").unwrap();
        assert!(import_curve_inner(&state, &request).is_err());
        let inner = state.lock_inner();
        assert_eq!(inner.dsl_source, before);
        assert_eq!(inner.undo.undo_entries().len(), undo);
        assert_eq!(inner.revision, receipt.revision);
    }
    #[test]
    fn transfer_curve_catalog_and_compact_path_tool_share_the_asset_contract() {
        use crate::tool_registry::arguments::ToolArguments;
        let catalog = crate::device_catalog::catalog(&serde_json::json!({})).unwrap();
        assert_eq!(catalog["assets"][0]["param"], "curve");
        assert_eq!(catalog["assets"][0]["max_samples"], 65_537);
        let tool = crate::tool_registry::panel_tool("effects_import_curve").unwrap();
        assert_eq!(tool.id, crate::tool_registry::ToolId::EffectsImportCurve);
        let args = serde_json::json!({"target":"track:0","index":0,"path":"curve.json","expected_revision":8});
        let request = CurveImportRequest::decode(&args).unwrap();
        assert_eq!(request.expected_revision, 8);
        assert!(!tool.schema.to_string().contains("samples"));
    }
}
