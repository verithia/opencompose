#[cfg(target_os = "macos")]
mod macos {
    use oc_host::bundle::binary_path;
    use std::path::{Path, PathBuf};

    fn bundle(name: &str) -> PathBuf {
        let root = oc_test_support::temp_dir("host-bundle", name);
        let bundle = root.join("Example.vst3");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        bundle
    }

    fn manifest(bundle: &Path, executable: plist::Value, binary: bool) {
        let mut value = plist::Dictionary::new();
        value.insert("CFBundleExecutable".into(), executable);
        let value = plist::Value::Dictionary(value);
        let path = bundle.join("Contents/Info.plist");
        if binary {
            value.to_file_binary(path).unwrap();
        } else {
            value.to_file_xml(path).unwrap();
        }
    }

    #[test]
    fn declared_module_name_overrides_the_bundle_stem_for_xml_and_binary_manifests() {
        for binary in [false, true] {
            let bundle = bundle("declared-name");
            let target = bundle.join("Contents/MacOS/Module Hooks Ω");
            std::fs::write(&target, b"fixture").unwrap();
            std::fs::write(bundle.join("Contents/MacOS/Example"), b"other").unwrap();
            manifest(&bundle, "Module Hooks Ω".into(), binary);
            assert_eq!(binary_path(&bundle), Some(target));
        }
    }

    #[test]
    fn invalid_or_missing_declared_module_does_not_load_a_different_binary() {
        for name in [
            "",
            ".",
            "..",
            "../other",
            "/outside",
            "dir/module",
            "dir\\module",
            "absent",
        ] {
            let bundle = bundle("invalid-name");
            std::fs::write(bundle.join("Contents/MacOS/Example"), b"other").unwrap();
            manifest(&bundle, name.into(), false);
            assert_eq!(binary_path(&bundle), None, "{name:?}");
        }
    }

    #[test]
    fn malformed_oversized_and_non_string_manifests_fail_closed() {
        let bundle = bundle("invalid-manifest");
        std::fs::write(bundle.join("Contents/MacOS/Example"), b"other").unwrap();
        let path = bundle.join("Contents/Info.plist");
        for bytes in [b"not a plist".to_vec(), vec![b'x'; 256 * 1024 + 1]] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(binary_path(&bundle), None);
        }
        manifest(&bundle, plist::Value::Boolean(true), false);
        assert_eq!(binary_path(&bundle), None);
    }

    #[test]
    fn symlinked_executable_must_stay_inside_the_bundle() {
        let bundle = bundle("symlink");
        let outside = bundle.parent().unwrap().join("outside");
        std::fs::write(&outside, b"other").unwrap();
        std::os::unix::fs::symlink(&outside, bundle.join("Contents/MacOS/Module")).unwrap();
        manifest(&bundle, "Module".into(), false);
        assert_eq!(binary_path(&bundle), None);
    }

    #[test]
    fn undeclared_legacy_bundle_keeps_conventional_resolution() {
        let bundle = bundle("legacy");
        let target = bundle.join("Contents/MacOS/Example");
        std::fs::write(&target, b"fixture").unwrap();
        assert_eq!(binary_path(&bundle), Some(target));
    }
}
