//! One LIFO address ledger shared by legacy and owned MIDI delivery.
#[derive(Debug, Clone, Copy)]
struct Slot {
    note_id: i32,
    next: Option<usize>,
}
#[derive(Debug)]
pub(super) struct Vst3NoteIds {
    next_id: i32,
    heads: Box<[Option<usize>]>,
    slots: Vec<Slot>,
    free: Option<usize>,
    active: usize,
}
impl Default for Vst3NoteIds {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}
impl Vst3NoteIds {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            next_id: 0,
            heads: vec![None; 16 * 128].into_boxed_slice(),
            slots: Vec::with_capacity(capacity),
            free: None,
            active: 0,
        }
    }
    fn address(channel: i16, key: i16) -> Option<usize> {
        if !(0..16).contains(&channel) || !(0..128).contains(&key) {
            return None;
        }
        Some(channel as usize * 128 + key as usize)
    }
    pub(super) fn reserve(&mut self, capacity: usize) {
        self.slots
            .reserve(capacity.saturating_sub(self.slots.len()));
    }
    pub(super) fn active_count(&self) -> usize {
        self.active
    }
    pub(super) fn identity_available(&self) -> bool {
        self.next_id < i32::MAX
    }
    pub(super) fn copy_prepared(&mut self, other: &Self) -> bool {
        if other.slots.len() > self.slots.capacity() {
            return false;
        }
        self.next_id = other.next_id;
        self.heads.copy_from_slice(&other.heads);
        self.free = other.free;
        self.active = other.active;
        self.slots.clear();
        self.slots.extend_from_slice(&other.slots);
        true
    }
    pub(super) fn clear(&mut self) {
        self.heads.fill(None);
        self.slots.clear();
        self.free = None;
        self.active = 0;
    }
    pub(super) fn note_on(&mut self, channel: i16, key: i16) -> i32 {
        let Some(address) = Self::address(channel, key) else {
            return -1;
        };
        if !self.identity_available() {
            return -1;
        }
        let note_id = self.next_id;
        self.next_id += 1;
        let slot = Slot {
            note_id,
            next: self.heads[address],
        };
        let index = if let Some(index) = self.free {
            self.free = self.slots[index].next;
            self.slots[index] = slot;
            index
        } else {
            self.slots.push(slot);
            self.slots.len() - 1
        };
        self.heads[address] = Some(index);
        self.active += 1;
        note_id
    }
    pub(super) fn note_off(&mut self, channel: i16, key: i16) -> i32 {
        let Some(address) = Self::address(channel, key) else {
            return -1;
        };
        let Some(index) = self.heads[address] else {
            return -1;
        };
        let slot = self.slots[index];
        self.heads[address] = slot.next;
        self.slots[index].next = self.free;
        self.free = Some(index);
        self.active -= 1;
        slot.note_id
    }
    pub(super) fn active_note_id(&self, channel: i16, key: i16) -> i32 {
        Self::address(channel, key)
            .and_then(|address| self.heads[address])
            .map_or(-1, |index| self.slots[index].note_id)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_heads_stay_on_heap_through_copy_and_reset() {
        assert!(
            std::mem::size_of::<Vst3NoteIds>() <= 128,
            "by-value MIDI owners must not embed the address table"
        );
        let mut original = Vst3NoteIds::with_capacity(2);
        original.note_on(0, 60);
        let mut staged = Vst3NoteIds::with_capacity(2);
        let heads = staged.heads.as_ptr();
        let slots = staged.slots.as_ptr();
        assert_eq!(staged.heads.len(), 16 * 128);
        assert!(staged.copy_prepared(&original));
        assert_eq!(staged.active_note_id(0, 60), 0);
        assert_eq!(staged.heads.as_ptr(), heads);
        assert_eq!(staged.slots.as_ptr(), slots);
        staged.clear();
        assert_eq!(staged.heads.as_ptr(), heads);
        assert_eq!(staged.slots.as_ptr(), slots);
        assert_eq!(staged.active_note_id(0, 60), -1);
    }

    #[test]
    fn freed_slots_preserve_address_lifo_and_never_reuse_note_identity() {
        let mut notes = Vst3NoteIds::with_capacity(3);
        let first = notes.note_on(0, 60);
        let second = notes.note_on(0, 60);
        let third = notes.note_on(1, 60);
        assert_eq!(notes.note_off(0, 60), second);
        let reused = notes.note_on(0, 64);
        assert!(reused > third);
        assert_eq!(notes.active_note_id(0, 60), first);
        assert_eq!(notes.active_note_id(1, 60), third);
        assert_eq!(notes.note_off(0, 64), reused);
        assert_eq!(notes.note_off(0, 60), first);
        assert_eq!(notes.note_off(1, 60), third);
        assert_eq!(notes.active_count(), 0);
        assert_eq!(notes.slots.len(), 3);
    }
    #[test]
    fn staged_copy_is_bounded_and_exhaustion_does_not_wrap() {
        let mut original = Vst3NoteIds::with_capacity(2);
        original.note_on(0, 60);
        original.note_on(0, 64);
        let mut tiny = Vst3NoteIds::with_capacity(1);
        assert!(!tiny.copy_prepared(&original));
        assert_eq!(tiny.active_count(), 0);
        let mut staged = Vst3NoteIds::with_capacity(2);
        assert!(staged.copy_prepared(&original));
        staged.note_off(0, 64);
        assert_eq!(original.active_count(), 2);
        assert_eq!(staged.active_count(), 1);
        staged.next_id = i32::MAX;
        assert_eq!(staged.note_on(0, 67), -1);
        assert_eq!(staged.active_count(), 1);
        staged.clear();
        assert_eq!(staged.active_count(), 0);
        assert_eq!(staged.note_on(0, 67), -1);
    }
}
