//! Bounded factory metadata and checked instance creation share one failure owner.
use super::*;

pub(super) const MAX_FACTORY_CLASSES: i32 = 4096;

fn healthy(failure: &Vst3Failure) -> Result<(), String> {
    if failure.failed() {
        Err(failure
            .message()
            .unwrap_or_else(|| "VST3 factory is quarantined".into()))
    } else {
        Ok(())
    }
}
fn text<const N: usize>(value: &[vst3::Steinberg::char8; N]) -> Result<String, &'static str> {
    let end = value
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("factory class text is not NUL-terminated")?;
    let bytes: Vec<u8> = value[..end].iter().map(|byte| *byte as u8).collect();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn classes(
    factory: &Vst3ForeignRef<IPluginFactory>,
    factory2: Option<&Vst3ForeignRef<IPluginFactory2>>,
) -> Result<Vec<Vst3ClassInfo>, String> {
    healthy(&factory.failure)?;
    // SAFETY: the retained factory stays live across the guarded virtual call.
    let count = unsafe { vst3_guard::factory_count(factory.as_ptr()) }
        .map_err(|fault| factory.fault(Operation::FactoryCount, fault).to_string())?;
    healthy(&factory.failure)?;
    if !(0..=MAX_FACTORY_CLASSES).contains(&count) {
        return Err(factory
            .fault(
                Operation::FactoryCount,
                Fault::host_contract("factory class count exceeds the supported 0..4096 range"),
            )
            .to_string());
    }
    let mut result = Vec::new();
    for index in 0..count {
        healthy(&factory.failure)?;
        if let Some(factory2) = factory2 {
            let mut info = std::mem::MaybeUninit::<vst3::Steinberg::PClassInfo2>::zeroed();
            // SAFETY: retained factory2 and the fixed-sized output buffer are live.
            let code = unsafe {
                vst3_guard::factory_class_info2(factory2.as_ptr(), index, info.as_mut_ptr())
            }
            .map_err(|fault| {
                factory2
                    .fault(Operation::FactoryClassInfo2, fault)
                    .to_string()
            })?;
            healthy(&factory.failure)?;
            if code == kResultOk {
                // SAFETY: the native wrapper initializes the complete fixed-size output.
                let info = unsafe { info.assume_init() };
                let read = || -> Result<Vst3ClassInfo, &'static str> {
                    text(&info.sdkVersion)?;
                    Ok(Vst3ClassInfo {
                        name: text(&info.name)?,
                        category: text(&info.category)?,
                        sub_categories: text(&info.subCategories)?,
                        vendor: text(&info.vendor)?,
                        version: text(&info.version)?,
                        cid: info.cid,
                    })
                };
                result.push(read().map_err(|fault| {
                    factory2
                        .fault(Operation::FactoryClassInfo2, Fault::host_contract(fault))
                        .to_string()
                })?);
                continue;
            }
        }
        let mut info = std::mem::MaybeUninit::<vst3::Steinberg::PClassInfo>::zeroed();
        // SAFETY: retained factory and the fixed-sized output buffer are live.
        let code =
            unsafe { vst3_guard::factory_class_info(factory.as_ptr(), index, info.as_mut_ptr()) }
                .map_err(|fault| {
                factory
                    .fault(Operation::FactoryClassInfo, fault)
                    .to_string()
            })?;
        healthy(&factory.failure)?;
        if code != kResultOk {
            continue;
        }
        // SAFETY: the native wrapper initializes the complete fixed-size output.
        let info = unsafe { info.assume_init() };
        let read = || -> Result<Vst3ClassInfo, &'static str> {
            Ok(Vst3ClassInfo {
                name: text(&info.name)?,
                category: text(&info.category)?,
                sub_categories: String::new(),
                vendor: String::new(),
                version: String::new(),
                cid: info.cid,
            })
        };
        result.push(read().map_err(|fault| {
            factory
                .fault(Operation::FactoryClassInfo, Fault::host_contract(fault))
                .to_string()
        })?);
    }
    Ok(result)
}

pub(super) fn audio_class(classes: Vec<Vst3ClassInfo>) -> Result<Vst3ClassInfo, String> {
    classes
        .into_iter()
        .find(|class| class.category == "Audio Module Class")
        .ok_or_else(|| "No Audio Module Class in VST3 bundle".into())
}

pub(super) fn create<I: Interface>(
    factory: &Vst3ForeignRef<IPluginFactory>,
    cid: &vst3::Steinberg::TUID,
) -> Result<Vst3ForeignRef<I>, String> {
    healthy(&factory.failure)?;
    let mut output = ptr::null_mut();
    // SAFETY: factory, class/interface IDs and exact output slot remain live.
    let code = unsafe {
        vst3_guard::factory_create(
            factory.as_ptr(),
            cid.as_ptr(),
            I::IID.as_ptr().cast(),
            &mut output,
        )
    }
    .map_err(|fault| factory.fault(Operation::FactoryCreate, fault).to_string())?;
    // A callback can fault the shared owner even when the native call succeeds;
    // the returned native reference is retained rather than released on that path.
    healthy(&factory.failure)?;
    if code != kResultOk {
        if !output.is_null() {
            return Err(factory
                .fault(
                    Operation::FactoryCreate,
                    Fault::host_contract("createInstance refused with a non-null output"),
                )
                .to_string());
        }
        return Err(Vst3ProcessError::refused(Operation::FactoryCreate, code).to_string());
    }
    if output.is_null() {
        return Err(factory
            .fault(
                Operation::FactoryCreate,
                Fault::host_contract("createInstance succeeded with a null output"),
            )
            .to_string());
    }
    // SAFETY: successful creation transferred exactly one non-null requested interface ref.
    let reference = unsafe { ComPtr::from_raw(output.cast::<I>()) }
        .ok_or_else(|| "Null VST3 instance".to_string())?;
    Ok(Vst3ForeignRef::adopt(reference, Arc::clone(&factory.failure)).with_parent(factory))
}

pub(super) fn controller_class_id(
    component: &Vst3ForeignRef<IComponent>,
) -> Result<Option<vst3::Steinberg::TUID>, String> {
    healthy(&component.failure)?;
    let mut cid = [0; 16];
    // SAFETY: retained component and exact 16-byte class-ID output remain live.
    let code = unsafe { vst3_guard::controller_class_id(component.as_ptr(), &mut cid) }.map_err(
        |fault| {
            component
                .fault(Operation::ControllerClassId, fault)
                .to_string()
        },
    )?;
    healthy(&component.failure)?;
    Ok((code == kResultOk).then_some(cid))
}

#[cfg(any(test, feature = "test-support"))]
#[path = "factory/tests.rs"]
pub(super) mod tests;
