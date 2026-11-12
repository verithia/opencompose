//! Production factory-owner validation using the controlled native COM factory.
use super::super::native_fixture::factory_audit::FactoryFixture;
use super::*;

fn owner(fixture: &FactoryFixture) -> Vst3ForeignRef<IPluginFactory> {
    let raw = fixture.acquire().unwrap();
    // SAFETY: acquire transfers exactly one retained native factory reference.
    let reference = unsafe { ComPtr::from_raw(raw) }.unwrap();
    Vst3ForeignRef::adopt(reference, Arc::new(Vst3Failure::default()))
}

#[derive(Debug)]
/// Coverage of VST3 factory owner checks.
pub struct FactoryOwnerAudit {
    pub metadata_cases: usize,
    pub creation_cases: usize,
    pub quarantined_partial_output: bool,
}

/// Audit the VST3 factory owner.
pub fn audit_factory_owner() -> Result<FactoryOwnerAudit, String> {
    let mut metadata_cases = 0;
    for count in [-1, MAX_FACTORY_CLASSES + 1, i32::MAX] {
        let fixture = FactoryFixture::new();
        let factory = owner(&fixture);
        fixture.configure(0, 0, count, false, false);
        assert!(classes(&factory, None).unwrap_err().contains("0..4096"));
        assert_eq!(fixture.calls(2), 0);
        assert_eq!(fixture.calls(3), 0);
        assert_eq!(
            factory.failure.first_fault().unwrap().operation,
            Operation::FactoryCount
        );
        drop(factory);
        assert_eq!(fixture.calls(10), 0);
        metadata_cases += 1;
    }
    let fixture = FactoryFixture::new();
    let factory = owner(&fixture);
    fixture.configure(0, 0, 0, false, false);
    assert!(classes(&factory, None)?.is_empty());
    assert!(audio_class(classes(&factory, None)?).is_err());
    fixture.configure(0, 0, MAX_FACTORY_CLASSES, false, false);
    assert_eq!(classes(&factory, None)?.len(), MAX_FACTORY_CLASSES as usize);
    metadata_cases += 1;
    fixture.configure(0, 0, 1, false, false);
    let factory2 = factory.try_query::<IPluginFactory2>().unwrap().unwrap();
    let plain = audio_class(classes(&factory, None)?)?;
    let extended = audio_class(classes(&factory, Some(&factory2))?)?;
    assert_eq!(plain.name, "Native factory");
    assert_eq!(plain.cid, [7; 16]);
    assert_eq!(extended.cid, plain.cid);
    assert_eq!(extended.category, plain.category);
    assert_eq!(extended.sub_categories, "Instrument");
    // Optional extended-metadata refusal falls back to the same bounded base info.
    fixture.configure(3, 4, 1, false, false);
    assert_eq!(
        audio_class(classes(&factory, Some(&factory2))?)?.name,
        plain.name
    );
    assert!(!factory.failure.failed());
    fixture.configure(2, 4, 1, false, false);
    assert!(classes(&factory, None)?.is_empty());
    assert!(!factory.failure.failed());
    metadata_cases += 4;
    drop(factory2);
    drop(factory);
    assert_eq!(fixture.refs(), 1);

    for extended in [false, true] {
        let fixture = FactoryFixture::new();
        let factory = owner(&fixture);
        let factory2 = if extended {
            Some(factory.try_query::<IPluginFactory2>().unwrap().unwrap())
        } else {
            None
        };
        fixture.configure(0, 0, 1, true, false);
        assert!(
            classes(&factory, factory2.as_ref())
                .unwrap_err()
                .contains("NUL-terminated")
        );
        assert!(factory.failure.failed());
        drop(factory2);
        drop(factory);
        assert_eq!(fixture.calls(10), 0);
        metadata_cases += 1;
    }
    for (point, extended) in [(1, false), (2, false), (3, true)] {
        let fixture = FactoryFixture::new();
        let factory = owner(&fixture);
        let factory2 = if extended {
            Some(factory.try_query::<IPluginFactory2>().unwrap().unwrap())
        } else {
            None
        };
        fixture.configure(point, 1, 1, false, false);
        assert!(classes(&factory, factory2.as_ref()).is_err());
        let first = factory.failure.first_fault().unwrap();
        assert!(classes(&factory, factory2.as_ref()).is_err());
        assert_eq!(
            fixture.calls(point),
            1,
            "failed factory calls must not be retried"
        );
        assert_eq!(factory.failure.first_fault(), Some(first));
        drop(factory2);
        drop(factory);
        assert_eq!(fixture.calls(10), 0);
        metadata_cases += 1;
    }

    let mut creation_cases = 0;
    let mut quarantined_partial_output = false;
    for (kind, null_output) in [
        (0, false),
        (0, true),
        (4, true),
        (4, false),
        (1, false),
        (2, false),
    ] {
        let fixture = FactoryFixture::new();
        let factory = owner(&fixture);
        fixture.set_create_iid(&IComponent::IID);
        fixture.configure(4, kind, 1, false, null_output);
        let created = create::<IComponent>(&factory, &[7; 16]);
        if kind == 0 && !null_output {
            let component = created?;
            assert_eq!(fixture.refs(), 3);
            drop(component);
            assert_eq!(fixture.refs(), 2);
            assert!(!factory.failure.failed());
        } else {
            assert!(created.is_err());
            if kind == 4 && null_output {
                assert!(
                    !factory.failure.failed(),
                    "ordinary refusal permits safe cleanup"
                );
            } else {
                assert!(factory.failure.failed());
                let first = factory.failure.first_fault().unwrap();
                assert!(create::<IComponent>(&factory, &[7; 16]).is_err());
                assert_eq!(factory.failure.first_fault(), Some(first));
                assert_eq!(fixture.calls(4), 1);
            }
        }
        let quarantined = factory.failure.failed();
        drop(factory);
        if quarantined {
            assert_eq!(fixture.calls(10), 0);
            if !null_output {
                assert_eq!(fixture.refs(), 3);
                quarantined_partial_output = true;
            }
        } else {
            assert_eq!(fixture.refs(), 1);
        }
        creation_cases += 1;
    }
    // Actual controller-class helper keeps optional refusal ordinary, but
    // quarantines native exceptions and preserves the initial cause.
    for kind in [0, 4, 1, 2] {
        let mut fixture = super::super::native_fixture::NativeFixture::new(false);
        fixture.set_fault(42, kind);
        let component = &fixture.instance().component;
        let discovered = controller_class_id(component);
        match kind {
            0 => assert_eq!(discovered?, Some([7; 16])),
            4 => assert_eq!(discovered?, None),
            _ => {
                assert!(discovered.is_err());
                let first = fixture.instance().failure().first_fault().unwrap();
                assert_eq!(first.operation, Operation::ControllerClassId);
                assert!(controller_class_id(&fixture.instance().component).is_err());
                assert_eq!(fixture.calls(42), 1);
            }
        }
        fixture.retire();
        if kind == 1 || kind == 2 {
            assert_eq!(fixture.calls(10), 0);
        }
        creation_cases += 1;
    }
    Ok(FactoryOwnerAudit {
        metadata_cases,
        creation_cases,
        quarantined_partial_output,
    })
}

#[cfg(test)]
#[test]
fn actual_factory_owner_bounds_metadata_creation_and_quarantine() {
    let audit = audit_factory_owner().unwrap();
    assert_eq!(audit.metadata_cases, 13);
    assert_eq!(audit.creation_cases, 10);
    assert!(audit.quarantined_partial_output);
}

#[cfg(test)]
#[test]
fn publishing_first_fault_refuses_factory_and_controller_native_admission() {
    let fixture = FactoryFixture::new();
    let factory = owner(&fixture);
    fixture.set_create_iid(&IComponent::IID);
    assert!(factory.failure.slot.begin_write_for_test());
    assert!(factory.failure.failed());
    assert!(factory.failure.message().is_none());
    assert!(classes(&factory, None).is_err());
    assert!(create::<IComponent>(&factory, &[7; 16]).is_err());
    assert_eq!(fixture.calls(1), 0);
    assert_eq!(fixture.calls(2), 0);
    assert_eq!(fixture.calls(4), 0);
    let first = Vst3ProcessError::native(
        Operation::FactoryClassInfo,
        Fault::host_contract("original publishing failure"),
    );
    factory.failure.slot.finish_write_for_test(first);
    assert_eq!(classes(&factory, None).unwrap_err(), first.to_string());
    assert!(create::<IComponent>(&factory, &[7; 16]).is_err());
    assert_eq!(factory.failure.first_fault(), Some(first));
    drop(factory);
    assert_eq!(fixture.calls(10), 0);

    let mut component_fixture = super::super::native_fixture::NativeFixture::new(false);
    let component = &component_fixture.instance().component;
    assert!(component.failure.slot.begin_write_for_test());
    assert!(controller_class_id(component).is_err());
    assert_eq!(component_fixture.calls(42), 0);
    component.failure.slot.finish_write_for_test(first);
    assert_eq!(
        controller_class_id(component).unwrap_err(),
        first.to_string()
    );
    component_fixture.retire();
    assert_eq!(component_fixture.calls(10), 0);
}
