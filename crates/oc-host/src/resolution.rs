//! Plugin identifier normalization and lookup: exact forms win, normalized
//! and bundle-identifier forms resolve only when unambiguous.

use std::path::{Path, PathBuf};

use super::{PluginCategory, PluginFormat, PluginInfo};

/// The identifier forms derived once per lookup.
pub(super) struct PluginQuery {
    raw: String,
    normalized: String,
    path: Option<PathBuf>,
    canonical_path: Option<PathBuf>,
    bundle_identifier: Option<String>,
}

impl PluginQuery {
    pub(super) fn parse(identifier: &str) -> Option<Self> {
        let raw = identifier.trim();
        let normalized = identifier_key(raw);
        if raw.is_empty() || normalized.is_empty() {
            return None;
        }
        let path = (raw.contains('/') || raw.contains('\\')).then(|| PathBuf::from(raw));
        let canonical_path = path
            .as_deref()
            .and_then(|path| std::fs::canonicalize(path).ok());
        // CFBundleIdentifier is reverse-DNS, so a dot is required.
        let bundle_identifier = (raw.contains('.') && !raw.contains('/') && !raw.contains('\\'))
            .then(|| normalized_bundle_identifier(raw));
        Some(Self {
            raw: raw.to_string(),
            normalized,
            path,
            canonical_path,
            bundle_identifier,
        })
    }

    pub(super) fn raw(&self) -> &str {
        &self.raw
    }

    /// An existing path whose extension matches the requested format.
    pub(super) fn direct_bundle_path(&self, format: PluginFormat) -> Option<&Path> {
        let path = self.path.as_deref()?;
        let extension = path.extension()?.to_str()?;
        let supported = match format {
            PluginFormat::Vst3 => extension.eq_ignore_ascii_case("vst3"),
            PluginFormat::Clap => extension.eq_ignore_ascii_case("clap"),
            PluginFormat::Au => extension.eq_ignore_ascii_case("component"),
            PluginFormat::Lv2 => extension.eq_ignore_ascii_case("lv2"),
            PluginFormat::Vst2 => ["vst", "so", "dll"]
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected)),
            PluginFormat::Internal => return None,
        };
        (supported && path.exists()).then_some(path)
    }
}

/// Resolve a query against an inventory, preferring exact identifiers.
pub(super) fn resolve_plugin<'a>(
    plugins: &'a [PluginInfo],
    query: &PluginQuery,
    format: PluginFormat,
) -> Option<&'a PluginInfo> {
    let of_format = || plugins.iter().filter(|plugin| plugin.format == format);

    if let Some(found) = of_format().find(|plugin| plugin.id == query.raw) {
        return Some(found);
    }
    if let Some(found) = of_format().find(|plugin| plugin.name == query.raw) {
        return Some(found);
    }
    // A path or bundle id names the bundle; default to its instrument class.
    if let Some(found) = preferred(of_format().filter(|plugin| plugin.path == query.raw)) {
        return Some(found);
    }
    if let Some(found) = preferred(
        of_format().filter(|plugin| canonical_path_matches(Path::new(&plugin.path), query)),
    ) {
        return Some(found);
    }
    if let Some(found) = preferred(of_format().filter(|plugin| {
        let path = Path::new(&plugin.path);
        path.file_name().and_then(|name| name.to_str()) == Some(query.raw())
            || path.file_stem().and_then(|stem| stem.to_str()) == Some(query.raw())
    })) {
        return Some(found);
    }
    if let Some(found) = unique_normalized_match(plugins, query, format) {
        return Some(found);
    }
    if query.bundle_identifier.is_some()
        && let Some(found) = preferred(
            of_format().filter(|plugin| bundle_identifier_matches(Path::new(&plugin.path), query)),
        )
    {
        return Some(found);
    }
    None
}

fn preferred<'a>(plugins: impl Iterator<Item = &'a PluginInfo>) -> Option<&'a PluginInfo> {
    let candidates: Vec<&'a PluginInfo> = plugins.collect();
    candidates
        .iter()
        .copied()
        .find(|plugin| plugin.category == PluginCategory::Instrument)
        .or_else(|| candidates.first().copied())
}

/// Whether a plugin descriptor itself satisfies the query (no bundle-id I/O).
pub(super) fn plugin_matches_query(plugin: &PluginInfo, query: &PluginQuery) -> bool {
    plugin.id == query.raw
        || plugin.name == query.raw
        || path_matches_query(Path::new(&plugin.path), query)
        || identifier_key(&plugin.id) == query.normalized
        || identifier_key(&plugin.name) == query.normalized
}

/// Whether a bundle path, its basename, or its normalized basename matches.
pub(super) fn path_matches_query(path: &Path, query: &PluginQuery) -> bool {
    if path.to_string_lossy() == query.raw {
        return true;
    }
    if canonical_path_matches(path, query) {
        return true;
    }
    let name = path.file_name().and_then(|name| name.to_str());
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    if name == Some(query.raw()) || stem == Some(query.raw()) {
        return true;
    }
    stem.is_some_and(|stem| identifier_key(stem) == query.normalized)
}

pub(super) fn canonical_path_matches(path: &Path, query: &PluginQuery) -> bool {
    let Some(expected) = query.canonical_path.as_ref() else {
        return false;
    };
    std::fs::canonicalize(path).is_ok_and(|actual| &actual == expected)
}

/// Read `CFBundleIdentifier` from a bundle's Info.plist.
pub(super) fn read_bundle_identifier(bundle: &Path) -> Option<String> {
    let data = std::fs::read(bundle.join("Contents/Info.plist")).ok()?;
    let value = plist::Value::from_reader(std::io::Cursor::new(data)).ok()?;
    value
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()
        .map(str::to_owned)
}

/// Exact bundle-identifier comparison (case and format suffix are ignored).
pub(super) fn bundle_identifier_matches(bundle: &Path, query: &PluginQuery) -> bool {
    let Some(expected) = query.bundle_identifier.as_deref() else {
        return false;
    };
    bundle.is_dir()
        && read_bundle_identifier(bundle)
            .is_some_and(|actual| normalized_bundle_identifier(&actual) == expected)
}

fn unique_normalized_match<'a>(
    plugins: &'a [PluginInfo],
    query: &PluginQuery,
    format: PluginFormat,
) -> Option<&'a PluginInfo> {
    let mut matches = plugins
        .iter()
        .filter(|plugin| plugin.format == format && plugin_matches_query(plugin, query));
    let first = matches.next()?;
    let (id, path) = (&first.id, &first.path);
    matches
        .all(|plugin| &plugin.id == id && &plugin.path == path)
        .then_some(first)
}

pub(super) fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

pub(super) fn identifier_key(value: &str) -> String {
    normalize_key(strip_plugin_extension(strip_format_prefix(value.trim())))
}

fn strip_format_prefix(value: &str) -> &str {
    for prefix in ["vst3:", "clap:", "au:", "vst2:", "audio-unit:", "lv2:"] {
        if value
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            return &value[prefix.len()..];
        }
    }
    value
}

fn strip_plugin_extension(value: &str) -> &str {
    for extension in [
        ".vst3",
        ".clap",
        ".component",
        ".lv2",
        ".vst",
        ".dll",
        ".so",
    ] {
        if let Some(stripped) = strip_suffix_ignore_ascii_case(value, extension) {
            return stripped;
        }
    }
    value
}

fn strip_suffix_ignore_ascii_case<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let start = value.len().checked_sub(suffix.len())?;
    let tail = value.get(start..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &value[..start])
}

fn normalized_bundle_identifier(value: &str) -> String {
    strip_plugin_extension(value.trim()).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCategory;

    fn plugin(id: &str, name: &str, path: &str) -> PluginInfo {
        PluginInfo {
            id: id.to_string(),
            name: name.to_string(),
            vendor: String::new(),
            format: PluginFormat::Vst3,
            path: path.to_string(),
            category: PluginCategory::Instrument,
            num_params: 0,
            version: None,
            architectures: vec![],
            state_abi: None,
        }
    }

    fn halcyon(path: &str) -> PluginInfo {
        plugin("vst3:halcyon_2", "Halcyon 2", path)
    }

    fn temp_dir(label: &str) -> PathBuf {
        oc_test_support::temp_dir("ocp-plugin-resolve", label)
    }

    fn write_bundle(root: &Path, identifier: &str) -> PathBuf {
        let bundle = root.join("Halcyon2.vst3");
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>{identifier}</string>
</dict>
</plist>
"#
            ),
        )
        .unwrap();
        bundle
    }

    #[test]
    fn resolves_id_name_path_and_basename_forms() {
        let plugins = vec![
            halcyon("/plugins/Halcyon2.vst3"),
            plugin("vst3:halcyon", "Halcyon", "/plugins/Halcyon.vst3"),
            plugin("vst3:vital", "Vital", "/plugins/Vital.vst3"),
        ];
        for identifier in [
            "vst3:halcyon_2",
            "VST3:HALCYON_2",
            "/plugins/Halcyon2.vst3",
            "Halcyon2",
            "Halcyon2.vst3",
            "Halcyon 2",
            "halcyon 2",
            "halcyon2",
            "halcyon_2",
        ] {
            let query = PluginQuery::parse(identifier).unwrap();
            let found = resolve_plugin(&plugins, &query, PluginFormat::Vst3)
                .unwrap_or_else(|| panic!("{identifier} did not resolve"));
            assert_eq!(found.id, "vst3:halcyon_2", "{identifier}");
            assert_eq!(found.path, "/plugins/Halcyon2.vst3", "{identifier}");
        }
    }

    #[test]
    fn path_and_basename_default_to_the_instrument_class() {
        let path = "/plugins/Halcyon2.vst3";
        let instrument = halcyon(path);
        let mut effect = plugin("vst3:halcyon_2_fx", "Halcyon 2 FX", path);
        effect.category = PluginCategory::Effect;
        let plugins = vec![effect, instrument];
        for identifier in [path, "Halcyon2", "Halcyon2.vst3"] {
            let query = PluginQuery::parse(identifier).unwrap();
            assert_eq!(
                resolve_plugin(&plugins, &query, PluginFormat::Vst3)
                    .map(|plugin| plugin.id.as_str()),
                Some("vst3:halcyon_2"),
                "{identifier}"
            );
        }
    }

    #[test]
    fn exact_id_wins_over_a_normalized_competitor() {
        let plugins = vec![
            plugin("halcyon2", "Unrelated", "/plugins/Unrelated.vst3"),
            halcyon("/plugins/Halcyon2.vst3"),
        ];
        let query = PluginQuery::parse("halcyon2").unwrap();
        assert_eq!(
            resolve_plugin(&plugins, &query, PluginFormat::Vst3).map(|plugin| plugin.id.as_str()),
            Some("halcyon2")
        );
    }

    #[test]
    fn unrelated_identifiers_do_not_fuzzy_match() {
        let plugins = vec![
            halcyon("/plugins/Halcyon2.vst3"),
            plugin("vst3:surge_xt", "Surge XT", "/plugins/Surge XT.vst3"),
        ];
        for identifier in [
            "halcyon",
            "Halcyon 2 Extra",
            "halcyon_22",
            "com.acme.halcyon",
            "com.acme.halcyon2.VST3.extra",
            "Halcyon2.vst3.bak",
            "Surge",
        ] {
            let query = PluginQuery::parse(identifier).unwrap();
            assert!(
                resolve_plugin(&plugins, &query, PluginFormat::Vst3).is_none(),
                "{identifier} must not resolve"
            );
        }
    }

    #[test]
    fn normalized_variants_require_a_unique_match() {
        let plugins = vec![
            plugin("vst3:a", "Halcyon 2", "/a/Halcyon2.vst3"),
            plugin("vst3:b", "Halcyon 2", "/b/Halcyon2.vst3"),
        ];
        let query = PluginQuery::parse("halcyon_2").unwrap();
        assert!(resolve_plugin(&plugins, &query, PluginFormat::Vst3).is_none());
    }

    #[test]
    fn bundle_identifier_is_read_from_info_plist() {
        let root = temp_dir("bundle-id");
        let bundle = write_bundle(&root, "com.acme.halcyon2.VST3");
        assert_eq!(
            read_bundle_identifier(&bundle).as_deref(),
            Some("com.acme.halcyon2.VST3")
        );
        let path = bundle.to_string_lossy().into_owned();
        let mut effect = plugin("vst3:halcyon_2_fx", "Halcyon 2 FX", &path);
        effect.category = PluginCategory::Effect;
        // Effect first: descriptor order must not decide the default class.
        let plugins = vec![effect, halcyon(&path)];
        for identifier in [
            "com.acme.halcyon2.VST3",
            "COM.ACME.HALCYON2.vst3",
            "com.acme.halcyon2",
            path.as_str(),
            "Halcyon2",
            "Halcyon2.vst3",
            "Halcyon 2",
        ] {
            let query = PluginQuery::parse(identifier).unwrap();
            assert_eq!(
                resolve_plugin(&plugins, &query, PluginFormat::Vst3)
                    .map(|plugin| plugin.id.as_str()),
                Some("vst3:halcyon_2"),
                "{identifier}"
            );
        }
        let query = PluginQuery::parse("Halcyon 2 FX").unwrap();
        assert_eq!(
            resolve_plugin(&plugins, &query, PluginFormat::Vst3).map(|plugin| plugin.id.as_str()),
            Some("vst3:halcyon_2_fx")
        );
        for identifier in ["com.acme.halcyon", "com.acme.halcyon2.VST3.extra"] {
            let query = PluginQuery::parse(identifier).unwrap();
            assert!(
                resolve_plugin(&plugins, &query, PluginFormat::Vst3).is_none(),
                "{identifier} must not resolve"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_path_resolves_through_canonicalization() {
        let root = temp_dir("canonical");
        let bundle = root.join("Halcyon 2.vst3");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        let link = root.join("Linked.vst3");
        std::os::unix::fs::symlink(&bundle, &link).unwrap();
        let plugins = vec![halcyon(bundle.to_str().unwrap())];
        let query = PluginQuery::parse(link.to_str().unwrap()).unwrap();
        assert_eq!(
            resolve_plugin(&plugins, &query, PluginFormat::Vst3).map(|plugin| plugin.id.as_str()),
            Some("vst3:halcyon_2")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
