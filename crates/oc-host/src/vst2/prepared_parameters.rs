//! Cached parameter identities retain the instance's existing setter authority.

use super::{Vst2Instance, Vst2ProcessError};
use oc_audio::{PreparedParameterError, PreparedParameterHandle, PreparedParameterOutcome};

impl Vst2Instance {
    pub(super) fn invalidate_prepared_parameters(&mut self) {
        if let Some(owner) = &mut self.prepared_parameters {
            owner.invalidate();
        }
    }

    /// Name resolution and recovery belong to graph preparation, not processing.
    pub(super) fn prepare_parameter_binding(
        &mut self,
        name: &str,
    ) -> Option<PreparedParameterHandle> {
        self.check_callback_route().ok()?;
        let index = self.param_index(name)?;
        if !(0..self.num_params).contains(&index) {
            return None;
        }
        let handle = self.prepared_parameters.as_ref()?.bind(index as u32)?;
        if matches!(
            self.process_error,
            Some(Vst2ProcessError::PreparedParameter(_))
        ) {
            self.process_error = None;
            self.error_reported.set(false);
        }
        Some(handle)
    }

    pub(super) fn write_prepared_parameter(
        &mut self,
        handle: PreparedParameterHandle,
        value: f64,
    ) -> PreparedParameterOutcome {
        if self.process_error.is_some() {
            return PreparedParameterOutcome::Failed;
        }
        let index = self
            .prepared_parameters
            .as_ref()
            .map_or(Err(PreparedParameterError::Invalidated), |owner| {
                owner.resolve(handle)
            });
        let index = match index {
            Ok(index) if index < self.num_params.max(0) as u32 => index as i32,
            result => {
                let error = result.err().unwrap_or(PreparedParameterError::OutOfBounds);
                self.note_process_error(Vst2ProcessError::PreparedParameter(error));
                return PreparedParameterOutcome::Invalidated(error);
            }
        };
        if !value.is_finite() {
            self.note_process_error(Vst2ProcessError::PreparedParameter(
                PreparedParameterError::NonFinite,
            ));
            return PreparedParameterOutcome::Failed;
        }
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return PreparedParameterOutcome::Failed;
        }
        self.set_param_index(index, value);
        if let Err(error) = self.check_callback_route() {
            self.note_process_error(error);
            return PreparedParameterOutcome::Failed;
        }
        PreparedParameterOutcome::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_audio::AudioEffect;

    fn fixture() -> Vst2Instance {
        let path = crate::vst2::test_support::fixture_path("OpenCompose Gain Fixture")
            .expect("controlled VST2 fixture must be built; this test never skips");
        Vst2Instance::load(&path, 48_000, 64).expect("controlled fixture must load")
    }

    fn calls(instance: &Vst2Instance) -> isize {
        instance.dispatch(10002, 0, 0, std::ptr::null_mut())
    }

    #[test]
    fn prepared_parameter_setters_preserve_native_values_count_and_pcm() {
        let mut named = fixture();
        let mut bound = fixture();
        let handle = bound.prepare_parameter("gAiN").unwrap();
        let lookups = bound.parameter_name_lookups.get();
        let values = [-1.0, 0.25, 0.25 + 1e-10, 0.25 + 2e-10, 1.5, -0.0];
        for value in values {
            assert!(named.set_param_by_name("Gain", value));
            assert_eq!(
                bound.set_prepared_parameter(handle, value),
                PreparedParameterOutcome::Applied
            );
            assert_eq!(named.read_param(0).to_bits(), bound.read_param(0).to_bits());
            let mut left = [0.125, -0.25];
            let mut right = left;
            named.process_effect_into(&mut left, 48_000, None).unwrap();
            bound.process_effect_into(&mut right, 48_000, None).unwrap();
            assert_eq!(left, right);
        }
        assert_eq!(calls(&named), values.len() as isize);
        assert_eq!(
            calls(&bound),
            values.len() as isize,
            "equal f32 payloads still call the setter"
        );
        assert_eq!(bound.parameter_name_lookups.get(), lookups);
    }

    #[test]
    fn prepared_parameter_identity_refuses_foreign_stale_and_invalid_slots() {
        let mut first = fixture();
        let mut second = fixture();
        let handle = first.prepare_parameter("Gain").unwrap();
        assert_eq!(
            second.set_prepared_parameter(handle, 0.7),
            PreparedParameterOutcome::Invalidated(PreparedParameterError::ForeignOwner)
        );
        assert_eq!(calls(&second), 0);
        assert!(second.processing_failed());
        second.reset();
        assert!(second.prepare_block_buffers(64).is_err());
        assert!(
            second.processing_failed(),
            "ordinary reset/preparation cannot rebind stale automation"
        );
        let own = second.prepare_parameter("Gain").unwrap();
        assert_eq!(
            second.set_prepared_parameter(own, 0.7),
            PreparedParameterOutcome::Applied
        );
        assert_eq!(calls(&second), 1);

        let state = first.state_bytes().unwrap();
        first.set_state_bytes(&state).unwrap();
        let lookups = first.parameter_name_lookups.get();
        assert_eq!(
            first.set_prepared_parameter(handle, 0.7),
            PreparedParameterOutcome::Invalidated(PreparedParameterError::Invalidated)
        );
        assert_eq!(first.parameter_name_lookups.get(), lookups);
        assert!(!first.set_param_by_name("Gain", 0.7));
        assert_eq!(calls(&first), 0, "a rejected binding never retries by name");

        let rebound = first.prepare_parameter("Gain").unwrap();
        assert_eq!(
            first.set_prepared_parameter(rebound, 0.5),
            PreparedParameterOutcome::Applied
        );
        let invalid = first
            .prepared_parameters
            .as_ref()
            .unwrap()
            .bind(u32::MAX)
            .unwrap();
        assert_eq!(
            first.set_prepared_parameter(invalid, 0.5),
            PreparedParameterOutcome::Invalidated(PreparedParameterError::OutOfBounds)
        );
        assert_eq!(calls(&first), 1);
    }

    #[test]
    fn prepared_parameter_resolution_preserves_named_precedence() {
        let mut instance = fixture();
        instance.param_names = vec!["Rate Hz".into(), "Rate-Hz".into()];
        for (name, index) in [("Rate-Hz", 1), ("RATE HZ", 0), ("rate_hz", 0), ("#1", 1)] {
            let handle = instance.prepare_parameter(name).unwrap();
            assert_eq!(
                instance
                    .prepared_parameters
                    .as_ref()
                    .unwrap()
                    .resolve(handle),
                Ok(index)
            );
        }
        assert!(instance.prepare_parameter("#2").is_none());
        assert!(instance.prepare_parameter("missing").is_none());
        let handle = instance.prepare_parameter("#0").unwrap();
        assert_eq!(
            instance.set_prepared_parameter(handle, f64::NAN),
            PreparedParameterOutcome::Failed
        );
        assert!(instance.processing_failed());
        assert_eq!(calls(&instance), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prepared_parameter_native_writes_have_no_heap_operations() {
        // The host aggregate library tests install the shared counting allocator
        // through their native boundary fixture on this platform.
        let mut instance = fixture();
        let handle = instance.prepare_parameter("Gain").unwrap();
        let lookups = instance.parameter_name_lookups.get();
        let count = oc_test_support::operations(|| {
            for index in 0..128 {
                assert_eq!(
                    instance.set_prepared_parameter(handle, 0.25 + f64::from(index) * 1e-10),
                    PreparedParameterOutcome::Applied
                );
            }
        });
        assert_eq!(count, 0);
        assert_eq!(calls(&instance), 128);
        assert_eq!(instance.parameter_name_lookups.get(), lookups);
    }
}
