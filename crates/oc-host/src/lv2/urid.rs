//! Host-side URID map for atom and state interfaces.

use std::ffi::{CStr, CString, c_char, c_void};

use super::abi::Lv2Urid;

/// URI-to-URID map handed to an LV2 plugin.
pub struct UridMap {
    entries: Vec<(CString, Lv2Urid)>,
    next: Lv2Urid,
}

impl Default for UridMap {
    fn default() -> Self {
        Self::new()
    }
}

impl UridMap {
    /// An empty map with ids starting at one.
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(64),
            next: 1,
        }
    }

    /// Intern a URI before the plugin is instantiated.
    pub fn preintern(&mut self, uri: &str) -> Lv2Urid {
        self.map(uri)
    }

    /// Stable numeric id for a URI. Zero means "unmappable".
    pub fn map(&mut self, uri: &str) -> Lv2Urid {
        if let Some((_, id)) = self
            .entries
            .iter()
            .find(|(name, _)| name.as_bytes() == uri.as_bytes())
        {
            return *id;
        }
        if self.entries.len() >= (Lv2Urid::MAX as usize - 1) {
            return 0;
        }
        let Ok(name) = CString::new(uri) else {
            return 0;
        };
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.entries.push((name, id));
        id
    }

    /// The URI for a URID, if known.
    pub fn unmap(&self, id: Lv2Urid) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, entry_id)| *entry_id == id)
            .and_then(|(name, _)| name.to_str().ok())
    }

    fn unmap_ptr(&self, id: Lv2Urid) -> *const c_char {
        self.entries
            .iter()
            .find(|(_, entry_id)| *entry_id == id)
            .map_or(std::ptr::null(), |(name, _)| name.as_ptr())
    }
}

/// Map a URI through the instance-owned feature handle.
///
/// # Safety
/// Non-null handle must point to a live, aligned UridMap with exclusive access
/// throughout this call. Non-null uri must be a readable NUL-terminated string.
/// Callers must serialize map/unmap callbacks and may not re-enter this map.
pub unsafe extern "C" fn map_callback(handle: *mut c_void, uri: *const c_char) -> Lv2Urid {
    if handle.is_null() || uri.is_null() {
        return 0;
    }
    // SAFETY: the callback contract guarantees a readable NUL-terminated uri.
    let Ok(uri) = unsafe { CStr::from_ptr(uri) }.to_str() else {
        return 0;
    };
    // SAFETY: the callback contract guarantees a live map and exclusive access.
    let map = unsafe { &mut *(handle as *mut UridMap) };
    map.map(uri)
}

/// Return a URI pointer owned by the instance's map.
///
/// # Safety
/// Non-null handle must point to a live, aligned UridMap with shared access for
/// this call. Serialize this callback against mapping and map destruction. A
/// returned pointer remains valid only while that map is alive.
pub unsafe extern "C" fn unmap_callback(handle: *mut c_void, id: Lv2Urid) -> *const c_char {
    if handle.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the callback contract guarantees a live map without concurrent mutation.
    let unmap = unsafe { &*(handle as *const UridMap) };
    unmap.unmap_ptr(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_round_trip() {
        let mut map = UridMap::new();
        let sequence = map.preintern("http://lv2plug.in/ns/ext/atom#Sequence");
        let midi = map.preintern("http://lv2plug.in/ns/ext/midi#MidiEvent");
        assert_ne!(sequence, 0);
        assert_ne!(sequence, midi);
        assert_eq!(
            map.preintern("http://lv2plug.in/ns/ext/atom#Sequence"),
            sequence
        );
        assert_eq!(
            map.unmap(midi),
            Some("http://lv2plug.in/ns/ext/midi#MidiEvent")
        );
        assert_eq!(map.unmap(9999), None);
    }
}
