//! LV2 bundle discovery and `manifest.ttl` metadata.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::abi::*;
use super::turtle::{self, Graph, Term};
use crate::{PluginCategory, PluginFormat, PluginInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// LV2 port data kind.
pub enum PortKind {
    Audio,
    Control,
    Atom,
    Cv,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Whether a port is input or output.
pub enum PortDirection {
    Input,
    Output,
}

#[derive(Clone, Debug)]
/// One declared LV2 port.
pub struct Port {
    pub index: u32,
    pub symbol: String,
    pub name: String,
    pub kind: PortKind,
    pub direction: PortDirection,
    pub minimum: f32,
    pub maximum: f32,
    pub default: f32,
    pub designation: Option<String>,
    pub supports_midi: bool,
    pub atom_buffer_type: Option<String>,
    /// Unit symbol from `units:unit`, when the bundle names one.
    pub unit: Option<String>,
    /// Port group label from `pg:group`, when the bundle assigns one.
    pub group: Option<String>,
    /// Step count for a toggled, integer or enumerated control.
    pub steps: Option<u32>,
}

impl Port {
    /// Whether the port carries audio.
    pub fn is_audio(&self) -> bool {
        self.kind == PortKind::Audio
    }

    /// Whether the port carries a control value.
    pub fn is_control(&self) -> bool {
        self.kind == PortKind::Control
    }

    /// Whether the port carries an atom sequence.
    pub fn is_atom(&self) -> bool {
        self.kind == PortKind::Atom
    }

    /// Whether the port is an input.
    pub fn is_input(&self) -> bool {
        self.direction == PortDirection::Input
    }
}

#[derive(Clone, Debug)]
/// A plugin discovered from an LV2 bundle.
pub struct Plugin {
    pub uri: String,
    pub name: String,
    pub vendor: String,
    pub license: Option<String>,
    pub project: Option<String>,
    pub bundle_path: PathBuf,
    pub binary_path: PathBuf,
    pub required_features: Vec<String>,
    pub optional_features: Vec<String>,
    pub ports: Vec<Port>,
    pub is_instrument: bool,
    pub has_state_interface: bool,
}

impl Plugin {
    /// Number of audio input ports.
    pub fn audio_inputs(&self) -> usize {
        self.ports
            .iter()
            .filter(|port| port.is_audio() && port.is_input())
            .count()
    }

    /// Number of audio output ports.
    pub fn audio_outputs(&self) -> usize {
        self.ports
            .iter()
            .filter(|port| port.is_audio() && !port.is_input())
            .count()
    }

    /// The MIDI atom input port, when declared.
    pub fn midi_input_port(&self) -> Option<&Port> {
        self.ports
            .iter()
            .find(|port| port.is_atom() && port.is_input() && port.supports_midi)
    }

    /// The MIDI atom output port, when declared.
    pub fn midi_output_port(&self) -> Option<&Port> {
        self.ports
            .iter()
            .find(|port| port.is_atom() && !port.is_input() && port.supports_midi)
    }

    /// Metadata in the host's plugin-info shape.
    pub fn info(&self) -> PluginInfo {
        PluginInfo {
            id: self.uri.clone(),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            format: PluginFormat::Lv2,
            path: self.bundle_path.to_string_lossy().into_owned(),
            category: if self.is_instrument {
                PluginCategory::Instrument
            } else {
                PluginCategory::Effect
            },
            num_params: self.ports.iter().filter(|port| port.is_control()).count() as u32,
            version: None,
            architectures: vec![std::env::consts::ARCH.to_string()],
            state_abi: Some(format!(
                "lv2:{}:{}",
                self.uri,
                if self.has_state_interface {
                    "state-v1"
                } else {
                    "stateless"
                }
            )),
        }
    }
}

/// Standard LV2 directories, plus `LV2_PATH` entries in front.
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !path.as_os_str().is_empty() && !paths.contains(&path) {
            paths.push(path);
        }
    };
    if let Some(env) = std::env::var_os("LV2_PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for entry in env.to_string_lossy().split(separator) {
            if !entry.is_empty() {
                push(PathBuf::from(entry));
            }
        }
    }
    for path in super::extra_search_paths() {
        push(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        {
            push(home.join("Library/Audio/Plug-Ins/LV2"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            push(home.join(".lv2"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        push(PathBuf::from("/Library/Audio/Plug-Ins/LV2"));
        push(PathBuf::from("/usr/local/lib/lv2"));
        push(PathBuf::from("/usr/lib/lv2"));
    }
    #[cfg(target_os = "linux")]
    {
        push(PathBuf::from("/usr/lib/lv2"));
        push(PathBuf::from("/usr/local/lib/lv2"));
        if let Some(arch) = linux_arch() {
            push(PathBuf::from(format!("/usr/lib/{arch}/lv2")));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            push(PathBuf::from(common).join("LV2"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            push(PathBuf::from(local).join("LV2"));
        }
    }
    paths
}

#[cfg(target_os = "linux")]
fn linux_arch() -> Option<String> {
    let output = std::process::Command::new("uname")
        .arg("-m")
        .output()
        .ok()?;
    let arch = String::from_utf8(output.stdout).ok()?;
    let arch = arch.trim();
    (!arch.is_empty()).then(|| arch.to_string())
}

/// Bundle directories under the given roots. A bundle is a directory that
/// directly contains `manifest.ttl`.
pub fn discover_bundles_in(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |bundle: PathBuf| {
        let key = std::fs::canonicalize(&bundle).unwrap_or_else(|_| bundle.clone());
        if seen.insert(key) {
            bundles.push(bundle);
        }
    };
    for root in paths {
        if root.join("manifest.ttl").is_file() {
            push(root.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && path.join("manifest.ttl").is_file())
            .collect();
        children.sort();
        for bundle in children {
            push(bundle);
        }
    }
    bundles
}

/// Bundles under the standard LV2 search paths.
pub fn discover_bundles() -> Vec<PathBuf> {
    discover_bundles_in(&search_paths())
}

/// Parse every plugin declared by one bundle's `manifest.ttl`.
pub fn scan_bundle(bundle: &Path) -> Result<Vec<Plugin>, String> {
    let manifest = bundle.join("manifest.ttl");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("cannot read {}: {error}", manifest.display()))?;
    let base = turtle::path_to_file_uri(&manifest);
    let graph = turtle::parse(&text, Some(&base))
        .map_err(|error| format!("malformed LV2 manifest {}: {error}", manifest.display()))?;
    let mut plugins = Vec::new();
    for subject in graph.subjects_of_type(LV2__PLUGIN) {
        let plugin = parse_plugin(bundle, &manifest, subject, &graph)?;
        plugins.push(plugin);
    }
    Ok(plugins)
}

fn parse_plugin(
    bundle: &Path,
    manifest: &Path,
    subject: &Term,
    graph: &Graph,
) -> Result<Plugin, String> {
    let Some(uri) = subject.as_iri() else {
        return Err(format!(
            "manifest {}: lv2:Plugin subject is not an IRI",
            manifest.display()
        ));
    };
    let binary = graph
        .object(subject, LV2__BINARY)
        .map(term_value)
        .ok_or_else(|| {
            format!(
                "manifest {}: plugin {uri} has no lv2:binary",
                manifest.display()
            )
        })?;
    let binary_path = if binary.starts_with("file:") {
        turtle::file_uri_to_path(&binary)
            .ok_or_else(|| format!("plugin {uri}: invalid binary IRI '{binary}'"))?
    } else if Path::new(&binary).is_absolute() {
        PathBuf::from(binary)
    } else {
        bundle.join(binary)
    };

    let mut ports = Vec::new();
    for port_term in graph.objects(subject, LV2__PORT) {
        ports.push(parse_port(uri, port_term, graph)?);
    }
    ports.sort_by_key(|port| port.index);

    let name = graph
        .objects(subject, RDFS__LABEL)
        .chain(graph.objects(subject, DOAP__NAME))
        .find_map(Term::as_str)
        .map(str::to_string)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| fallback_name(uri));
    let vendor = graph
        .objects(subject, DOAP__MAINTAINER)
        .chain(graph.objects(subject, DOAP__DEVELOPER))
        .find_map(|term| entity_name(term, graph))
        .unwrap_or_default();
    let license = graph.objects(subject, DOAP__LICENSE).next().map(term_value);
    let project = graph.objects(subject, DOAP__PROJECT).next().map(term_value);

    let mut required_features = feature_list(subject, graph, LV2__REQUIRED_FEATURE);
    let optional_features = feature_list(subject, graph, LV2__OPTIONAL_FEATURE);
    required_features.sort();
    required_features.dedup();

    let has_midi_input = ports
        .iter()
        .any(|port| port.is_atom() && port.is_input() && port.supports_midi);
    let audio_inputs = ports
        .iter()
        .filter(|port| port.is_audio() && port.is_input())
        .count();
    let audio_outputs = ports
        .iter()
        .filter(|port| port.is_audio() && !port.is_input())
        .count();
    let is_instrument = graph.has_type(subject, LV2__INSTRUMENT_PLUGIN)
        || has_midi_input
        || (audio_inputs == 0 && audio_outputs > 0);
    let has_state_interface = graph
        .objects(subject, LV2__EXTENSION_DATA)
        .any(|term| term.as_iri() == Some(STATE__INTERFACE));

    Ok(Plugin {
        uri: uri.to_string(),
        name,
        vendor,
        license,
        project,
        bundle_path: bundle.to_path_buf(),
        binary_path,
        required_features,
        optional_features,
        ports,
        is_instrument,
        has_state_interface,
    })
}

fn parse_port(plugin_uri: &str, term: &Term, graph: &Graph) -> Result<Port, String> {
    let index = graph
        .object(term, LV2__INDEX)
        .and_then(Term::as_i64)
        .ok_or_else(|| format!("plugin {plugin_uri}: port without lv2:index"))?;
    let index = u32::try_from(index)
        .map_err(|_| format!("plugin {plugin_uri}: port index {index} is out of range"))?;
    let symbol = graph
        .object(term, LV2__SYMBOL)
        .and_then(Term::as_str)
        .unwrap_or_default()
        .to_string();
    let name = graph
        .objects(term, LV2__NAME)
        .find_map(Term::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| symbol.clone());
    let direction = if graph.has_type(term, LV2__INPUT_PORT) {
        PortDirection::Input
    } else if graph.has_type(term, LV2__OUTPUT_PORT) {
        PortDirection::Output
    } else {
        return Err(format!(
            "plugin {plugin_uri}: port {index} is neither lv2:InputPort nor lv2:OutputPort"
        ));
    };
    let kind = if graph.has_type(term, LV2__AUDIO_PORT) {
        PortKind::Audio
    } else if graph.has_type(term, LV2__CONTROL_PORT) {
        PortKind::Control
    } else if graph.has_type(term, LV2__ATOM_PORT) {
        PortKind::Atom
    } else if graph.has_type(term, LV2__CV_PORT) {
        PortKind::Cv
    } else {
        PortKind::Other
    };
    let minimum = graph
        .object(term, LV2__MINIMUM)
        .and_then(Term::as_f64)
        .map_or(0.0, |value| value as f32);
    let maximum = graph
        .object(term, LV2__MAXIMUM)
        .and_then(Term::as_f64)
        .map_or(1.0, |value| value as f32);
    let default = graph
        .object(term, LV2__DEFAULT)
        .and_then(Term::as_f64)
        .map_or(0.0, |value| value as f32);
    let designation = graph
        .object(term, LV2__DESIGNATION)
        .and_then(Term::as_iri)
        .map(str::to_string);
    let supports: Vec<&str> = graph
        .objects(term, ATOM__SUPPORTS)
        .filter_map(Term::as_iri)
        .collect();
    let supports_midi = supports
        .iter()
        .any(|uri| *uri == MIDI__MIDI_EVENT || *uri == ATOM__SEQUENCE);
    let atom_buffer_type = graph
        .object(term, ATOM__BUFFER_TYPE)
        .and_then(Term::as_iri)
        .map(str::to_string);
    let unit = graph.object(term, UNITS__UNIT).and_then(|unit| {
        graph
            .object(unit, UNITS__SYMBOL)
            .or_else(|| graph.object(unit, UNITS__NAME))
            .and_then(Term::as_str)
            .map(str::to_string)
            .or_else(|| {
                // A bundle usually imports the standard units instead of
                // defining them, so the symbol is not in this graph.
                unit.as_iri()
                    .and_then(standard_unit_symbol)
                    .map(str::to_string)
            })
    });
    let group = graph
        .object(term, PG__GROUP)
        .and_then(|group| {
            graph
                .object(group, RDFS__LABEL)
                .or_else(|| graph.object(group, LV2__NAME))
        })
        .and_then(Term::as_str)
        .map(str::to_string);
    let steps = port_steps(term, graph, minimum, maximum);
    Ok(Port {
        index,
        symbol,
        name,
        kind,
        direction,
        minimum,
        maximum,
        default,
        designation,
        supports_midi,
        atom_buffer_type,
        unit,
        group,
        steps,
    })
}

/// Symbol for a standard `units:` unit whose definition lives outside the bundle.
fn standard_unit_symbol(iri: &str) -> Option<&'static str> {
    let name = iri.rsplit(['#', '/']).next().unwrap_or(iri);
    Some(match name {
        "db" => "dB",
        "hz" => "Hz",
        "khz" => "kHz",
        "mhz" => "MHz",
        "pc" | "percent" => "%",
        "s" => "s",
        "ms" => "ms",
        "min" => "min",
        "beats" => "beats",
        "bpm" => "BPM",
        "cent" | "cents" => "ct",
        "semitones" => "st",
        "octaves" => "oct",
        "frame" | "frames" => "frames",
        "coef" | "_" => "",
        _ => return None,
    })
}

/// Step count for a control the bundle describes as discrete; `None` for a
/// continuous one. An enumerated port counts its scale points instead.
fn port_steps(term: &Term, graph: &Graph, minimum: f32, maximum: f32) -> Option<u32> {
    let properties: Vec<&str> = graph
        .objects(term, LV2__PORT_PROPERTY)
        .filter_map(Term::as_iri)
        .collect();
    if properties.contains(&LV2__TOGGLED) {
        return Some(2);
    }
    if properties.contains(&LV2__ENUMERATION) {
        let points = graph.objects(term, LV2__SCALE_POINT).count();
        return (points > 1).then_some(points as u32);
    }
    if properties.contains(&LV2__INTEGER) {
        return crate::params::integer_steps(f64::from(minimum), f64::from(maximum));
    }
    None
}

fn feature_list(subject: &Term, graph: &Graph, predicate: &str) -> Vec<String> {
    graph
        .objects(subject, predicate)
        .filter_map(Term::as_iri)
        .map(str::to_string)
        .collect()
}

fn term_value(term: &Term) -> String {
    term.as_str().unwrap_or_default().to_string()
}

fn entity_name(term: &Term, graph: &Graph) -> Option<String> {
    if let Term::Literal { value, .. } = term {
        return Some(value.clone());
    }
    graph
        .objects(term, FOAF__NAME)
        .chain(graph.objects(term, RDFS__LABEL))
        .find_map(Term::as_str)
        .map(str::to_string)
        .or_else(|| term.as_iri().map(fallback_name))
}

fn fallback_name(uri: &str) -> String {
    uri.rsplit(['/', '#'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(uri)
        .to_string()
}

/// Scan every bundle on the LV2 search path.
pub fn scan_installed() -> (Vec<Plugin>, Vec<String>) {
    scan_bundles_on(&search_paths())
}

/// Scan bundles under `paths`, returning plugins and parse errors.
pub fn scan_bundles_on(paths: &[PathBuf]) -> (Vec<Plugin>, Vec<String>) {
    let mut plugins = Vec::new();
    let mut errors = Vec::new();
    for bundle in discover_bundles_in(paths) {
        match scan_bundle(&bundle) {
            Ok(mut found) => plugins.append(&mut found),
            Err(error) => errors.push(error),
        }
    }
    (plugins, errors)
}

/// Find one plugin URI on the search path without scanning every bundle twice.
pub fn find_installed(uri: &str) -> Option<Plugin> {
    find_in_paths(uri, &search_paths())
}

/// Find a plugin URI under `paths`.
pub fn find_in_paths(uri: &str, paths: &[PathBuf]) -> Option<Plugin> {
    for bundle in discover_bundles_in(paths) {
        let Ok(plugins) = scan_bundle(&bundle) else {
            continue;
        };
        if let Some(plugin) = plugins.into_iter().find(|plugin| plugin.uri == uri) {
            return Some(plugin);
        }
    }
    None
}

/// Bundle path for an installed plugin URI.
pub fn bundle_for_uri(uri: &str) -> Option<PathBuf> {
    find_installed(uri).map(|plugin| plugin.bundle_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bundle(name: &str, manifest: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ocp-lv2-meta-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("manifest.ttl"), manifest).unwrap();
        root
    }

    #[test]
    fn scans_a_bundle_with_two_plugins() {
        let bundle = write_bundle(
            "two",
            r#"
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            @prefix doap: <http://usefulinc.com/ns/doap#> .
            @prefix atom: <http://lv2plug.in/ns/ext/atom#> .
            @prefix midi: <http://lv2plug.in/ns/ext/midi#> .
            <urn:gain> a lv2:Plugin ;
                lv2:binary <libfixture.dylib> ;
                doap:name "Fixture Gain" ;
                lv2:port [
                    a lv2:AudioPort, lv2:InputPort ;
                    lv2:index 0 ; lv2:symbol "in_l"
                ], [
                    a lv2:AudioPort, lv2:OutputPort ;
                    lv2:index 1 ; lv2:symbol "out_l"
                ] .
            <urn:tone> a lv2:Plugin, lv2:InstrumentPlugin ;
                lv2:binary <libfixture.dylib> ;
                doap:name "Fixture Tone" ;
                lv2:requiredFeature <http://example.org/unsupported> ;
                lv2:port [
                    a lv2:AudioPort, lv2:OutputPort ;
                    lv2:index 2 ; lv2:symbol "out_l"
                ], [
                    a lv2:AtomPort, lv2:InputPort ;
                    lv2:index 3 ; lv2:symbol "midi_in" ;
                    atom:bufferType atom:Sequence ;
                    atom:supports midi:MidiEvent
                ] .
            "#,
        );
        let plugins = scan_bundle(&bundle).unwrap();
        assert_eq!(plugins.len(), 2);
        let gain = plugins
            .iter()
            .find(|plugin| plugin.uri == "urn:gain")
            .unwrap();
        assert_eq!(gain.name, "Fixture Gain");
        assert_eq!(gain.audio_inputs(), 1);
        assert_eq!(gain.audio_outputs(), 1);
        assert_eq!(gain.binary_path, bundle.join("libfixture.dylib"));
        assert!(!gain.is_instrument);
        let tone = plugins
            .iter()
            .find(|plugin| plugin.uri == "urn:tone")
            .unwrap();
        assert!(tone.is_instrument);
        assert_eq!(tone.required_features, ["http://example.org/unsupported"]);
        assert!(tone.midi_input_port().is_some());
        let _ = std::fs::remove_dir_all(bundle);
    }

    #[test]
    fn malformed_manifest_is_an_error() {
        let bundle = write_bundle("broken", "<urn:p> a lv2:Plugin");
        let error = scan_bundle(&bundle).unwrap_err();
        assert!(error.contains("malformed LV2 manifest"), "{error}");
        let _ = std::fs::remove_dir_all(bundle);
    }

    #[test]
    fn discovery_requires_manifest_ttl() {
        let root = std::env::temp_dir().join(format!("ocp-lv2-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("good.lv2")).unwrap();
        std::fs::write(root.join("good.lv2/manifest.ttl"), "@prefix lv2: <x> .").unwrap();
        std::fs::create_dir_all(root.join("not-a-bundle")).unwrap();
        let bundles = discover_bundles_in(std::slice::from_ref(&root));
        assert_eq!(bundles, [root.join("good.lv2")]);
        let _ = std::fs::remove_dir_all(root);
    }
}
