//! Restore bounded VST2 parameter programs and preset banks.

use super::{Vst2Instance, Vst2ProcessError, abi::plugin_op as op};
use std::ffi::c_void;

const BANK_HEADER: usize = 156;
const CHUNK_HEADER: usize = BANK_HEADER + 4;
const PROGRAM_HEADER: usize = 56;
const CHUNK_PROGRAM_HEADER: usize = PROGRAM_HEADER + 4;
const MAX_PROGRAM_NAME: usize = 24;
const MAX_PROGRAMS: usize = 1024;
const MAX_PARAMETER_WRITES: usize = 262_144;

#[derive(Debug, PartialEq, Eq)]
enum InvalidBank {
    Header,
    Plugin,
    Dimensions,
    Program,
    Value,
}

impl std::fmt::Display for InvalidBank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Header => "invalid or unsupported VST2 FXB header",
            Self::Plugin => "VST2 FXB belongs to another plugin",
            Self::Dimensions => "VST2 FXB does not fit the exposed program or parameter counts",
            Self::Program => "invalid record or name in VST2 FXB",
            Self::Value => "VST2 FXB contains an invalid normalized parameter value",
        })
    }
}

struct Program<'a> {
    name: &'a [u8],
    values: &'a [u8],
}

impl<'a> Program<'a> {
    fn parse(record: &'a [u8], unique_id: u32, parameters: usize) -> Result<Self, InvalidBank> {
        if parameters > super::MAX_REPORTED_PARAMS as usize
            || record.len() != PROGRAM_HEADER + parameters * 4
            || !is_parameter_program(record)
            || !size_matches(record)
            || word(record, 12) != 1
            || word(record, 16) != unique_id as usize
            || word(record, 24) != parameters
        {
            return Err(InvalidBank::Program);
        }
        let name_bytes = &record[28..PROGRAM_HEADER];
        let name = name_bytes.split(|byte| *byte == 0).next().unwrap();
        if name.len() > MAX_PROGRAM_NAME {
            return Err(InvalidBank::Program);
        }
        let program = Self {
            name,
            values: &record[PROGRAM_HEADER..],
        };
        if program
            .parameters()
            .any(|value| !(0.0..=1.0).contains(&value))
        {
            return Err(InvalidBank::Value);
        }
        Ok(program)
    }

    fn parameters(&self) -> impl Iterator<Item = f32> {
        self.values
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_be_bytes(*bytes))
    }
}

struct Bank<'a> {
    programs: Vec<Program<'a>>,
    selected: Option<usize>,
}

struct ChunkBank<'a> {
    bytes: &'a [u8],
    selected: Option<usize>,
}

pub(super) enum ChunkInput<'a> {
    Bank(&'a [u8], Option<usize>),
    Program(&'a [u8]),
}

impl<'a> ChunkInput<'a> {
    pub(super) fn parse(
        bytes: &'a [u8],
        unique_id: u32,
        available_programs: usize,
    ) -> Result<Self, String> {
        let result = if is_chunk_program(bytes) {
            chunk_program(bytes, unique_id).map(Self::Program)
        } else if bytes.starts_with(b"CcnK") && bytes.get(8..12) == Some(b"FBCh") {
            ChunkBank::parse(bytes, unique_id, available_programs)
                .map(|bank| Self::Bank(bank.bytes, bank.selected))
        } else {
            return Ok(Self::Bank(bytes, None));
        };
        result.map_err(|error| error.to_string())
    }
}

pub(super) fn is_chunk_program(bytes: &[u8]) -> bool {
    bytes.starts_with(b"CcnK") && bytes.get(8..12) == Some(b"FPCh")
}

fn chunk_program(bytes: &[u8], unique_id: u32) -> Result<&[u8], InvalidBank> {
    if bytes.len() <= CHUNK_PROGRAM_HEADER
        || !size_matches(bytes)
        || word(bytes, 12) != 1
        || word(bytes, PROGRAM_HEADER) != bytes.len() - CHUNK_PROGRAM_HEADER
    {
        return Err(InvalidBank::Program);
    }
    if word(bytes, 16) != unique_id as usize {
        return Err(InvalidBank::Plugin);
    }
    Ok(&bytes[CHUNK_PROGRAM_HEADER..])
}

fn word(record: &[u8], offset: usize) -> usize {
    u32::from_be_bytes(record[offset..offset + 4].try_into().unwrap()) as usize
}

fn size_matches(record: &[u8]) -> bool {
    // Hosts may leave the byte-size field zero. Record boundaries are still
    // determined and checked from the bounded parameter and program counts.
    let declared = word(record, 4);
    declared == 0 || declared == record.len() - 8
}

pub(super) fn is_parameter_bank(bytes: &[u8]) -> bool {
    bytes.starts_with(b"CcnK") && bytes.get(8..12) == Some(b"FxBk")
}

pub(super) fn is_parameter_program(bytes: &[u8]) -> bool {
    bytes.starts_with(b"CcnK") && bytes.get(8..12) == Some(b"FxCk")
}

impl<'a> ChunkBank<'a> {
    fn parse(bytes: &'a [u8], unique_id: u32, available: usize) -> Result<Self, InvalidBank> {
        if bytes.len() < CHUNK_HEADER || !size_matches(bytes) || !matches!(word(bytes, 12), 1 | 2) {
            return Err(InvalidBank::Header);
        }
        if word(bytes, 16) != unique_id as usize {
            return Err(InvalidBank::Plugin);
        }
        let selected = if word(bytes, 12) == 2 {
            let index = word(bytes, 28);
            if index >= available.max(1) {
                return Err(InvalidBank::Dimensions);
            }
            (available != 0).then_some(index)
        } else {
            None
        };
        let chunk = &bytes[CHUNK_HEADER..];
        if chunk.is_empty() || word(bytes, BANK_HEADER) != chunk.len() {
            return Err(InvalidBank::Header);
        }
        Ok(Self {
            bytes: chunk,
            selected,
        })
    }
}

/// Validate an opaque bank whose active program is the first entry.
pub(crate) fn migration_bank_id(bytes: &[u8]) -> Result<u32, String> {
    if bytes.len() < CHUNK_HEADER
        || bytes.len() > crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES
        || !bytes.starts_with(b"CcnK")
        || &bytes[8..12] != b"FBCh"
    {
        return Err("expected a bounded opaque VST2 FXB bank".into());
    }
    if word(bytes, 28) != 0 {
        return Err("the VST2 FXB bank selects a program other than the first".into());
    }
    let id = word(bytes, 16) as u32;
    ChunkBank::parse(bytes, id, 1).map_err(|error| error.to_string())?;
    Ok(id)
}

impl<'a> Bank<'a> {
    fn parse(
        bytes: &'a [u8],
        unique_id: u32,
        available_programs: usize,
        parameters: usize,
    ) -> Result<Self, InvalidBank> {
        if bytes.len() < BANK_HEADER
            || &bytes[..4] != b"CcnK"
            || &bytes[8..12] != b"FxBk"
            || !size_matches(bytes)
            || !matches!(word(bytes, 12), 1 | 2)
        {
            return Err(InvalidBank::Header);
        }
        let version = word(bytes, 12);
        let count = word(bytes, 24);
        if word(bytes, 16) != unique_id as usize {
            return Err(InvalidBank::Plugin);
        }
        if count == 0
            || count > available_programs
            || count > MAX_PROGRAMS
            || parameters > super::MAX_REPORTED_PARAMS as usize
            || count * parameters > MAX_PARAMETER_WRITES
        {
            return Err(InvalidBank::Dimensions);
        }
        let selected = (version == 2).then(|| word(bytes, 28));
        if selected.is_some_and(|index| index >= count) {
            return Err(InvalidBank::Dimensions);
        }
        let stride = PROGRAM_HEADER + parameters * 4;
        let payload = &bytes[BANK_HEADER..];
        if payload.len() != stride * count {
            return Err(InvalidBank::Program);
        }
        let mut programs = Vec::with_capacity(count);
        for record in payload.chunks_exact(stride) {
            programs.push(Program::parse(record, unique_id, parameters)?);
        }
        Ok(Self { programs, selected })
    }
}

impl Vst2Instance {
    pub(super) fn restore_parameter_program(&mut self, bytes: &[u8]) -> Result<(), String> {
        // SAFETY: State control exclusively retains the healthy descriptor.
        let effect = unsafe { &*self.effect };
        let parameters =
            usize::try_from(effect.num_params).map_err(|_| InvalidBank::Dimensions.to_string())?;
        let program = Program::parse(bytes, effect.unique_id as u32, parameters)
            .map_err(|error| error.to_string())?;
        let setter = effect
            .set_parameter
            .ok_or_else(|| "VST2 plugin has no parameter setter".to_string())?;
        let name_program = effect.num_programs > 0;
        self.invalidate_prepared_parameters();
        if name_program {
            self.set_bank_program_name(program.name)?;
        }
        self.write_program_parameters(setter, &program)?;
        self.finish_parameter_restore()
    }

    pub(super) fn restore_chunk_bank_selection(&self, index: usize) -> Result<(), String> {
        let current = self.dispatch(op::GET_PROGRAM, 0, 0, std::ptr::null_mut());
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        if current != index as isize {
            self.select_bank_program(index)?;
        }
        Ok(())
    }

    pub(super) fn restore_parameter_bank(&mut self, bytes: &[u8]) -> Result<(), String> {
        // SAFETY: The caller admits exclusive healthy state control before
        // entering this function. The descriptor stays live for the instance.
        let effect = unsafe { &*self.effect };
        let programs = usize::try_from(effect.num_programs).unwrap_or(0);
        let parameters =
            usize::try_from(effect.num_params).map_err(|_| InvalidBank::Dimensions.to_string())?;
        let bank = Bank::parse(bytes, effect.unique_id as u32, programs, parameters)
            .map_err(|error| error.to_string())?;
        let setter = effect
            .set_parameter
            .ok_or_else(|| "VST2 plugin has no parameter setter".to_string())?;

        let previous = self.dispatch(op::GET_PROGRAM, 0, 0, std::ptr::null_mut());
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        let previous = usize::try_from(previous)
            .ok()
            .filter(|index| *index < programs)
            .ok_or_else(|| "VST2 plugin reported an invalid program selection".to_string())?;
        // All byte, dimension and identity validation precedes native writes.
        self.invalidate_prepared_parameters();
        for (index, program) in bank.programs.iter().enumerate() {
            self.select_bank_program(index)?;
            self.set_bank_program_name(program.name)?;
            self.write_program_parameters(setter, program)?;
        }
        self.select_bank_program(bank.selected.unwrap_or(previous))?;
        self.finish_parameter_restore()
    }

    fn write_program_parameters(
        &mut self,
        setter: super::abi::SetParameterProc,
        program: &Program<'_>,
    ) -> Result<(), String> {
        for (parameter, value) in program.parameters().enumerate() {
            self.write_parameter_callback(setter, parameter as i32, value);
            self.check_callback_route()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(super) fn finish_parameter_restore(&mut self) -> Result<(), String> {
        if matches!(
            self.process_error,
            Some(Vst2ProcessError::PreparedParameter(_))
        ) {
            self.process_error = None;
            self.error_reported.set(false);
        }
        for index in 0..self.num_params {
            self.host.store_value(index, self.read_param(index));
            self.check_callback_route()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn select_bank_program(&self, index: usize) -> Result<(), String> {
        self.dispatch(op::SET_PROGRAM, 0, index as isize, std::ptr::null_mut());
        let current = self.dispatch(op::GET_PROGRAM, 0, 0, std::ptr::null_mut());
        self.check_callback_route()
            .map_err(|error| error.to_string())?;
        if current != index as isize {
            return Err(format!("VST2 plugin refused program {index}"));
        }
        Ok(())
    }

    fn set_bank_program_name(&self, name: &[u8]) -> Result<(), String> {
        let mut text = [0u8; MAX_PROGRAM_NAME + 1];
        text[..name.len()].copy_from_slice(name);
        let input = if let Some(packet) = self.owned_scratch() {
            // SAFETY: The exclusive state owner uses the retained input
            // packet so a native fault cannot borrow a dead stack pointer.
            let packet = unsafe { &mut *packet };
            packet.prepare_state_input(&text)?;
            packet.state_input.as_mut_ptr().cast()
        } else {
            text.as_mut_ptr().cast::<c_void>()
        };
        self.dispatch(op::SET_PROGRAM_NAME, 0, 0, input);
        self.check_callback_route()
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_ID: u32 = 0x4f43_4632;
    const PROGRAMS: [(&str, [f32; 2]); 3] = [
        ("First", [0.25, 0.0]),
        ("Second", [0.5, 0.75]),
        ("Third", [0.75, 0.5]),
    ];

    fn put_word(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn bank(version: u32, selected: u32) -> Vec<u8> {
        const STRIDE: usize = PROGRAM_HEADER + 8;
        let size = BANK_HEADER + STRIDE * PROGRAMS.len();
        let mut bytes = vec![0; size];
        bytes[..4].copy_from_slice(b"CcnK");
        put_word(&mut bytes, 4, (size - 8) as u32);
        bytes[8..12].copy_from_slice(b"FxBk");
        put_word(&mut bytes, 12, version);
        put_word(&mut bytes, 16, FIXTURE_ID);
        put_word(&mut bytes, 24, PROGRAMS.len() as u32);
        put_word(&mut bytes, 28, selected);
        for (record, (name, values)) in bytes[BANK_HEADER..].chunks_exact_mut(STRIDE).zip(PROGRAMS)
        {
            record[..4].copy_from_slice(b"CcnK");
            put_word(record, 4, (STRIDE - 8) as u32);
            record[8..12].copy_from_slice(b"FxCk");
            put_word(record, 12, 1);
            put_word(record, 16, FIXTURE_ID);
            put_word(record, 24, 2);
            record[28..28 + name.len()].copy_from_slice(name.as_bytes());
            put_word(record, PROGRAM_HEADER, values[0].to_bits());
            put_word(record, PROGRAM_HEADER + 4, values[1].to_bits());
        }
        bytes
    }

    fn chunk_bank(selected: u32, chunk: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; CHUNK_HEADER];
        bytes[..4].copy_from_slice(b"CcnK");
        put_word(&mut bytes, 4, (CHUNK_HEADER + chunk.len() - 8) as u32);
        bytes[8..12].copy_from_slice(b"FBCh");
        put_word(&mut bytes, 12, 2);
        put_word(&mut bytes, 16, FIXTURE_ID);
        put_word(&mut bytes, 24, 1);
        put_word(&mut bytes, 28, selected);
        put_word(&mut bytes, BANK_HEADER, chunk.len() as u32);
        bytes.extend_from_slice(chunk);
        bytes
    }

    fn opaque_program(chunk: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; CHUNK_PROGRAM_HEADER];
        bytes[..4].copy_from_slice(b"CcnK");
        put_word(
            &mut bytes,
            4,
            (CHUNK_PROGRAM_HEADER + chunk.len() - 8) as u32,
        );
        bytes[8..12].copy_from_slice(b"FPCh");
        put_word(&mut bytes, 12, 1);
        put_word(&mut bytes, 16, FIXTURE_ID);
        put_word(&mut bytes, 24, 1);
        put_word(&mut bytes, PROGRAM_HEADER, chunk.len() as u32);
        bytes.extend_from_slice(chunk);
        bytes
    }

    #[test]
    fn native_fxp_opaque_program_restores_only_the_current_program_and_changes_audio() {
        let fixture = crate::vst2::test_support::fixture_path("OpenCompose Program Bank Fixture")
            .expect("the program-bank fixture is built for the host");
        let mut instance = Vst2Instance::load(&fixture, 48_000, 64).unwrap();
        instance.set_state_bytes(&bank(2, 1)).unwrap();
        let mut state = b"OCP2".to_vec();
        state.extend_from_slice(&0.875_f32.to_le_bytes());
        state.extend_from_slice(&0.25_f32.to_le_bytes());
        let valid = opaque_program(&state);
        assert!(instance.set_state_bytes(&valid).is_err());
        instance.flags |= super::super::abi::FLAG_PROGRAM_CHUNKS;
        for (offset, value) in [(4, 1), (12, 2), (16, 7), (PROGRAM_HEADER, 2)] {
            let mut invalid = valid.clone();
            put_word(&mut invalid, offset, value);
            assert!(instance.set_state_bytes(&invalid).is_err());
            assert_eq!(instance.get_param_by_name("Gain"), Some(0.5));
        }
        assert!(
            instance
                .set_state_bytes(&valid[..CHUNK_PROGRAM_HEADER])
                .is_err()
        );
        assert!(instance.set_state_bytes(&opaque_program(&[])).is_err());
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.5));
        instance.set_state_bytes(&valid).unwrap();
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "Second");
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.875));
        let mut audio = [0.25; 64];
        instance
            .process_effect_into(&mut audio, 48_000, None)
            .unwrap();
        assert!(audio.iter().all(|sample| *sample == 0.5));
        instance.select_bank_program(0).unwrap();
        assert_eq!(instance.read_param(0), 0.25);
        instance.select_bank_program(2).unwrap();
        assert_eq!(instance.read_param(0), 0.75);
    }

    #[test]
    fn native_fxb_chunk_bank_unwraps_state_and_selects_the_saved_program() {
        let fixture = crate::vst2::test_support::fixture_path("OpenCompose Program Bank Fixture")
            .expect("the program-bank fixture is built for the host");
        let mut instance = Vst2Instance::load(&fixture, 48_000, 64).unwrap();
        instance.set_state_bytes(&bank(2, 0)).unwrap();
        // This fixture implements both dispatcher paths. Advertise chunk support for this case.
        instance.flags |= super::super::abi::FLAG_PROGRAM_CHUNKS;
        let state = instance.state_bytes().unwrap();
        let valid = chunk_bank(2, &state);
        let mut wrong_id = valid.clone();
        put_word(&mut wrong_id, 16, FIXTURE_ID ^ 1);
        let mut wrong_size = valid.clone();
        put_word(&mut wrong_size, BANK_HEADER, u32::MAX);
        for invalid in [&wrong_id, &wrong_size, &chunk_bank(3, &state)] {
            assert!(instance.set_state_bytes(invalid).is_err());
            assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "First");
        }
        assert!(ChunkBank::parse(&valid, FIXTURE_ID, 0).is_err());
        assert_eq!(
            ChunkBank::parse(&chunk_bank(0, &state), FIXTURE_ID, 0)
                .unwrap()
                .selected,
            None
        );
        instance.set_state_bytes(&valid).unwrap();
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "Third");
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.75));
        let mut audio = [0.25; 64];
        instance
            .process_effect_into(&mut audio, 48_000, None)
            .unwrap();
        assert!(audio.iter().all(|sample| *sample == 0.5));
    }

    #[test]
    fn malformed_fxb_program_bank_is_rejected_before_native_writes() {
        let source = bank(2, 1);
        let parse = |bytes| Bank::parse(bytes, FIXTURE_ID, 3, 2).err();
        assert_eq!(parse(&source[..20]), Some(InvalidBank::Header));
        assert_eq!(
            parse(&source[..source.len() - 1]),
            Some(InvalidBank::Header)
        );
        for (offset, value, error) in [
            (12, 3, InvalidBank::Header),
            (16, 0, InvalidBank::Plugin),
            (24, u32::MAX, InvalidBank::Dimensions),
            (28, 3, InvalidBank::Dimensions),
            (BANK_HEADER + 16, 0, InvalidBank::Program),
            (BANK_HEADER + 24, 3, InvalidBank::Program),
            (source.len() - 4, f32::NAN.to_bits(), InvalidBank::Value),
            (source.len() - 4, 2.0f32.to_bits(), InvalidBank::Value),
        ] {
            let mut malformed = source.clone();
            put_word(&mut malformed, offset, value);
            assert_eq!(Bank::parse(&malformed, FIXTURE_ID, 3, 2).err(), Some(error));
        }
        let mut malformed = source;
        malformed[BANK_HEADER + 28..BANK_HEADER + PROGRAM_HEADER].fill(b'x');
        assert_eq!(
            Bank::parse(&malformed, FIXTURE_ID, 3, 2).err(),
            Some(InvalidBank::Program)
        );
    }

    #[test]
    fn native_fxb_program_bank_restores_distinct_names_values_and_selected_sound() {
        let fixture = crate::vst2::test_support::fixture_path("OpenCompose Program Bank Fixture")
            .expect("the parameter-bank fixture is built for the host");
        let mut instance = Vst2Instance::load(&fixture, 48_000, 64).unwrap();
        let mut invalid = bank(2, 2);
        let last = invalid.len() - 4;
        put_word(&mut invalid, last, f32::NAN.to_bits());
        assert!(instance.set_state_bytes(&invalid).is_err());
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.5));

        instance.set_state_bytes(&bank(2, 2)).unwrap();
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "Third");
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.75));
        let mut buffer = [0.25; 64];
        instance
            .process_effect_into(&mut buffer, 48_000, None)
            .unwrap();
        assert!(buffer.iter().all(|sample| *sample == 0.5));
        for (index, (name, params)) in PROGRAMS.iter().enumerate() {
            instance.select_bank_program(index).unwrap();
            assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), *name);
            assert_eq!([instance.read_param(0), instance.read_param(1)], *params);
        }
        let mut legacy = bank(1, 0);
        put_word(&mut legacy, 4, 0);
        instance.set_state_bytes(&legacy).unwrap();
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "Third");
    }

    #[test]
    fn native_fxp_restores_only_the_current_program_and_validates_before_writing() {
        const STRIDE: usize = PROGRAM_HEADER + 8;
        let fixture = crate::vst2::test_support::fixture_path("OpenCompose Program Bank Fixture")
            .expect("the parameter-bank fixture is built for the host");
        let mut instance = Vst2Instance::load(&fixture, 48_000, 64).unwrap();
        instance.set_state_bytes(&bank(2, 1)).unwrap();
        let contents = bank(2, 0);
        let third = &contents[BANK_HEADER + 2 * STRIDE..];
        let mut invalid = third.to_vec();
        put_word(&mut invalid, 16, FIXTURE_ID ^ 1);
        assert!(instance.set_state_bytes(&invalid).is_err());
        put_word(&mut invalid, 16, FIXTURE_ID);
        put_word(&mut invalid, STRIDE - 4, f32::NAN.to_bits());
        assert!(instance.set_state_bytes(&invalid).is_err());
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "Second");
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.5));
        instance.set_state_bytes(third).unwrap();
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "Third");
        assert_eq!(instance.get_param_by_name("Gain"), Some(0.75));
        let mut audio = [0.25; 32];
        instance.prepare_block_buffers(64).unwrap();
        instance
            .process_effect_into(&mut audio, 48_000, None)
            .unwrap();
        assert!(audio.iter().all(|sample| *sample == 0.5));
        instance.select_bank_program(0).unwrap();
        assert_eq!(instance.query_string(op::GET_PROGRAM_NAME), "First");
        assert_eq!(instance.read_param(0), 0.25);
    }
}
