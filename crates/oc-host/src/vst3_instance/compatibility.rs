//! Resolve an FXB processor identity using the VST3 compatibility contract.
use super::{Vst3ClassInfo, Vst3Failure, Vst3ForeignRef, Vst3Instance, Vst3StateStream, factory};
use crate::plugin_state_io::read_regular_state_file;
use crate::vst3_guard::{self, Operation, Vst3ProcessError};
use serde::Deserialize;
use std::io::ErrorKind;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use vst3::ComWrapper;
use vst3::Steinberg::{IBStream, IPluginCompatibility, IPluginFactory, IPluginFactory2, kResultOk};

const MAX_DECLARATION_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const COMPATIBILITY_CLASS: &str = "Plugin Compatibility Class";

#[derive(Deserialize)]
struct Replacement {
    #[serde(rename = "New")]
    target: String,
    #[serde(rename = "Old")]
    sources: Vec<String>,
}

#[derive(Deserialize)]
struct ModuleInfo {
    #[serde(rename = "Compatibility", default)]
    replacements: Vec<Replacement>,
}

pub(super) fn resolve_legacy_class(
    bundle: &Path,
    source: [u8; 16],
) -> Result<Vst3ClassInfo, String> {
    let failure = Arc::new(Vst3Failure::default());
    let (factory, _) = Vst3Instance::load_factory(bundle, &failure)?;
    let factory2 = factory
        .try_query::<IPluginFactory2>()
        .map_err(|error| error.to_string())?;
    let classes = factory::classes(&factory, factory2.as_ref())?;
    if let Some(exact) = factory::audio_class_by_uid(&classes, source)? {
        return Ok(exact);
    }
    let replacements = match module_info(bundle)? {
        Some(manifest) => manifest.replacements,
        None => factory_replacements(&factory, &classes)?,
    };
    declared_class(&classes, &replacements, source)?
        .ok_or_else(|| "the VST3 bundle has no processor with the bank's legacy class identity or a declared replacement".into())
}

fn declared_class(
    classes: &[Vst3ClassInfo],
    replacements: &[Replacement],
    source: [u8; 16],
) -> Result<Option<Vst3ClassInfo>, String> {
    let mut target = None;
    for row in replacements {
        let declared = parse_uid(&row.target)?;
        for prior in &row.sources {
            if parse_uid(prior)? == source {
                if target.is_some_and(|target| target != declared) {
                    return Err(
                        "the VST3 bundle declares multiple replacements for the legacy class"
                            .into(),
                    );
                }
                target = Some(declared);
            }
        }
    }
    match target {
        Some(id) => factory::audio_class_by_uid(classes, id)?
            .map(Some)
            .ok_or_else(|| "the declared VST3 replacement is absent from the audio factory".into()),
        None => Ok(None),
    }
}

fn parse_uid(text: &str) -> Result<[u8; 16], String> {
    let raw = text.as_bytes();
    if raw.len() != 32 || !raw.iter().all(u8::is_ascii_hexdigit) {
        return Err("invalid class ID in VST3 compatibility declaration".into());
    }
    let mut id = [0; 16];
    for (byte, pair) in id.iter_mut().zip(raw.chunks_exact(2)) {
        let hi = (pair[0] as char).to_digit(16).unwrap();
        let lo = (pair[1] as char).to_digit(16).unwrap();
        *byte = (hi * 16 + lo) as u8;
    }
    Ok(id)
}

fn module_info(bundle: &Path) -> Result<Option<ModuleInfo>, String> {
    if bundle.is_file() {
        return Ok(None);
    }
    for entry in [
        "Contents/Resources/moduleinfo.json",
        "Contents/moduleinfo.json",
    ] {
        let path = bundle.join(entry);
        match path.symlink_metadata() {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot examine VST3 module info: {error}")),
            Ok(_) => {
                let data = read_regular_state_file(&path, MAX_DECLARATION_BYTES)?;
                return parse_json(&data).map(Some);
            }
        }
    }
    Ok(None)
}

fn factory_replacements(
    factory: &Vst3ForeignRef<IPluginFactory>,
    classes: &[Vst3ClassInfo],
) -> Result<Vec<Replacement>, String> {
    let mut compatibility = classes
        .iter()
        .filter(|class| class.category == COMPATIBILITY_CLASS);
    let Some(class) = compatibility.next() else {
        return Ok(Vec::new());
    };
    if compatibility.next().is_some() {
        return Err("the VST3 bundle has multiple compatibility classes".into());
    }
    let reader = factory::create::<IPluginCompatibility>(factory, &class.cid)?;
    let output = Vst3StateStream::with_write_limit(Vec::new(), MAX_DECLARATION_BYTES);
    let data = Rc::clone(&output.data);
    let overflowed = Rc::clone(&output.overflowed);
    let stream = ComWrapper::new(output)
        .to_com_ptr::<IBStream>()
        .ok_or("cannot create VST3 compatibility stream")?;
    // SAFETY: both guarded native interface and bounded host stream are retained
    // on the import worker's main thread for the complete synchronous call.
    let status = unsafe { vst3_guard::compatibility_json(reader.as_ptr(), stream.as_ptr()) }
        .map_err(|fault| {
            reader
                .fault(Operation::CompatibilityJson, fault)
                .to_string()
        })?;
    drop(reader);
    if let Some(error) = factory.failure.first_fault() {
        return Err(error.to_string());
    }
    if overflowed.get() {
        return Err("VST3 compatibility stream exceeded its byte limit".into());
    }
    if status != kResultOk {
        return Err(Vst3ProcessError::refused(Operation::CompatibilityJson, status).to_string());
    }
    parse_json(&data.borrow())
}

fn parse_json<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, String> {
    let text = std::str::from_utf8(data).map_err(|_| "VST3 compatibility JSON is not UTF-8")?;
    check_depth(text)?;
    json5::from_str(text).map_err(|error| format!("invalid VST3 compatibility JSON: {error}"))
}

fn check_depth(text: &str) -> Result<(), String> {
    // Bound recursion before the deserializer visits even unused manifest fields.
    let mut chars = text.chars().peekable();
    let mut depth = 0usize;
    while let Some(ch) = chars.next() {
        match ch {
            '[' | '{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err("VST3 compatibility JSON exceeds its nesting limit".into());
                }
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            '\'' | '"' => {
                while let Some(quoted) = chars.next() {
                    if quoted == ch {
                        break;
                    }
                    if quoted == '\\' {
                        chars.next();
                    }
                }
            }
            '/' if chars.next_if_eq(&'/').is_some() => {
                for comment in chars.by_ref() {
                    if matches!(comment, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
                        break;
                    }
                }
            }
            '/' if chars.next_if_eq(&'*').is_some() => {
                while let Some(comment) = chars.next() {
                    if comment == '*' && chars.next_if_eq(&'/').is_some() {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::factory::AUDIO_CLASS;
    use super::*;

    const SOURCE: &str = "565354546573746c6561640000000000";
    const TARGET: &str = "4480b0a0ac2140c4854af3a5ea9e264a";
    const OTHER: &str = "813b6c71bbf148bd8d09566ee1cb114b";

    fn class(uid: &str, category: &str) -> Vst3ClassInfo {
        let mut bytes = parse_uid(uid).unwrap();
        if cfg!(windows) {
            bytes[..4].reverse();
            bytes[4..6].reverse();
            bytes[6..8].reverse();
        }
        Vst3ClassInfo {
            cid: bytes.map(|byte| byte as i8),
            name: uid.to_owned(),
            category: category.to_owned(),
            sub_categories: String::new(),
            vendor: String::new(),
            version: String::new(),
        }
    }

    #[test]
    fn declared_uid_requires_an_unambiguous_audio_class_in_the_selected_bundle() {
        let manifest: ModuleInfo = parse_json(format!(
            "{{ // comments and trailing commas are valid\n Compatibility: [{{New:'{TARGET}',Old:['{}',],}},], }}",
            SOURCE.to_ascii_uppercase()
        ).as_bytes()).unwrap();
        let source = parse_uid(SOURCE).unwrap();
        let audio = class(TARGET, AUDIO_CLASS);
        let controller = class(TARGET, "Component Controller Class");
        let selected = declared_class(
            &[audio.clone(), controller.clone()],
            &manifest.replacements,
            source,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.cid, audio.cid);
        assert!(declared_class(&[controller], &manifest.replacements, source).is_err());
        assert!(
            declared_class(
                &[audio.clone(), audio.clone()],
                &manifest.replacements,
                source
            )
            .is_err()
        );
        assert!(
            declared_class(
                std::slice::from_ref(&audio),
                &manifest.replacements,
                parse_uid(OTHER).unwrap()
            )
            .unwrap()
            .is_none()
        );
        let mut conflicting = manifest.replacements;
        conflicting.push(Replacement {
            target: OTHER.into(),
            sources: vec![SOURCE.into()],
        });
        assert!(declared_class(&[audio], &conflicting, source).is_err());
        assert!(parse_uid("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").is_err());
    }

    #[test]
    fn module_info_precedence_and_bounded_json_nesting() {
        let temp = tempfile::tempdir().unwrap();
        let contents = temp.path().join("Contents");
        let resources = contents.join("Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(contents.join("moduleinfo.json"), "{Compatibility: []}").unwrap();
        assert!(module_info(temp.path()).unwrap().is_some());
        let modern = resources.join("moduleinfo.json");
        std::fs::write(&modern, "malformed").unwrap();
        assert!(module_info(temp.path()).is_err());
        let brackets = "[".repeat(MAX_JSON_DEPTH + 1);
        let allowed = format!("{{Name:'{brackets}', /* {brackets} */ Compatibility: []}}");
        assert!(parse_json::<ModuleInfo>(allowed.as_bytes()).is_ok());
        let too_deep = format!("{{Extra:{brackets}0{}}}", "]".repeat(MAX_JSON_DEPTH + 1));
        assert!(parse_json::<ModuleInfo>(too_deep.as_bytes()).is_err());
    }

    #[test]
    fn compatibility_query_contains_native_exceptions_and_bounds_stream_writes() {
        unsafe extern "C" {
            fn oc_vst3_compatibility_fixture(mode: u32) -> *mut IPluginCompatibility;
        }
        let query = |mode, limit| {
            let output = Vst3StateStream::with_write_limit(Vec::new(), limit);
            let data = Rc::clone(&output.data);
            let overflowed = Rc::clone(&output.overflowed);
            let stream = ComWrapper::new(output).to_com_ptr::<IBStream>().unwrap();
            // SAFETY: the borrowed thread-local fixture and retained COM stream
            // both remain live during the synchronous call.
            let result = unsafe {
                vst3_guard::compatibility_json(oc_vst3_compatibility_fixture(mode), stream.as_ptr())
            };
            (result, data.take(), overflowed.get())
        };
        let (success, bytes, overflowed) = query(0, 2);
        assert_eq!(success, Ok(kResultOk));
        assert_eq!(bytes, b"[]");
        assert!(!overflowed);
        let (ignored_refusal, bytes, overflowed) = query(0, 1);
        assert_eq!(ignored_refusal, Ok(kResultOk));
        assert!(bytes.is_empty() && overflowed);
        let (fault, bytes, overflowed) = query(1, 2);
        assert_eq!(
            fault.unwrap_err().kind,
            crate::native_fault::FaultKind::CppException
        );
        assert!(bytes.is_empty() && !overflowed);
    }
}
