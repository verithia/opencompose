use super::super::native_fixture::factory_audit::FactoryFixture;
use super::*;

#[test]
fn actual_module_factory_admission_transfers_one_reference_and_releases_when_healthy() {
    let fixture = FactoryFixture::new();
    fixture.configure(0, 0, 1, false, false);
    let failure = Arc::new(Vst3Failure::default());
    let before = fixture.refs();
    let factory = acquire_factory(fixture.symbol(7), &failure).unwrap();
    assert_eq!(fixture.refs(), before + 1);
    assert!(!failure.failed());
    drop(factory);
    assert_eq!(fixture.refs(), before);
}

#[test]
fn actual_module_factory_exception_retains_existing_reference_and_original_failure() {
    for kind in 1..=3 {
        let fixture = FactoryFixture::new();
        fixture.configure(0, 0, 1, false, false);
        let failure = Arc::new(Vst3Failure::default());
        let existing = acquire_factory(fixture.symbol(7), &failure).unwrap();
        let before = fixture.refs();
        fixture.configure(7, kind, 1, false, false);
        assert!(acquire_factory(fixture.symbol(7), &failure).is_err());
        let original = failure.first_fault().unwrap();
        assert_eq!(original.operation, Operation::GetPluginFactory);
        assert_eq!(fixture.refs(), before);
        let calls = fixture.calls(7);
        assert!(acquire_factory(fixture.symbol(7), &failure).is_err());
        assert_eq!(fixture.calls(7), calls);
        assert_eq!(failure.first_fault(), Some(original));
        drop(existing);
        assert_eq!(fixture.refs(), before);
        assert_eq!(fixture.calls(10), 0);
    }
}

#[test]
fn actual_module_factory_null_is_an_ordinary_unpublished_refusal() {
    let fixture = FactoryFixture::new();
    fixture.configure(7, 0, 1, false, true);
    let failure = Arc::new(Vst3Failure::default());
    let before = fixture.refs();
    assert!(acquire_factory(fixture.symbol(7), &failure).is_err());
    assert!(!failure.failed());
    assert_eq!(fixture.refs(), before);
}

#[test]
fn actual_module_entry_refusal_and_exception_use_the_platform_abi() {
    #[cfg(target_os = "macos")]
    let (point, operation) = (9, Operation::BundleEntry);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (point, operation) = (5, Operation::ModuleEntry);
    #[cfg(windows)]
    let (point, operation) = (8, Operation::ModuleInit);
    let fixture = FactoryFixture::new();
    let failure = Arc::new(Vst3Failure::default());
    fixture.configure(point, 4, 1, false, false);
    assert!(!enter_symbol(fixture.symbol(point), fixture.raw().cast(), &failure).unwrap());
    assert!(!failure.failed());
    for kind in 1..=3 {
        let failure = Arc::new(Vst3Failure::default());
        fixture.configure(point, kind, 1, false, false);
        assert!(enter_symbol(fixture.symbol(point), fixture.raw().cast(), &failure).is_err());
        assert_eq!(failure.first_fault().unwrap().operation, operation);
        let calls = fixture.calls(point);
        assert!(enter_symbol(fixture.symbol(point), fixture.raw().cast(), &failure).is_err());
        assert_eq!(fixture.calls(point), calls);
    }
}

#[test]
fn actual_module_exit_refusal_and_exception_keep_cleanup_quarantined() {
    let fixture = FactoryFixture::new();
    for kind in 1..=4 {
        let failure = Arc::new(Vst3Failure::default());
        fixture.configure(6, kind, 1, false, false);
        assert!(exit_symbol(fixture.symbol(6), &failure).is_err());
        assert!(failure.failed());
        assert_eq!(
            failure.first_fault().unwrap().operation,
            Operation::ModuleExit
        );
        let calls = fixture.calls(6);
        assert!(exit_symbol(fixture.symbol(6), &failure).is_err());
        assert_eq!(fixture.calls(6), calls);
    }
}

#[test]
fn platform_module_symbol_names_match_their_native_contracts() {
    #[cfg(target_os = "macos")]
    assert_eq!((ENTRY_NAME, EXIT_NAME), (c"bundleEntry", c"bundleExit"));
    #[cfg(all(unix, not(target_os = "macos")))]
    assert_eq!((ENTRY_NAME, EXIT_NAME), (c"ModuleEntry", c"ModuleExit"));
    #[cfg(windows)]
    assert_eq!((ENTRY_NAME, EXIT_NAME), (c"InitDll", c"ExitDll"));
}

#[cfg(unix)]
#[test]
fn module_paths_preserve_non_utf8_os_bytes_and_refuse_nul() {
    use std::os::unix::ffi::OsStrExt;
    let bytes = b"/plugin-\xff.vst3";
    assert_eq!(
        path_cstring(Path::new(std::ffi::OsStr::from_bytes(bytes)))
            .unwrap()
            .as_bytes(),
        bytes
    );
    assert!(path_cstring(Path::new(std::ffi::OsStr::from_bytes(b"plugin\0.vst3"))).is_err());
}
