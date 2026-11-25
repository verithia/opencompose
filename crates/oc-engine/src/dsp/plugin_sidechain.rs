//! Authored input selections are checked before they reach a native setter.

use oc_host::PluginFormat;

pub(super) const PARAMETER: &str = "sidechain_input";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Selection(usize);

impl Selection {
    pub(super) fn parse(format: PluginFormat, value: f64) -> Result<Self, String> {
        if !matches!(format, PluginFormat::Vst3 | PluginFormat::Clap) {
            return Err(format!(
                "{PARAMETER} selection is not supported by this host format"
            ));
        }
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= usize::MAX as f64 {
            return Err(format!(
                "{PARAMETER} requires a nonnegative integer input index"
            ));
        }
        Ok(Self(value as usize))
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(super) fn index(self) -> usize {
        self.0
    }

    pub(super) fn apply(self, count: usize, select: impl FnOnce(usize)) -> Result<(), String> {
        if self.0 >= count {
            return Err(format!(
                "{PARAMETER}={} is unavailable; the plugin declares {count} sidechain inputs",
                self.0
            ));
        }
        select(self.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidechain_selection_refuses_invalid_values_and_unsupported_formats() {
        for format in [PluginFormat::Vst3, PluginFormat::Clap] {
            for value in [-1.0, -0.5, 0.5, f64::NAN, f64::INFINITY, usize::MAX as f64] {
                assert!(
                    Selection::parse(format, value).is_err(),
                    "{format:?}: {value}"
                );
            }
            for value in [0.0, 1.0, 7.0] {
                let selection = Selection::parse(format, value).unwrap();
                let mut selected = None;
                selection.apply(8, |index| selected = Some(index)).unwrap();
                assert_eq!(selected, Some(value as usize));
            }
        }
        for format in [PluginFormat::Au, PluginFormat::Vst2, PluginFormat::Lv2] {
            assert!(Selection::parse(format, 0.0).is_err());
        }
    }

    #[test]
    fn sidechain_selection_refuses_out_of_range_without_calling_native_setter() {
        for (index, count) in [(0, 0), (1, 1), (7, 2)] {
            let selection = Selection::parse(PluginFormat::Vst3, index as f64).unwrap();
            assert!(
                selection
                    .apply(count, |_| panic!("invalid selection reached native setter"))
                    .is_err()
            );
        }
    }
}
