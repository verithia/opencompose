//! Host-side `IMessage` and `IAttributeList` objects.
//!
//! Plugins ask `IHostApplication::createInstance` for these by IID when they
//! need to pass structured data to their editor. A host that answers
//! `kNotImplemented` hands the plugin a null message; editors that expect one
//! during construction crash. The objects here are the minimum working
//! implementation: an ID plus an attribute map with int, float, string and
//! binary values.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};

use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::{IAttributeList, IAttributeListTrait, IMessage, IMessageTrait, TChar};
use vst3::Steinberg::{FIDString, char16, int64, kResultFalse, kResultOk, tresult, uint32};
use vst3::{Class, ComPtr, ComWrapper};

#[derive(Default)]
struct MessageData {
    id: Option<CString>,
    ints: HashMap<String, i64>,
    floats: HashMap<String, f64>,
    strings: HashMap<String, Vec<u16>>,
    binaries: HashMap<String, Vec<u8>>,
}

/// VST3 host attribute list.
pub struct HostAttributeList {
    data: Arc<Mutex<MessageData>>,
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

/// VST3 host message.
pub struct HostMessage {
    data: Arc<Mutex<MessageData>>,
    attributes: ComPtr<IAttributeList>,
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

/// Create a message object with one reference for the caller.
pub fn create_message() -> *mut IMessage {
    let data = Arc::new(Mutex::new(MessageData::default()));
    let attributes = ComWrapper::new(HostAttributeList {
        data: Arc::clone(&data),
    })
    .to_com_ptr::<IAttributeList>()
    .expect("IAttributeList wrapper exposes its interface");
    ComWrapper::new(HostMessage { data, attributes })
        .to_com_ptr::<IMessage>()
        .expect("IMessage wrapper exposes its interface")
        .into_raw()
}

/// Create an attribute list with one reference for the caller.
pub fn create_attribute_list() -> *mut IAttributeList {
    ComWrapper::new(HostAttributeList {
        data: Arc::new(Mutex::new(MessageData::default())),
    })
    .to_com_ptr::<IAttributeList>()
    .expect("IAttributeList wrapper exposes its interface")
    .into_raw()
}

fn key(id: AttrID) -> String {
    if id.is_null() {
        return String::new();
    }
    // SAFETY: VST3 guarantees an `AttrID` points at a NUL-terminated string for
    // the duration of the call.
    unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned()
}

unsafe fn read_string(string: *const TChar) -> Vec<u16> {
    if string.is_null() {
        return Vec::new();
    }
    let mut units = Vec::new();
    let mut index = 0;
    loop {
        // SAFETY: the caller passes a NUL-terminated UTF-16 string.
        let unit = unsafe { *string.add(index) };
        if unit == 0 {
            break;
        }
        units.push(unit);
        index += 1;
    }
    units
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.id
            .as_ref()
            .map_or(std::ptr::null(), |id| id.as_ptr() as *const c_char)
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let value = if id.is_null() {
            None
        } else {
            // SAFETY: VST3 guarantees a valid NUL-terminated ID for this call.
            Some(unsafe { CStr::from_ptr(id) }.to_owned())
        };
        self.data.lock().unwrap_or_else(|e| e.into_inner()).id = value;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes.as_ptr()
    }
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ints
            .insert(key(id), value);
        kResultOk
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        if value.is_null() {
            return kResultFalse;
        }
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        match data.ints.get(&key(id)) {
            // SAFETY: the caller passes a valid output pointer.
            Some(found) => {
                // SAFETY: The unsafe COM caller supplies writable int64 storage; the null output was rejected before this locked value copy.
                unsafe { *value = *found };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .floats
            .insert(key(id), value);
        kResultOk
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        if value.is_null() {
            return kResultFalse;
        }
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        match data.floats.get(&key(id)) {
            // SAFETY: the caller passes a valid output pointer.
            Some(found) => {
                // SAFETY: The unsafe COM caller supplies writable f64 storage; the null output was rejected before this locked value copy.
                unsafe { *value = *found };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        // SAFETY: The unsafe VST3 caller supplies a readable NUL-terminated UTF-16 string, or null; read_string copies it during this call.
        let value = unsafe { read_string(string) };
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .strings
            .insert(key(id), value);
        kResultOk
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        if string.is_null() || (size_in_bytes as usize) < std::mem::size_of::<char16>() {
            return kResultFalse;
        }
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let Some(value) = data.strings.get(&key(id)) else {
            return kResultFalse;
        };
        let capacity = (size_in_bytes as usize) / std::mem::size_of::<char16>();
        let copied = value.len().min(capacity.saturating_sub(1));
        for (index, unit) in value.iter().take(copied).enumerate() {
            // SAFETY: the caller provides `capacity` UTF-16 units.
            unsafe { *string.add(index) = *unit as char16 };
        }
        // SAFETY: `copied < capacity`, so the terminator fits.
        unsafe { *string.add(copied) = 0 };
        kResultOk
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: uint32) -> tresult {
        if data.is_null() && size_in_bytes > 0 {
            return kResultFalse;
        }
        let mut bytes = Vec::with_capacity(size_in_bytes as usize);
        if size_in_bytes > 0 {
            // SAFETY: the caller provides `size_in_bytes` readable bytes.
            bytes.extend_from_slice(unsafe {
                std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize)
            });
        }
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .binaries
            .insert(key(id), bytes);
        kResultOk
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kResultFalse;
        }
        let data_guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let Some(bytes) = data_guard.binaries.get(&key(id)) else {
            return kResultFalse;
        };
        // SAFETY: the caller passes valid output pointers. The returned pointer
        // stays valid until the attribute is replaced.
        unsafe {
            *data = bytes.as_ptr() as *const c_void;
            *size_in_bytes = bytes.len() as uint32;
        }
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::{ComPtr, ComRef};

    #[test]
    fn message_id_and_attributes_round_trip() {
        let raw = create_message();
        // SAFETY: create_message returns one owned IMessage reference; this ComPtr adopts that reference exactly once.
        let message = unsafe { ComPtr::<IMessage>::from_raw(raw) }.expect("message");
        let id = CString::new("opencompose.editor").unwrap();
        // SAFETY: The owned in-process message stays alive; the CString is NUL-terminated and no mutation occurs while reading its returned ID.
        unsafe {
            message.setMessageID(id.as_ptr());
            let read = CStr::from_ptr(message.getMessageID());
            assert_eq!(read.to_str().unwrap(), "opencompose.editor");
        }

        let attributes =
            // SAFETY: The owned message retains its non-null in-process attribute list throughout this borrowed interface use.
            unsafe { ComRef::<IAttributeList>::from_raw_unchecked(message.getAttributes()) };
        let key = CString::new("count").unwrap();
        // SAFETY: The retained in-process attribute list receives a live NUL-terminated key and writable scalar output.
        unsafe {
            assert_eq!(attributes.setInt(key.as_ptr(), 7), kResultOk);
            let mut value = 0;
            assert_eq!(attributes.getInt(key.as_ptr(), &mut value), kResultOk);
            assert_eq!(value, 7);
        }
    }

    #[test]
    fn get_string_rejects_a_buffer_smaller_than_one_char16() {
        let raw = create_attribute_list();
        // SAFETY: create_attribute_list returns one owned interface reference; this ComPtr adopts it once.
        let attributes = unsafe { ComPtr::<IAttributeList>::from_raw(raw) }.expect("attributes");
        let key = CString::new("payload").unwrap();
        let text: Vec<char16> = "hello".encode_utf16().chain([0]).collect();
        // SAFETY: The owned in-process list receives a NUL-terminated key and a live one-byte output buffer; the call must refuse it.
        unsafe {
            assert_eq!(attributes.setString(key.as_ptr(), text.as_ptr()), kResultOk);
            let mut output = [0_u8; 1];
            assert_eq!(
                attributes.getString(key.as_ptr(), output.as_mut_ptr() as *mut char16, 1),
                kResultFalse
            );
            assert_eq!(output[0], 0);
        }
    }

    #[test]
    fn attribute_strings_and_binary_values_round_trip() {
        let raw = create_attribute_list();
        // SAFETY: create_attribute_list returns one owned interface reference; this ComPtr adopts it once.
        let attributes = unsafe { ComPtr::<IAttributeList>::from_raw(raw) }.expect("attributes");
        let key = CString::new("payload").unwrap();
        let text: Vec<char16> = "hello".encode_utf16().chain([0]).collect();
        let bytes = [1_u8, 2, 3, 4];
        // SAFETY: The owned in-process list receives a NUL-terminated key/text and buffers with the exact byte capacities passed below.
        unsafe {
            assert_eq!(attributes.setString(key.as_ptr(), text.as_ptr()), kResultOk);
            let mut output = [0_u16; 16];
            assert_eq!(
                attributes.getString(key.as_ptr(), output.as_mut_ptr(), 32),
                kResultOk
            );
            assert_eq!(&output[..5], &text[..5]);

            assert_eq!(
                attributes.setBinary(
                    key.as_ptr(),
                    bytes.as_ptr() as *const c_void,
                    bytes.len() as uint32
                ),
                kResultOk
            );
            let mut data: *const c_void = std::ptr::null();
            let mut size = 0;
            assert_eq!(
                attributes.getBinary(key.as_ptr(), &mut data, &mut size),
                kResultOk
            );
            assert_eq!(size, bytes.len() as uint32);
            let read = std::slice::from_raw_parts(data as *const u8, size as usize);
            assert_eq!(read, bytes);
        }
    }
}
