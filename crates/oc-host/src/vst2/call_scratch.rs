//! Stable storage for every host argument whose native call may fail.
use super::{MAX_CHANNELS, abi};
use std::cell::UnsafeCell;
use std::ffi::c_void;
const TEXT_BYTES: usize = if abi::MAX_NAME_STR > abi::MAX_PARAM_STR {
    abi::MAX_NAME_STR
} else {
    abi::MAX_PARAM_STR
};

/// Allocate this packet before native metadata or processing borrows its fields.
/// Keep it with the full candidate on failure; never replace it after poisoning.
pub(super) struct CallScratch {
    pub input_pointers: [*const f32; MAX_CHANNELS as usize],
    pub output_pointers: [*mut f32; MAX_CHANNELS as usize],
    pub text: [u8; TEXT_BYTES],
    pub properties: abi::VstParameterProperties,
    pub chunk_output: *mut c_void,
    pub rectangle_output: *mut abi::ERect,
    pub state_input: Vec<u8>,
    pub query_input: Vec<u8>,
}
impl CallScratch {
    pub(super) fn new() -> Box<UnsafeCell<Self>> {
        Box::new(UnsafeCell::new(Self {
            input_pointers: [std::ptr::null(); MAX_CHANNELS as usize],
            output_pointers: [std::ptr::null_mut(); MAX_CHANNELS as usize],
            text: [0; TEXT_BYTES],
            // SAFETY: This C record contains integer/float fields and byte
            // arrays, all of which admit zero initialization before output use.
            properties: unsafe { std::mem::zeroed() },
            chunk_output: std::ptr::null_mut(),
            rectangle_output: std::ptr::null_mut(),
            state_input: Vec::new(),
            query_input: Vec::new(),
        }))
    }
    pub(super) fn prepare_query_input(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        if bytes.len() >= 4096 || bytes.contains(&0) {
            return Err("VST2 feature input exceeds its bounds");
        }
        self.query_input.clear();
        self.query_input
            .try_reserve(bytes.len() + 1)
            .map_err(|_| "Could not prepare VST2 feature input")?;
        self.query_input.extend_from_slice(bytes);
        self.query_input.push(0);
        Ok(())
    }
    /// UI-only preparation before lending state bytes to native code.
    pub(super) fn prepare_state_input(&mut self, bytes: &[u8]) -> Result<(), String> {
        super::checked_state_chunk_size(bytes.len())?;
        self.state_input.clear();
        self.state_input
            .try_reserve(bytes.len())
            .map_err(|_| "Could not prepare bounded VST2 state input")?;
        self.state_input.extend_from_slice(bytes);
        Ok(())
    }
}
