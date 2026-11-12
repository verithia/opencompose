use super::*;
use crate::preset_parameters::{AuthoredParameter, ParameterApplyError};

#[test]
fn au_authored_system_reverb_uses_native_percent_units_and_changes_offline_pcm() {
    let rate = 48_000;
    let mut instance = AuInstance::load("Apple: AUReverb2", rate, 128)
        .expect("macOS supplies the built-in AUReverb2 acceptance fixture");
    let mix = instance
        .get_params()
        .into_iter()
        .find(|parameter| parameter.id == 0)
        .expect("AUReverb2 exposes its documented dry/wet parameter");
    assert_eq!((mix.min, mix.max), (0.0, 100.0));
    let mut renders = Vec::new();
    for value in [100.0, 0.0] {
        {
            let mut owner = instance.authored_parameter_owner().unwrap();
            owner
                .apply_authored_parameters(&[AuthoredParameter {
                    name: &mix.name,
                    value,
                    domain: ParameterWriteDomain::NativePlain,
                }])
                .unwrap();
        }
        assert_eq!(instance.get_param_by_name(&mix.name), Some(value));
        // Allow the system processor's parameter ramp to settle. These samples
        // stay in memory; no output device or hardware playback is opened.
        let mut silence = [0.0; 256];
        for _ in 0..32 {
            silence.fill(0.0);
            instance
                .process_effect_into(&mut silence, rate, None)
                .unwrap();
        }
        let mut impulse = [0.0; 256];
        impulse[0] = 0.5;
        impulse[1] = 0.5;
        instance
            .process_effect_into(&mut impulse, rate, None)
            .unwrap();
        assert!(impulse.iter().all(|sample| sample.is_finite()));
        renders.push(impulse);
    }
    assert!(
        renders[0]
            .iter()
            .zip(&renders[1])
            .any(|(wet, dry)| (wet - dry).abs() > 0.1)
    );
}

#[derive(Clone)]
struct Parameter {
    id: u32,
    name: String,
    min: f32,
    max: f32,
    unit: u32,
    writable: bool,
}
impl Parameter {
    fn new(id: u32, name: &str) -> Self {
        Self {
            id,
            name: name.into(),
            min: 0.0,
            max: 1.0,
            unit: 0,
            writable: true,
        }
    }
}
struct Scope {
    id: u32,
    count: u32,
    parameters: Vec<Parameter>,
}
#[derive(Clone, Copy)]
enum Mutation {
    Range,
    Writable,
    Name,
    DuplicateId,
}
struct Unit {
    scopes: Vec<Scope>,
    writes: Vec<(u32, u32, u32, f32, u32)>,
    status: i32,
    mutation: Option<Mutation>,
    oversized: bool,
    partial: bool,
    metadata_error: i32,
    gets: usize,
    malformed_name: bool,
}
struct Fixture {
    native: Box<Unit>,
    error: Option<AuProcessError>,
}
impl Fixture {
    fn new(parameters: Vec<Parameter>) -> Self {
        Self {
            native: Box::new(Unit {
                scopes: vec![Scope {
                    id: 0,
                    count: 1,
                    parameters,
                }],
                writes: Vec::new(),
                status: 0,
                mutation: None,
                oversized: false,
                partial: false,
                metadata_error: 0,
                gets: 0,
                malformed_name: false,
            }),
            error: None,
        }
    }
    fn owner(&mut self) -> Result<AuAuthoredParameterOwner<'_>, ParameterErrorKind> {
        let unit = (&mut *self.native as *mut Unit).cast();
        // SAFETY: the fixture's separately owned Unit remains alive and exclusively
        // borrowed for the owner lifetime; every thunk below uses that ABI context.
        unsafe {
            AuAuthoredParameterOwner::prepare(
                unit,
                &mut self.error,
                ParameterDispatch {
                    property_info,
                    property,
                    set,
                },
                Arc::new(au_guard::Boundary::default()),
            )
        }
    }
}
unsafe extern "C" fn property_info(
    unit: *mut c_void,
    property: u32,
    scope: u32,
    element: u32,
    size: *mut u32,
    writable: *mut u8,
) -> i32 {
    // SAFETY: matching fixture dispatch retains a live exclusive Unit context.
    let unit = unsafe { &mut *unit.cast::<Unit>() };
    assert_eq!(property, PROP_PARAMETER_LIST);
    assert_eq!(element, 0);
    let Some(scope) = unit.scopes.iter().find(|entry| entry.id == scope) else {
        return INVALID_SCOPE;
    };
    // SAFETY: the production reader supplies writable scalar output pointers.
    unsafe {
        *size = if unit.oversized {
            (MAX_PARAMETERS as u32 + 1) * 4
        } else {
            scope.parameters.len() as u32 * 4
        };
        *writable = 0;
    }
    0
}
unsafe extern "C" fn property(
    unit: *mut c_void,
    property: u32,
    scope: u32,
    element: u32,
    data: *mut c_void,
    size: *mut u32,
) -> i32 {
    // SAFETY: matching fixture dispatch retains a live exclusive Unit context.
    let unit = unsafe { &mut *unit.cast::<Unit>() };
    unit.gets += 1;
    let Some(scope) = unit.scopes.iter().find(|entry| entry.id == scope) else {
        return INVALID_SCOPE;
    };
    match property {
        PROP_PARAMETER_LIST => {
            assert_eq!(element, 0);
            for (index, parameter) in scope.parameters.iter().enumerate() {
                // SAFETY: production sized this bounded array through property_info.
                unsafe {
                    *data.cast::<u32>().add(index) = parameter.id;
                }
            }
            if unit.partial {
                // SAFETY: the reader supplies one live size scalar.
                unsafe {
                    *size = (*size).saturating_sub(4);
                }
            }
        }
        PROP_ELEMENT_COUNT => {
            assert_ne!(scope.id, SCOPE_NOTE, "a note ID is not an indexed element");
            assert_eq!(element, 0);
            // SAFETY: the production query supplies a u32 output record.
            unsafe {
                *data.cast::<u32>() = scope.count;
            }
        }
        PROP_PARAMETER_INFO => {
            if unit.metadata_error != 0 {
                return unit.metadata_error;
            }
            // The element argument here is an ID, not the target element/bus.
            let parameter = scope
                .parameters
                .iter()
                .find(|entry| entry.id == element)
                .unwrap();
            // SAFETY: this C record consists only of numbers/nullable pointers.
            let mut info: AudioUnitParameterInfo = unsafe { std::mem::zeroed() };
            for (output, byte) in info.name.iter_mut().zip(parameter.name.as_bytes()) {
                *output = *byte as c_char;
            }
            if unit.malformed_name {
                info.name[0] = 255_u8 as c_char;
            }
            info.min_value = parameter.min;
            info.max_value = parameter.max;
            info.default_value = parameter.min;
            info.unit = parameter.unit;
            info.flags = if parameter.writable {
                PARAM_FLAG_IS_WRITABLE
            } else {
                0
            };
            // SAFETY: the production query supplies the exact C record capacity.
            unsafe {
                *data.cast::<AudioUnitParameterInfo>() = info;
            }
        }
        _ => panic!("unexpected AU property"),
    }
    0
}
unsafe extern "C" fn set(
    unit: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    value: f32,
    offset: u32,
) -> i32 {
    // SAFETY: matching fixture dispatch retains a live exclusive Unit context.
    let unit = unsafe { &mut *unit.cast::<Unit>() };
    unit.writes.push((id, scope, element, value, offset));
    if unit.writes.len() == 1
        && let Some(mutation) = unit.mutation
    {
        let parameter = &mut unit.scopes[0].parameters[0];
        match mutation {
            Mutation::Range => parameter.max = 0.75,
            Mutation::Writable => parameter.writable = false,
            Mutation::Name => parameter.name = "Renamed native control".into(),
            Mutation::DuplicateId => {
                let duplicate = parameter.clone();
                unit.scopes[0].parameters.push(duplicate);
            }
        }
    }
    unit.status
}
fn assignment(name: &str, value: f64) -> AuthoredParameter<'_> {
    AuthoredParameter {
        name,
        value,
        domain: ParameterWriteDomain::NativePlain,
    }
}
fn native_failure(error: ParameterApplyError) -> AuProcessError {
    let ParameterErrorKind::Native { source, .. } = error.kind else {
        panic!("missing native failure");
    };
    *source.downcast_ref::<AuProcessError>().unwrap()
}

#[test]
fn au_authored_native_units_last_native_target_wins_and_plain_f32_rounding_is_allowed() {
    let mut parameter = Parameter::new(19, "Wet / Dry");
    parameter.max = 100.0;
    parameter.unit = 3;
    let mut fixture = Fixture::new(vec![parameter]);
    let mut owner = fixture.owner().unwrap();
    assert_eq!(owner.inventory[0].unit, 3);
    owner
        .apply_authored_parameters(&[assignment("Wet / Dry", 500.0), assignment("wetdry", 27.1)])
        .unwrap();
    drop(owner);
    assert_eq!(fixture.native.writes, vec![(19, 0, 0, 27.1_f32, 0)]);
}

#[test]
fn au_authored_static_scopes_and_dynamic_note_metadata_do_not_disable_global_controls() {
    let mut fixture = Fixture::new(vec![Parameter::new(31, "Global gain")]);
    fixture.native.scopes.push(Scope {
        id: SCOPE_INPUT,
        count: 2,
        parameters: vec![Parameter::new(77, "Input amount")],
    });
    fixture.native.scopes.push(Scope {
        id: SCOPE_OUTPUT,
        count: 1,
        parameters: vec![Parameter::new(88, "Output amount")],
    });
    fixture.native.scopes.push(Scope {
        id: SCOPE_NOTE,
        count: 128,
        parameters: vec![Parameter::new(99, "Note tuning")],
    });
    let mut owner = fixture.owner().unwrap();
    assert_eq!(owner.inventory.len(), 5);
    assert!(matches!(
        owner.resolve_authored_parameter("Input amount"),
        Err(ParameterErrorKind::Ambiguous)
    ));
    owner
        .apply_authored_parameters(&[
            assignment("Global gain", 0.5),
            assignment("Output amount", 0.25),
        ])
        .unwrap();
    drop(owner);
    assert_eq!(
        fixture.native.writes,
        vec![(31, 0, 0, 0.5, 0), (88, 2, 0, 0.25, 0)]
    );
    fixture.native.writes.clear();
    let mut owner = fixture.owner().unwrap();
    let error = owner
        .apply_authored_parameters(&[
            assignment("Global gain", 0.5),
            assignment("Note tuning", 0.25),
        ])
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::UnsupportedDomain));
    drop(owner);
    assert!(fixture.native.writes.is_empty());
}

#[test]
fn au_authored_alias_collisions_and_unicode_names_keep_distinct_native_targets() {
    let mut fixture = Fixture::new(vec![
        Parameter::new(1, "A B"),
        Parameter::new(2, "AB"),
        Parameter::new(3, "低频"),
        Parameter::new(4, "高频"),
    ]);
    let mut owner = fixture.owner().unwrap();
    for name in ["A B", "AB", "a-b"] {
        assert!(matches!(
            owner.resolve_authored_parameter(name),
            Err(ParameterErrorKind::Ambiguous)
        ));
    }
    assert!(matches!(
        owner.resolve_authored_parameter("!!!"),
        Err(ParameterErrorKind::Unknown)
    ));
    owner
        .apply_authored_parameters(&[assignment("低频", 0.25), assignment("高频", 0.75)])
        .unwrap();
    drop(owner);
    assert_eq!(
        fixture.native.writes,
        vec![(3, 0, 0, 0.25, 0), (4, 0, 0, 0.75, 0)]
    );
}

#[test]
fn au_authored_complete_batch_preflight_refuses_invalid_values_and_read_only_without_writes() {
    for invalid in [
        assignment("missing", 0.5),
        assignment("Read only", 0.5),
        assignment("Gain", f64::NAN),
        assignment("Gain", 2.0),
        AuthoredParameter {
            name: "Gain",
            value: 0.5,
            domain: ParameterWriteDomain::Normalized,
        },
    ] {
        let mut read_only = Parameter::new(2, "Read only");
        read_only.writable = false;
        let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain"), read_only]);
        let mut owner = fixture.owner().unwrap();
        assert!(
            owner
                .apply_authored_parameters(&[assignment("Gain", 0.25), invalid])
                .is_err()
        );
        drop(owner);
        assert!(fixture.native.writes.is_empty());
    }
    let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain")]);
    fixture
        .owner()
        .unwrap()
        .apply_authored_parameters(&[assignment("Gain", f64::from(f32::from_bits(1)))])
        .unwrap();
    assert_eq!(fixture.native.writes[0].3.to_bits(), 1);
}

#[test]
fn au_authored_post_set_metadata_changes_stop_before_a_second_native_write() {
    for mutation in [Mutation::Range, Mutation::Writable, Mutation::Name] {
        let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain"), Parameter::new(2, "Mix")]);
        fixture.native.mutation = Some(mutation);
        let error = fixture
            .owner()
            .unwrap()
            .apply_authored_parameters(&[assignment("Gain", 0.25), assignment("Mix", 0.5)])
            .unwrap_err();
        assert!(matches!(error.kind, ParameterErrorKind::InventoryChanged));
        assert_eq!(fixture.native.writes.len(), 1);
    }
}

#[test]
fn au_authored_native_setter_and_metadata_status_are_preserved_without_success_fallback() {
    let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain"), Parameter::new(2, "Mix")]);
    fixture.native.status = -10863;
    let error = fixture
        .owner()
        .unwrap()
        .apply_authored_parameters(&[assignment("Gain", 0.25), assignment("Mix", 0.5)])
        .unwrap_err();
    assert_eq!(
        native_failure(error),
        AuProcessError::Native {
            operation: "AudioUnitSetParameter",
            status: -10863
        }
    );
    assert_eq!(fixture.native.writes.len(), 1);
    assert_eq!(
        fixture.error,
        Some(AuProcessError::Native {
            operation: "AudioUnitSetParameter",
            status: -10863
        })
    );
    let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain")]);
    fixture.native.metadata_error = -10877;
    let error = match fixture.owner() {
        Err(error) => error,
        Ok(_) => panic!("invalid native element was hidden"),
    };
    let ParameterErrorKind::Native { source, .. } = error else {
        panic!("missing native source");
    };
    assert_eq!(
        source.downcast_ref::<AuProcessError>(),
        Some(&AuProcessError::Native {
            operation: "AudioUnitGetProperty",
            status: -10877
        })
    );
}

#[test]
fn au_authored_discovery_sizes_and_preflight_verification_budget_are_bounded_before_writes() {
    for (oversized, partial) in [(true, false), (false, true)] {
        let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain")]);
        fixture.native.oversized = oversized;
        fixture.native.partial = partial;
        assert!(matches!(
            fixture.owner(),
            Err(ParameterErrorKind::InvalidMetadata)
        ));
        assert!(fixture.native.writes.is_empty());
        if oversized {
            assert_eq!(fixture.native.gets, 0);
        }
    }
    let parameters: Vec<_> = (0..500)
        .map(|id| Parameter::new(id, &format!("Parameter {id}")))
        .collect();
    let names: Vec<_> = parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    let assignments: Vec<_> = names.iter().map(|name| assignment(name, 0.5)).collect();
    let mut fixture = Fixture::new(parameters);
    let error = fixture
        .owner()
        .unwrap()
        .apply_authored_parameters(&assignments)
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::InvalidMetadata));
    assert!(fixture.native.writes.is_empty());
}

#[test]
fn au_authored_keys_cannot_cross_preparation_owners_and_nan_metadata_is_not_admitted() {
    let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain")]);
    let key = fixture
        .owner()
        .unwrap()
        .resolve_authored_parameter("Gain")
        .unwrap()
        .key;
    let mut other = Fixture::new(vec![Parameter::new(1, "Gain")]);
    let mut owner = other.owner().unwrap();
    assert!(matches!(
        owner.prepare_bound_value(&key, 0.5, ParameterWriteDomain::NativePlain),
        Err(ParameterErrorKind::InventoryChanged)
    ));
    drop(owner);
    assert!(other.native.writes.is_empty());
    fixture.native.scopes[0].parameters[0].max = f32::NAN;
    assert!(matches!(
        fixture.owner(),
        Err(ParameterErrorKind::InvalidMetadata)
    ));
}

#[test]
fn au_authored_duplicate_native_ids_are_refused_initially_and_after_first_write() {
    let duplicate = Parameter::new(1, "Gain");
    let mut initial = Fixture::new(vec![duplicate.clone(), duplicate]);
    assert!(matches!(
        initial.owner(),
        Err(ParameterErrorKind::InvalidMetadata)
    ));
    assert!(initial.native.writes.is_empty());
    let mut changed = Fixture::new(vec![Parameter::new(1, "Gain"), Parameter::new(2, "Mix")]);
    changed.native.mutation = Some(Mutation::DuplicateId);
    let error = changed
        .owner()
        .unwrap()
        .apply_authored_parameters(&[assignment("Gain", 0.25), assignment("Mix", 0.5)])
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::InvalidMetadata));
    assert_eq!(changed.native.writes, vec![(1, 0, 0, 0.25, 0)]);
}

#[test]
fn au_authored_invalid_native_utf8_is_refused_instead_of_lossily_aliasing_names() {
    let mut fixture = Fixture::new(vec![Parameter::new(1, "Gain")]);
    fixture.native.malformed_name = true;
    assert!(matches!(
        fixture.owner(),
        Err(ParameterErrorKind::InvalidMetadata)
    ));
    assert!(fixture.native.writes.is_empty());
}
