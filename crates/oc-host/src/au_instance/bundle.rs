//! AudioUnit discovery reads declarations without instantiating components.

use std::{fs::File, path::Path};

use super::{
    AuCategory, AuError, AuErrorKind, AudioComponentDescription, api_lock, au_guard, category_of,
    component_name, normalize_key, resolve_component, split_display_name,
};
use crate::{PluginCategory, PluginFormat, PluginInfo, display_path};

const MAX_BUNDLE_METADATA_BYTES: u64 = 4 * 1024 * 1024;

pub(super) struct Component {
    pub(super) description: AudioComponentDescription,
    name: String,
}

pub(super) struct Bundle {
    stem: String,
    version: Option<String>,
    components: Vec<Component>,
}

impl Bundle {
    pub(super) fn read(path: &Path) -> Result<Self, AuError> {
        let metadata_path = if path.is_dir() {
            path.join("Contents/Info.plist")
        } else {
            path.to_path_buf()
        };
        let invalid = |message: &dyn std::fmt::Display| {
            AuError::new(
                AuErrorKind::InvalidDescription,
                format!("could not read '{}': {message}", metadata_path.display()),
            )
        };
        let info = std::fs::metadata(&metadata_path).map_err(|error| invalid(&error))?;
        if !info.is_file() || info.len() > MAX_BUNDLE_METADATA_BYTES {
            return Err(invalid(&"AudioUnit bundle metadata exceeds its size limit"));
        }
        let file = File::open(&metadata_path).map_err(|error| invalid(&error))?;
        let metadata = plist::Value::from_reader(std::io::BufReader::new(file))
            .map_err(|error| invalid(&error))?;
        let root = metadata
            .as_dictionary()
            .ok_or_else(|| invalid(&"expected a property list dictionary"))?;
        let components = root
            .get("AudioComponents")
            .and_then(plist::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.as_dictionary()?;
                let string = |key| entry.get(key).and_then(plist::Value::as_string);
                let description = AudioComponentDescription::from_codes(
                    string("type")?,
                    string("subtype")?,
                    string("manufacturer")?,
                )?;
                (category_of(&description) != AuCategory::Unsupported).then(|| Component {
                    description,
                    name: string("name").unwrap_or_default().to_string(),
                })
            })
            .collect();
        let version = ["CFBundleShortVersionString", "CFBundleVersion"]
            .into_iter()
            .find_map(|key| root.get(key).and_then(plist::Value::as_string))
            .map(str::to_string);
        Ok(Self {
            stem: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            version,
            components,
        })
    }

    pub(super) fn preferred(&self, query: &str) -> Result<&Component, AuError> {
        let stem = normalize_key(&self.stem);
        let selector = normalize_key(query);
        self.components
            .iter()
            .find(|entry| {
                let name = normalize_key(&entry.name);
                !name.is_empty() && (name == stem || name == selector)
            })
            .or_else(|| self.components.first())
            .ok_or_else(|| {
                AuError::new(
                    AuErrorKind::UnsupportedType,
                    format!("'{query}' declares no instrument or effect AudioComponents entry"),
                )
            })
    }

    fn info(&self, component: &Component, path: &str, id: String) -> PluginInfo {
        let name = if component.name.is_empty() {
            &self.stem
        } else {
            &component.name
        };
        let mut info = metadata(component.description, name, path.to_owned(), id);
        info.version = self.version.clone();
        info
    }
}

fn metadata(
    description: AudioComponentDescription,
    name: &str,
    path: String,
    id: String,
) -> PluginInfo {
    let (vendor, name) = split_display_name(name);
    let category = match category_of(&description) {
        AuCategory::Instrument => PluginCategory::Instrument,
        AuCategory::Effect => PluginCategory::Effect,
        AuCategory::Unsupported => PluginCategory::Other,
    };
    PluginInfo {
        id,
        name: name.to_owned(),
        vendor: vendor.to_owned(),
        format: PluginFormat::Au,
        path,
        category,
        num_params: 0,
        version: None,
        architectures: Vec::new(),
        state_abi: None,
    }
}

fn bundle_inventory(path: &Path) -> Vec<PluginInfo> {
    let rendered_path = display_path(path);
    if let Ok(bundle) = Bundle::read(path)
        && let Ok(preferred) = bundle.preferred(&rendered_path)
    {
        let mut plugins = vec![bundle.info(preferred, &rendered_path, rendered_path.clone())];
        for component in &bundle.components {
            if component.description == preferred.description {
                continue;
            }
            let id = format!("au:{}", component.description.codes());
            if !plugins.iter().any(|plugin| plugin.id == id) {
                plugins.push(bundle.info(component, &rendered_path, id));
            }
        }
        return plugins;
    }
    // Keep legacy packages visible without assigning them a role that their
    // bundle does not declare. The OS can still resolve them by registered id.
    vec![metadata(
        AudioComponentDescription::default(),
        &path.file_stem().unwrap_or_default().to_string_lossy(),
        rendered_path.clone(),
        rendered_path,
    )]
}

pub(crate) fn collect_au_bundles(dir: &Path, all: &mut Vec<PluginInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("component") {
            continue;
        }
        all.extend(bundle_inventory(&path));
    }
}

/// Resolve a registered AudioUnit without scanning or loading other formats.
pub fn resolve_plugin_metadata(selector: &str) -> Result<PluginInfo, AuError> {
    let description = resolve_component(selector)?;
    let path = Path::new(selector);
    if path
        .extension()
        .is_some_and(|extension| extension == "component")
    {
        let bundle = Bundle::read(path)?;
        let entry = bundle
            .components
            .iter()
            .find(|entry| entry.description == description)
            .ok_or_else(|| {
                AuError::new(
                    AuErrorKind::NotFound,
                    "resolved AudioUnit is no longer in the bundle",
                )
            })?;
        return Ok(bundle.info(entry, selector, selector.to_owned()));
    }
    let _api = api_lock();
    let foreign = au_guard::Boundary::default();
    // SAFETY: the description stays live under the registry lock.
    let name =
        unsafe { component_name(&description, &foreign) }?.unwrap_or_else(|| description.codes());
    Ok(metadata(
        description,
        &name,
        String::new(),
        format!("au:{}", description.codes()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn au_bundle_inventory_keeps_path_for_primary_and_lists_other_classes() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Example.component");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(bundle.join("Contents/Info.plist"), br#"
            <plist version="1.0"><dict>
              <key>CFBundleShortVersionString</key><string>1.2</string>
              <key>AudioComponents</key><array>
                <dict><key>type</key><string>aumf</string><key>subtype</key><string>Fx01</string>
                  <key>manufacturer</key><string>Test</string><key>name</key><string>Maker: Example FX</string></dict>
                <dict><key>type</key><string>aumu</string><key>subtype</key><string>In01</string>
                  <key>manufacturer</key><string>Test</string><key>name</key><string>Maker: Example</string></dict>
                <dict><key>type</key><string>aumi</string><key>subtype</key><string>Md01</string>
                  <key>manufacturer</key><string>Test</string><key>name</key><string>Maker: Example MIDI</string></dict>
              </array>
            </dict></plist>
        "#).unwrap();
        let mut plugins = Vec::new();
        collect_au_bundles(root.path(), &mut plugins);
        let [effect, instrument] = plugins.as_slice() else {
            panic!("expected two hostable classes")
        };
        assert_eq!(effect.id, display_path(&bundle));
        assert_eq!(instrument.category, PluginCategory::Instrument);
        assert_eq!(instrument.vendor, "Maker");
        assert_eq!(instrument.version.as_deref(), Some("1.2"));
        assert_eq!(instrument.id, "au:aumu:In01:Test");
        assert_eq!(effect.path, instrument.path);
        assert_eq!(effect.category, PluginCategory::Effect);
        assert_eq!(
            crate::effect_plugin_selector(&effect.id),
            (Some(PluginFormat::Au), effect.path.as_str())
        );
        assert_eq!(
            crate::effect_plugin_selector(&format!("au.{}", effect.path)).0,
            Some(PluginFormat::Au)
        );
        assert_eq!(
            crate::effect_plugin_selector(&instrument.id),
            (Some(PluginFormat::Au), "aumu:In01:Test")
        );
    }
}
