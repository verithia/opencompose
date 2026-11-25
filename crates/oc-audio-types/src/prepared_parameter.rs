//! Runtime-only identities for parameters resolved outside audio processing.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// An instance-local parameter identity; it never owns a native pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedParameterHandle {
    owner: NonZeroU64,
    generation: u64,
    slot: u32,
}

/// Why a prepared parameter no longer identifies a writable slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedParameterError {
    ForeignOwner,
    Invalidated,
    OutOfBounds,
    NonFinite,
}

/// A prepared write either completes, uses the named adapter, or fails closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedParameterOutcome {
    Applied,
    Unsupported,
    Invalidated(PreparedParameterError),
    Failed,
}

/// Unique authority for one live instance's prepared parameter inventory.
///
/// Resolution only checks identity. The device still checks its index bounds,
/// callback admission and parameter domain before writing anything native.
pub struct PreparedParameterOwner {
    id: NonZeroU64,
    generation: Option<u64>,
}

impl PreparedParameterOwner {
    /// Allocate an identity without ever reusing an exhausted counter.
    pub fn new() -> Option<Self> {
        let id = NEXT_OWNER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()
            .and_then(NonZeroU64::new)?;
        Some(Self {
            id,
            generation: Some(0),
        })
    }

    /// Bind a checked device slot outside processing.
    pub fn bind(&self, slot: u32) -> Option<PreparedParameterHandle> {
        Some(PreparedParameterHandle {
            owner: self.id,
            generation: self.generation?,
            slot,
        })
    }

    /// Resolve an identity without allocation, native access or name lookup.
    pub fn resolve(&self, handle: PreparedParameterHandle) -> Result<u32, PreparedParameterError> {
        if handle.owner != self.id {
            return Err(PreparedParameterError::ForeignOwner);
        }
        if self.generation != Some(handle.generation) {
            return Err(PreparedParameterError::Invalidated);
        }
        Ok(handle.slot)
    }

    /// Invalidate all issued handles; generation exhaustion retires authority.
    pub fn invalidate(&mut self) {
        self.generation = self.generation.and_then(|value| value.checked_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_parameter_identity_is_owner_and_generation_scoped() {
        let mut first = PreparedParameterOwner::new().unwrap();
        let other = PreparedParameterOwner::new().unwrap();
        let handle = first.bind(17).unwrap();
        assert_eq!(first.resolve(handle), Ok(17));
        assert_eq!(
            other.resolve(handle),
            Err(PreparedParameterError::ForeignOwner)
        );
        first.invalidate();
        assert_eq!(
            first.resolve(handle),
            Err(PreparedParameterError::Invalidated)
        );
        assert_eq!(first.resolve(first.bind(17).unwrap()), Ok(17));
        first.generation = Some(u64::MAX);
        let last = first.bind(17).unwrap();
        first.invalidate();
        assert!(first.bind(17).is_none());
        assert_eq!(
            first.resolve(last),
            Err(PreparedParameterError::Invalidated)
        );
    }
}
