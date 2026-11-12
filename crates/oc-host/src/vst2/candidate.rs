//! Mandatory owner for an admitted native allocation before registry publication.
use super::*;
use crate::application_main::{self, UiRetirementToken};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

pub(super) struct OwnedCandidate {
    native: ManuallyDrop<Box<Vst2Instance>>,
    live: bool,
    _not_send: PhantomData<Rc<()>>,
}
impl OwnedCandidate {
    pub(super) fn new(native: Box<Vst2Instance>) -> Self {
        Self {
            native: ManuallyDrop::new(native),
            live: true,
            _not_send: PhantomData,
        }
    }
    pub(super) fn into_legacy(mut self) -> Vst2Instance {
        // SAFETY: The sole compatibility constructor consumes this owner once;
        // only owned candidates enter the UI registry and its retirement path.
        let native = unsafe { ManuallyDrop::take(&mut self.native) };
        self.live = false;
        *native
    }
    pub(super) fn retire(&mut self, token: &UiRetirementToken<'_>) -> Result<(), Arc<Vst2Failure>> {
        self.native.retire_owned(token)
    }
    pub(super) fn quarantine(&mut self) {
        self.native.binding.quarantine();
    }
}
impl Deref for OwnedCandidate {
    type Target = Vst2Instance;
    fn deref(&self) -> &Self::Target {
        &self.native
    }
}
impl DerefMut for OwnedCandidate {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.native
    }
}
impl Drop for OwnedCandidate {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        // SAFETY: This owner consumes its unique Box once. No reference escapes
        // the drop path; uncertain ownership is retained as an entire allocation.
        let mut native = unsafe { ManuallyDrop::take(&mut self.native) };
        self.live = false;
        if native.module.is_none() {
            drop(native);
            return;
        }
        let healthy = application_main::ui_retirement_authority()
            .ok()
            .is_some_and(|token| native.retire_owned(&token).is_ok());
        if healthy {
            drop(native);
        } else {
            native.binding.quarantine();
            std::mem::forget(native);
        }
    }
}
