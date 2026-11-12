//! LV2 instantiate paths preserve Unicode and include the required separator.

use std::ffi::CString;
use std::path::Path;

#[derive(Debug)]
pub(super) enum BundlePathError {
    NonUnicode,
    InteriorNul,
    Resolve(std::io::Error),
}

impl std::fmt::Display for BundlePathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonUnicode => {
                formatter.write_str("LV2 bundle path cannot be represented as UTF-8")
            }
            Self::InteriorNul => formatter.write_str("LV2 bundle path contains a NUL"),
            Self::Resolve(error) => write!(
                formatter,
                "Cannot resolve an absolute LV2 bundle path: {error}"
            ),
        }
    }
}

pub(super) fn instantiate_bundle_path(path: &Path) -> Result<CString, BundlePathError> {
    let original = path.to_str().ok_or(BundlePathError::NonUnicode)?;
    if original.contains('\0') {
        return Err(BundlePathError::InteriorNul);
    }
    let absolute = std::path::absolute(path).map_err(BundlePathError::Resolve)?;
    let mut text = absolute
        .to_str()
        .ok_or(BundlePathError::NonUnicode)?
        .to_owned();
    if !text.ends_with(std::path::MAIN_SEPARATOR) {
        text.push(std::path::MAIN_SEPARATOR);
    }
    CString::new(text).map_err(|_| BundlePathError::InteriorNul)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lv2_bundle_path_absolute_unicode_and_separator_contract() {
        let relative = Path::new("voice-é-声-🎹.lv2");
        let expected = std::path::absolute(relative).unwrap();
        for path in [relative.to_path_buf(), expected.clone()] {
            let encoded = instantiate_bundle_path(&path).unwrap();
            let text = encoded.to_str().unwrap();
            assert!(Path::new(text).is_absolute());
            assert!(text.ends_with(std::path::MAIN_SEPARATOR));
            assert_eq!(Path::new(text), expected.as_path());
            assert!(text.contains("voice-é-声-🎹.lv2"));
            let repeated = instantiate_bundle_path(Path::new(text)).unwrap();
            assert_eq!(encoded, repeated);
        }
    }

    #[test]
    fn lv2_bundle_path_refuses_nul_instead_of_truncating() {
        assert!(matches!(
            instantiate_bundle_path(Path::new("voice\0hidden.lv2")),
            Err(BundlePathError::InteriorNul)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn lv2_bundle_path_refuses_non_utf8_instead_of_replacing_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = std::ffi::OsString::from_vec(vec![b'/', b'v', 0xff]);
        assert!(matches!(
            instantiate_bundle_path(Path::new(&path)),
            Err(BundlePathError::NonUnicode)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn lv2_bundle_path_refuses_unpaired_surrogate_instead_of_replacing_it() {
        use std::os::windows::ffi::OsStringExt;
        let path = std::ffi::OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
        assert!(matches!(
            instantiate_bundle_path(Path::new(&path)),
            Err(BundlePathError::NonUnicode)
        ));
    }
}
