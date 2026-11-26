//! Bounded descriptor queries for the processor architecture selected by discovery.
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ScanRuntime;
#[cfg(any(target_os = "macos", all(test, unix)))]
use super::{HelperOutcome, run_helper_query, scan_timeout};
use crate::vst2::{Vst2Instance, Vst2Param};
use crate::vst3_instance::{Vst3Instance, Vst3ParamDetail};
use crate::{PluginFormat, PluginInfo};

#[derive(Debug, Serialize, Deserialize)]
pub enum PluginParameterInventory {
    Vst2(Vec<Vst2Param>),
    Vst3(Vec<Vst3ParamDetail>),
}

/// Read the default program's parameters with the same architecture used to scan the bundle.
pub fn inspect_plugin_parameters(info: &PluginInfo) -> Result<PluginParameterInventory, String> {
    let path = Path::new(&info.path);
    let format = match info.format {
        PluginFormat::Vst2 => "vst2",
        PluginFormat::Vst3 => "vst3",
        _ => return Err("Parameter inventory requires a VST2 or VST3 plugin".into()),
    };
    match ScanRuntime::for_bundle(path, info.format) {
        ScanRuntime::Native => load(path, format, &info.name),
        _ => {
            #[cfg(target_os = "macos")]
            {
                let helper = crate::vst2_bridge::helper_path()
                    .ok_or("The OpenCompose Intel plugin worker is missing")?;
                query_helper(&helper, path, format, &info.name)
            }
            #[cfg(not(target_os = "macos"))]
            Err("Intel plugin translation is only supported on macOS".into())
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn query_helper(
    helper: &Path,
    path: &Path,
    format: &str,
    class: &str,
) -> Result<PluginParameterInventory, String> {
    let timeout = scan_timeout();
    match run_helper_query(helper, path, format, Some(class), timeout)
        .map_err(|error| format!("Intel plugin parameter inventory: {error}"))?
    {
        HelperOutcome::Finished {
            success: true,
            stdout,
            ..
        } => {
            let result = serde_json::from_slice(&stdout)
                .map_err(|error| format!("Invalid Intel plugin parameter inventory: {error}"))?;
            if matches!(
                (&result, format),
                (PluginParameterInventory::Vst2(_), "vst2")
                    | (PluginParameterInventory::Vst3(_), "vst3")
            ) {
                Ok(result)
            } else {
                Err("Intel plugin worker returned an inventory for the wrong format".into())
            }
        }
        HelperOutcome::Finished {
            stderr,
            code,
            signal,
            ..
        } => Err(format!(
            "Intel plugin parameter inventory failed (exit {code:?}, signal {signal:?}): {stderr}"
        )),
        HelperOutcome::TimedOut { .. } => Err(format!(
            "Intel plugin parameter inventory exceeded {} ms",
            timeout.as_millis()
        )),
    }
}

fn load(path: &Path, format: &str, class: &str) -> Result<PluginParameterInventory, String> {
    match format {
        "vst2" => {
            let instance = Vst2Instance::load(path, 48_000, 512)?;
            Ok(PluginParameterInventory::Vst2(instance.get_params()))
        }
        "vst3" => {
            let instance = Vst3Instance::load_class(path, class, 48_000, 512)?;
            let params = instance.get_param_details();
            if let Some(error) = instance.failure().message() {
                return Err(error);
            }
            Ok(PluginParameterInventory::Vst3(params))
        }
        _ => Err("Parameter inventory requires a VST2 or VST3 plugin".into()),
    }
}

pub(super) fn emit(path: &Path, format: &str, class: &str) -> Result<(), String> {
    let parameters = load(path, format, class)?;
    println!(
        "{}",
        serde_json::to_string(&parameters).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn helper_queries_pass_the_resolved_class_and_reject_a_wrong_format() {
        let root = oc_test_support::temp_dir("scanner", "parameters");
        let helper = root.join("worker");
        fs::write(
            &helper,
            "#!/bin/sh\n[ \"$3\" = '--format' ] && [ \"$5\" = '--path' ] && [ \"$7\" = '--parameters-for' ] && [ \"$8\" = 'Alternate Class' ] || exit 2\nprintf '%s' '{\"Vst2\":[{\"index\":3,\"name\":\"Mix\",\"label\":\"%\",\"display\":\"50\",\"value\":0.5,\"default\":0.5,\"can_automate\":true}]}'\n",
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let bundle = root.join("Bundle with spaces.vst");
        let PluginParameterInventory::Vst2(params) =
            query_helper(&helper, &bundle, "vst2", "Alternate Class").unwrap()
        else {
            panic!("worker returned the wrong inventory");
        };
        assert_eq!((params[0].index, params[0].name.as_str()), (3, "Mix"));
        assert!(query_helper(&helper, &bundle, "vst3", "Alternate Class").is_err());
    }
}
