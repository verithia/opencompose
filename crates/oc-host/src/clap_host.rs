//! CLAP plugin host — loads and processes CLAP audio plugins.
//!
//! Uses clap-sys raw bindings. We implement the host interface,
//! load plugin entries from .clap bundles, and instantiate them.

use std::ffi::CStr;
use std::path::Path;

use super::{PluginCategory, PluginFormat, PluginInfo};
use crate::clap_guard::{self, Fault};

/// Information extracted from a CLAP plugin entry
pub fn scan_clap_bundle(path: &Path) -> Vec<PluginInfo> {
    let mut results = Vec::new();

    let path_str = match path.to_str() {
        Some(s) => s,
        None => return results,
    };

    let lease = match crate::clap_module::acquire_native_module(path) {
        Ok(lease) => lease,
        Err(error) => {
            log::warn!("CLAP scan refused '{}': {error}", path.display());
            return results;
        }
    };
    let failure = lease.failure();
    let factory = lease.module().factory();
    // Enumerate plugins
    // SAFETY: The initialized module lease retains this immutable CLAP factory descriptor throughout callback lookup.
    let get_count = unsafe { (*factory).get_plugin_count };
    // SAFETY: The initialized module lease retains this immutable CLAP factory descriptor throughout callback lookup.
    let get_desc = unsafe { (*factory).get_plugin_descriptor };
    let (Some(get_count_fn), Some(get_desc_fn)) = (get_count, get_desc) else {
        return results;
    };
    // SAFETY: this lease retains the initialized factory for the complete call.
    let Some(count) = scan_call(
        &failure,
        crate::clap_failure::ClapOperation::FactoryCount,
        // SAFETY: The strong Core/module lease retains the initialized descriptor; control/audio admission surrounds this exact-signature shim and its borrowed arguments remain live until return.
        unsafe { clap_guard::factory_count(get_count_fn, factory) },
    ) else {
        return results;
    };
    if count > 4096 {
        log::warn!("CLAP scan refused an excessive descriptor count: {count}");
        return results;
    }
    for i in 0..count {
        let Some(desc) =
            // SAFETY: this callback comes from the still-mapped CLAP entry/factory; the shim preserves its exact signature.
            scan_call(&failure, crate::clap_failure::ClapOperation::FactoryDescriptor, unsafe { clap_guard::factory_descriptor(get_desc_fn, factory, i) })
        else {
            return results;
        };
        if desc.is_null() {
            continue;
        }

        // SAFETY: The CLAP descriptor contract supplies NUL-terminated strings; the scan lease retains descriptor storage/code until all strings have been copied.
        let id = unsafe { CStr::from_ptr((*desc).id) }
            .to_string_lossy()
            .to_string();
        // SAFETY: The CLAP descriptor contract supplies NUL-terminated strings; the scan lease retains descriptor storage/code until all strings have been copied.
        let name = unsafe { CStr::from_ptr((*desc).name) }
            .to_string_lossy()
            .to_string();
        // SAFETY: The module scan lease retains the CLAP descriptor and its SDK-defined terminated feature/string storage throughout this bounded enumeration.
        let vendor = if unsafe { (*desc).vendor }.is_null() {
            String::new()
        } else {
            // SAFETY: The CLAP descriptor contract supplies NUL-terminated strings; the scan lease retains descriptor storage/code until all strings have been copied.
            unsafe { CStr::from_ptr((*desc).vendor) }
                .to_string_lossy()
                .to_string()
        };
        // SAFETY: The module scan lease retains the CLAP descriptor and its SDK-defined terminated feature/string storage throughout this bounded enumeration.
        let version = if unsafe { (*desc).version }.is_null() {
            None
        } else {
            Some(
                // SAFETY: The CLAP descriptor contract supplies NUL-terminated strings; the scan lease retains descriptor storage/code until all strings have been copied.
                unsafe { CStr::from_ptr((*desc).version) }
                    .to_string_lossy()
                    .to_string(),
            )
        };

        // Determine category from features
        let mut category = PluginCategory::Effect;
        // SAFETY: The module scan lease retains the CLAP descriptor and its SDK-defined terminated feature/string storage throughout this bounded enumeration.
        if !unsafe { (*desc).features }.is_null() {
            let mut fi = 0;
            while fi < 128 {
                // SAFETY: The module scan lease retains the CLAP descriptor and its SDK-defined terminated feature/string storage throughout this bounded enumeration.
                let feat_ptr = unsafe { *(*desc).features.add(fi) };
                if feat_ptr.is_null() {
                    break;
                }
                // SAFETY: The CLAP descriptor contract supplies NUL-terminated strings; the scan lease retains descriptor storage/code until all strings have been copied.
                let feat = unsafe { CStr::from_ptr(feat_ptr) }.to_string_lossy();
                if feat.contains("instrument") || feat.contains("synthesizer") {
                    category = PluginCategory::Instrument;
                    break;
                }
                if feat.contains("analyzer") {
                    category = PluginCategory::Analyzer;
                }
                fi += 1;
            }
        }

        results.push(PluginInfo {
            id,
            name,
            vendor,
            format: PluginFormat::Clap,
            path: path_str.to_string(),
            category,
            num_params: 0,
            version,
            architectures: vec![],
            state_abi: None,
        });
    }

    results
}

fn scan_call<T>(
    failure: &crate::clap_failure::ClapFailure,
    operation: crate::clap_failure::ClapOperation,
    result: Result<T, Fault>,
) -> Option<T> {
    result
        .map_err(|fault| {
            failure.record(operation, fault);
            log::warn!("CLAP scan aborted: {}", fault.describe_for("CLAP"));
        })
        .ok()
}
