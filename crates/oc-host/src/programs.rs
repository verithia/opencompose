//! Program (patch) inventory and selection, per plugin format.
//!
//! A program list is **native** only when the plugin exposes one through its
//! own API; the capability table below is the single source of truth for what a
//! caller may expect. Formats without a native list are still selectable by
//! state injection, but the caller supplies the state — the host does not
//! invent a program list it cannot read.
//!
//! The table is data, not prose: the agent surface serialises it verbatim so a
//! caller can tell a real inventory from an empty one before loading a plugin.

use oc_audio::PluginFormat;
use serde::{Deserialize, Serialize};

/// How a caller can discover a plugin's programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramEnumeration {
    /// The plugin exposes its own program list.
    Native,
    /// No list is exposed; the caller must bring its own program state.
    None,
}

/// How a chosen program can be written into a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramSelection {
    /// The plugin's own program data is captured and stored as state.
    NativeProgramData,
    /// The plugin accepts a MIDI program change.
    MidiProgramChange,
    /// The caller supplies a state blob and the host stores it.
    StateInjection,
    /// No selection path exists.
    None,
}

/// One format's measured capability. `note` explains a `None` or a fallback in
/// one line; it is shown next to the mechanism, never instead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FormatProgramCapability {
    pub format: PluginFormat,
    pub enumerate: ProgramEnumeration,
    pub select: &'static [ProgramSelection],
    pub note: &'static str,
}

/// No list and no selection: a built-in device is not a plugin program host.
const NO_SELECTION: &[ProgramSelection] = &[ProgramSelection::None];
/// Every format without a native program list can still store a supplied blob.
const STATE_ONLY: &[ProgramSelection] = &[ProgramSelection::StateInjection];

/// The per-format capability table. VST3 is the only format whose instance API
/// the host can both read and select today; the others are state-injection
/// only, and say so.
pub const PROGRAM_CAPABILITIES: &[FormatProgramCapability] = &[
    FormatProgramCapability {
        format: PluginFormat::Vst3,
        enumerate: ProgramEnumeration::Native,
        select: &[
            ProgramSelection::NativeProgramData,
            ProgramSelection::StateInjection,
        ],
        note: "IUnitInfo names the programs; IProgramListData supplies the chosen program's state",
    },
    FormatProgramCapability {
        format: PluginFormat::Clap,
        enumerate: ProgramEnumeration::None,
        select: STATE_ONLY,
        note: "CLAP exposes no instance program list; store a captured state blob",
    },
    FormatProgramCapability {
        format: PluginFormat::Vst2,
        enumerate: ProgramEnumeration::None,
        select: STATE_ONLY,
        note: "VST2 program enumeration is not wired; store a captured state blob",
    },
    FormatProgramCapability {
        format: PluginFormat::Au,
        enumerate: ProgramEnumeration::None,
        select: STATE_ONLY,
        note: "AU factory-preset enumeration is not wired; store an aupreset state blob",
    },
    FormatProgramCapability {
        format: PluginFormat::Lv2,
        enumerate: ProgramEnumeration::None,
        select: STATE_ONLY,
        note: "LV2 presets are bundles, not an instance list; store a captured state blob",
    },
    FormatProgramCapability {
        format: PluginFormat::Internal,
        enumerate: ProgramEnumeration::None,
        select: NO_SELECTION,
        note: "built-in devices have no plugin program list",
    },
];

/// Capability for one format; unknown formats get a conservative row.
pub fn capability(format: PluginFormat) -> FormatProgramCapability {
    PROGRAM_CAPABILITIES
        .iter()
        .copied()
        .find(|row| row.format == format)
        .unwrap_or(FormatProgramCapability {
            format,
            enumerate: ProgramEnumeration::None,
            select: STATE_ONLY,
            note: "no capability row for this format",
        })
}

/// Stable lowercase label used on the wire and in diagnostics.
pub fn format_label(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Clap => "clap",
        PluginFormat::Vst2 => "vst2",
        PluginFormat::Au => "au",
        PluginFormat::Lv2 => "lv2",
        PluginFormat::Internal => "internal",
    }
}

/// One program a plugin reported natively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeProgram {
    /// Zero-based position in the plugin's list.
    pub index: u32,
    /// Plugin-assigned list id, needed to fetch that program's data.
    pub list_id: i32,
    pub list_name: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hosted_format_has_a_row() {
        for format in [
            PluginFormat::Vst3,
            PluginFormat::Clap,
            PluginFormat::Vst2,
            PluginFormat::Au,
            PluginFormat::Lv2,
            PluginFormat::Internal,
        ] {
            let row = capability(format);
            assert_eq!(row.format, format);
            assert!(!row.note.is_empty(), "{format:?} has no note");
            assert!(
                !row.select.is_empty(),
                "{format:?} advertises no selection mechanism"
            );
        }
    }

    #[test]
    fn only_vst3_claims_native_enumeration() {
        let native: Vec<_> = PROGRAM_CAPABILITIES
            .iter()
            .filter(|row| row.enumerate == ProgramEnumeration::Native)
            .map(|row| row.format)
            .collect();
        assert_eq!(native, vec![PluginFormat::Vst3]);
    }

    #[test]
    fn state_injection_is_the_fallback_everywhere_else() {
        for row in PROGRAM_CAPABILITIES {
            if row.format == PluginFormat::Vst3 || row.format == PluginFormat::Internal {
                continue;
            }
            assert_eq!(row.enumerate, ProgramEnumeration::None, "{:?}", row.format);
            assert!(
                row.select.contains(&ProgramSelection::StateInjection),
                "{:?} lost its state fallback",
                row.format
            );
        }
    }

    #[test]
    fn labels_are_lowercase_wire_values() {
        assert_eq!(format_label(PluginFormat::Vst3), "vst3");
        assert_eq!(format_label(PluginFormat::Clap), "clap");
        assert_eq!(format_label(PluginFormat::Au), "au");
    }
}
