use super::*;

fn checked_status(
    instance: &Vst3Instance,
    operation: Operation,
    result: Result<tresult, Fault>,
) -> Result<(), Vst3OwnedError> {
    let code = result.map_err(|fault| {
        Vst3OwnedError::Native(Box::new(instance.native_fault(operation, fault)))
    })?;
    if code != kResultOk {
        return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
            operation, code,
        ))));
    }
    Ok(())
}

pub(super) fn activate(
    _token: &UiThreadToken<'_>,
    instance: &mut Vst3Instance,
) -> Result<(), Vst3OwnedError> {
    application_main::ui_authority()?;
    instance
        .healthy()
        .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?;
    if instance.active || instance.processing {
        return Err(Vst3OwnedError::InvalidRequest(
            "Owned VST3 activation requires an inactive prepared instance",
        ));
    }
    // SAFETY: the real UI owner retains this healthy, fully prepared component.
    checked_status(instance, Operation::SetActive, unsafe {
        vst3_guard::active(instance.component.as_ptr(), true)
    })?;
    instance.active = true;
    application_main::ui_authority()?;
    // SAFETY: component activation succeeded and this UI owner retains its processor.
    checked_status(instance, Operation::SetProcessing, unsafe {
        vst3_guard::processing(instance.processor.as_ptr(), true)
    })?;
    instance.processing = true;
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
/// Audit results for VST3 activation.
pub struct Vst3ActivationAudit {
    pub ordinary_refusals: usize,
    pub exceptions_quarantined: usize,
    pub successful_activation_and_retirement: bool,
}

/// Uses the real application bootstrap and optimized native fixture; no window
/// or audio device is created. Every failed activation remains unpublished.
#[cfg(any(test, feature = "test-support"))]
pub fn audit_checked_owned_activation() -> Result<Vst3ActivationAudit, Vst3OwnedError> {
    use super::super::native_fixture::NativeFixture;
    fn require(condition: bool, message: &'static str) -> Result<(), Vst3OwnedError> {
        condition
            .then_some(())
            .ok_or(Vst3OwnedError::InvalidRequest(message))
    }
    fn inactive_fixture(_token: &UiThreadToken<'_>) -> Result<NativeFixture, Vst3OwnedError> {
        application_main::ui_authority()?;
        let mut fixture = NativeFixture::new(false);
        let instance = fixture.instance_mut();
        // The fixture starts active. Complete the actual native transitions,
        // then publish each corresponding inactive flag, before testing load admission.
        // SAFETY: the UI token and fixture retain the healthy native processor.
        checked_status(instance, Operation::SetProcessing, unsafe {
            vst3_guard::processing(instance.processor.as_ptr(), false)
        })?;
        instance.processing = false;
        application_main::ui_authority()?;
        // SAFETY: processing stopped successfully and the same UI owns the component.
        checked_status(instance, Operation::SetActive, unsafe {
            vst3_guard::active(instance.component.as_ptr(), false)
        })?;
        instance.active = false;
        Ok(fixture)
    }

    let token = application_main::ui_authority()?;
    for point in [4, 3] {
        let mut fixture = inactive_fixture(&token)?;
        let before_active = fixture.calls(4);
        let before_processing = fixture.calls(3);
        let refusal = -700 - point as i32;
        fixture.set_refusal(refusal);
        fixture.set_fault(point, 4);
        let expected_operation = if point == 4 {
            Operation::SetActive
        } else {
            Operation::SetProcessing
        };
        let result = activate(&token, fixture.instance_mut());
        require(
            matches!(result, Err(Vst3OwnedError::Native(error))
            if error.operation == expected_operation && error.plugin_result == Some(refusal)),
            "Owned activation lost the ordinary native refusal",
        )?;
        require(
            !fixture.instance().processing && fixture.instance().active == (point == 3),
            "Owned activation published an uncompleted phase",
        )?;
        require(
            !fixture.instance().failed() && fixture.calls(1) == 0,
            "Ordinary activation refusal poisoned or processed the candidate",
        )?;
        fixture.retire();
        require(
            fixture.refs() == 0
                && fixture.calls(4) == before_active + if point == 3 { 2 } else { 1 }
                && fixture.calls(3) == before_processing + u32::from(point == 3),
            "Activation refusal cleanup repeated an uncompleted phase or leaked references",
        )?;
    }
    for (point, kind) in [(4, 1), (3, 2)] {
        let mut fixture = inactive_fixture(&token)?;
        fixture.set_fault(point, kind);
        let expected_operation = if point == 4 {
            Operation::SetActive
        } else {
            Operation::SetProcessing
        };
        let result = activate(&token, fixture.instance_mut());
        require(
            matches!(result, Err(Vst3OwnedError::Native(error))
            if error.operation == expected_operation && error.plugin_result.is_none()),
            "Owned activation lost the original native exception",
        )?;
        require(
            fixture.instance().failed() && fixture.calls(1) == 0,
            "Exceptional activation did not quarantine the unpublished candidate",
        )?;
        let counts: [u32; 64] = std::array::from_fn(|point| fixture.calls(point as u32));
        let refs = fixture.refs();
        fixture.retire();
        require(
            refs > 0
                && fixture.refs() == refs
                && std::array::from_fn::<_, 64, _>(|point| fixture.calls(point as u32)) == counts,
            "Exceptional activation reentered native cleanup",
        )?;
    }
    let mut fixture = inactive_fixture(&token)?;
    activate(&token, fixture.instance_mut())?;
    require(
        fixture.instance().active && fixture.instance().processing,
        "Successful owned activation did not publish both completed phases",
    )?;
    fixture.retire();
    require(
        fixture.refs() == 0 && fixture.calls(1) == 0,
        "Successful owned activation did not retire without processing",
    )?;
    Ok(Vst3ActivationAudit {
        ordinary_refusals: 2,
        exceptions_quarantined: 2,
        successful_activation_and_retirement: true,
    })
}
