//! Core MMCSS logic, kept free of Win32 so it can be host-unit-tested.
//!
//! The avrt exports (see `exports`) resolve a task name to a [`Category`], then
//! drive the calling thread's NT priority through a [`ThreadOs`] backend. The
//! real build wires `ThreadOs` to `Get/SetThreadPriority`/`SetLastError` (see
//! `win_os`); tests use a mock. Active registrations live in a fixed-size,
//! spinlock-guarded table so the crate needs no allocator.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// NT thread-priority deltas (winbase.h THREAD_PRIORITY_*). Wine's
// server/thread.c maps the resulting effective priority onto a host `nice`
// level (RLIMIT_NICE-gated), so raising these actually improves scheduling.
pub const THREAD_PRIORITY_NORMAL: i32 = 0;
pub const THREAD_PRIORITY_ABOVE_NORMAL: i32 = 1;
pub const THREAD_PRIORITY_HIGHEST: i32 = 2;
pub const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

// AVRT_PRIORITY (avrt.h), the adjustment AvSetMmThreadPriority applies within a
// task's scheduling category.
pub const AVRT_PRIORITY_VERYLOW: i32 = -2;
pub const AVRT_PRIORITY_LOW: i32 = -1;
pub const AVRT_PRIORITY_NORMAL: i32 = 0;
pub const AVRT_PRIORITY_HIGH: i32 = 1;
pub const AVRT_PRIORITY_CRITICAL: i32 = 2;

// Win32 error codes (winerror.h) reported on failure.
pub const ERROR_INVALID_HANDLE: u32 = 6;
pub const ERROR_NO_SYSTEM_RESOURCES: u32 = 1450;
pub const ERROR_INVALID_TASK_NAME: u32 = 1550;

/// Value reported by AvQuerySystemResponsiveness. Windows reads this from
/// `HKLM\...\SystemProfile\SystemResponsiveness` (percent of CPU reserved for
/// low-priority work); 20 is the documented default. Wine does not seed that
/// key, so we report the default directly.
pub const SYSTEM_RESPONSIVENESS_DEFAULT: u32 = 20;

/// Backend for the per-thread priority operations MMCSS performs on the *calling*
/// thread. Abstracted so the table/mapping logic is host-testable with a mock.
pub trait ThreadOs {
    /// `GetThreadPriority(GetCurrentThread())` -- saved so revert can restore it.
    fn get_priority(&self) -> i32;
    /// `SetThreadPriority(GetCurrentThread(), prio)`.
    fn set_priority(&self, prio: i32);
    /// `SetLastError(err)`.
    fn set_last_error(&self, err: u32);
}

/// MMCSS scheduling category. Ordered Low < Medium < High so two tasks can be
/// combined by taking the max (AvSetMmMaxThreadCharacteristics).
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Category {
    Low = 0,
    Medium = 1,
    High = 2,
}

impl Category {
    /// The NT thread-priority delta this category boosts a thread to at
    /// registration (AVRT_PRIORITY_NORMAL). High = time-critical (audio/render),
    /// Medium = highest (games/playback), Low = above-normal.
    pub fn base_priority(self) -> i32 {
        match self {
            Category::High => THREAD_PRIORITY_TIME_CRITICAL,
            Category::Medium => THREAD_PRIORITY_HIGHEST,
            Category::Low => THREAD_PRIORITY_ABOVE_NORMAL,
        }
    }

    /// Resolve the thread priority for an AVRT_PRIORITY adjustment within this
    /// category (AvSetMmThreadPriority). NORMAL keeps the category default; the
    /// others step the boost up or down.
    pub fn priority_for(self, avrt_priority: i32) -> i32 {
        match avrt_priority {
            AVRT_PRIORITY_CRITICAL => THREAD_PRIORITY_TIME_CRITICAL,
            AVRT_PRIORITY_HIGH => THREAD_PRIORITY_HIGHEST,
            AVRT_PRIORITY_LOW => THREAD_PRIORITY_ABOVE_NORMAL,
            AVRT_PRIORITY_VERYLOW => THREAD_PRIORITY_NORMAL,
            _ => self.base_priority(), // AVRT_PRIORITY_NORMAL and out-of-range
        }
    }
}

/// The standard Windows default MMCSS tasks and their scheduling category
/// (`HKLM\...\Multimedia\SystemProfile\Tasks`). Wine ships no registry seeding,
/// so the defaults are built in. Unknown (but non-NULL) task names fall back to
/// [`Category::Medium`], preserving the old stub's "succeed for any name" contract
/// while still granting a useful boost.
const TASKS: &[(&[u8], Category)] = &[
    (b"Audio", Category::Medium),
    (b"Capture", Category::Medium),
    (b"DisplayPostProcessing", Category::Low),
    (b"Distribution", Category::Medium),
    (b"Games", Category::Medium),
    (b"Low Latency", Category::High),
    (b"Playback", Category::Medium),
    (b"Pro Audio", Category::High),
    (b"Window Manager", Category::High),
];

/// Category for an ASCII task name (case-insensitive); `Medium` for unknown names.
pub fn category_for_ascii(name: &[u8]) -> Category {
    for &(task, cat) in TASKS {
        if task.len() == name.len()
            && task
                .iter()
                .zip(name)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return cat;
        }
    }
    Category::Medium
}

/// Category for a NUL-terminated UTF-16 task name (case-insensitive); `Medium`
/// for unknown names. The defined task names are ASCII, so non-ASCII input never
/// matches and falls through to the default.
///
/// # Safety
/// `name` must be a valid pointer to a NUL-terminated UTF-16 string.
pub unsafe fn category_for_utf16(name: *const u16) -> Category {
    for &(task, cat) in TASKS {
        if unsafe { utf16_eq_ascii_ci(name, task) } {
            return cat;
        }
    }
    Category::Medium
}

/// Case-insensitively compare a NUL-terminated UTF-16 string to an ASCII slice.
///
/// # Safety
/// `name` must point to a NUL-terminated UTF-16 string.
unsafe fn utf16_eq_ascii_ci(name: *const u16, ascii: &[u8]) -> bool {
    let mut i = 0usize;
    loop {
        let w = unsafe { *name.add(i) };
        if i == ascii.len() {
            return w == 0; // matched iff the UTF-16 string also ends here
        }
        if w == 0 || w > 0x7f {
            return false;
        }
        if !(w as u8).eq_ignore_ascii_case(&ascii[i]) {
            return false;
        }
        i += 1;
    }
}

// --- handle table -------------------------------------------------------------

const MAX_SLOTS: usize = 256;
/// Low bits of a handle hold `index + 1` (so a handle is never NULL); the rest
/// hold the slot generation to detect stale handles after a slot is reused.
const INDEX_BITS: u32 = 12;
const INDEX_MASK: usize = (1 << INDEX_BITS) - 1;

#[derive(Clone, Copy)]
struct Slot {
    in_use: bool,
    generation: u16,
    /// Thread priority captured at registration, restored on revert.
    saved_priority: i32,
    category: Category,
}

impl Slot {
    const EMPTY: Slot = Slot {
        in_use: false,
        generation: 0,
        saved_priority: THREAD_PRIORITY_NORMAL,
        category: Category::Medium,
    };
}

fn encode_handle(index: usize, generation: u16) -> usize {
    ((generation as usize) << INDEX_BITS) | (index + 1)
}

/// Decode a handle to `(index, generation)`, or `None` if it is malformed.
fn decode_handle(handle: usize) -> Option<(usize, u16)> {
    let raw = handle & INDEX_MASK;
    if raw == 0 {
        return None;
    }
    let index = raw - 1;
    if index >= MAX_SLOTS {
        return None;
    }
    Some((index, (handle >> INDEX_BITS) as u16))
}

/// The process-wide MMCSS state: the registration table plus the task-index
/// counter handed back through the `TaskIndex` out-parameter.
pub struct Mmcss {
    table: SpinLock<[Slot; MAX_SLOTS]>,
    next_task_index: AtomicU32,
}

impl Mmcss {
    pub const fn new() -> Self {
        Self {
            table: SpinLock::new([Slot::EMPTY; MAX_SLOTS]),
            // Task indices are non-zero; callers pass 0 in and read back an index.
            next_task_index: AtomicU32::new(1),
        }
    }

    /// Register the calling thread under `category`, boosting its priority. On
    /// success returns the handle value and, if `task_index` is `Some`, writes a
    /// fresh non-zero task index. Fails only if the table is full.
    pub fn set_characteristics<O: ThreadOs>(
        &self,
        os: &O,
        category: Category,
        task_index: Option<&mut u32>,
    ) -> Result<usize, u32> {
        let saved = os.get_priority();

        let handle = {
            let mut table = self.table.lock();
            let Some(index) = table.iter().position(|s| !s.in_use) else {
                return Err(ERROR_NO_SYSTEM_RESOURCES);
            };
            let generation = table[index].generation.wrapping_add(1);
            table[index] = Slot {
                in_use: true,
                generation,
                saved_priority: saved,
                category,
            };
            encode_handle(index, generation)
        };

        os.set_priority(category.base_priority());
        if let Some(out) = task_index {
            *out = self.next_task_index.fetch_add(1, Ordering::Relaxed);
        }
        Ok(handle)
    }

    /// AvSetMmThreadPriority: adjust the registered thread's priority within its
    /// task category.
    pub fn set_priority<O: ThreadOs>(
        &self,
        os: &O,
        handle: usize,
        avrt_priority: i32,
    ) -> Result<(), u32> {
        let category = {
            let table = self.table.lock();
            let (index, generation) = decode_handle(handle).ok_or(ERROR_INVALID_HANDLE)?;
            let slot = &table[index];
            if !slot.in_use || slot.generation != generation {
                return Err(ERROR_INVALID_HANDLE);
            }
            slot.category
        };
        os.set_priority(category.priority_for(avrt_priority));
        Ok(())
    }

    /// AvRevertMmThreadCharacteristics: restore the priority saved at
    /// registration and free the slot.
    pub fn revert<O: ThreadOs>(&self, os: &O, handle: usize) -> Result<(), u32> {
        let saved = {
            let mut table = self.table.lock();
            let (index, generation) = decode_handle(handle).ok_or(ERROR_INVALID_HANDLE)?;
            let slot = &mut table[index];
            if !slot.in_use || slot.generation != generation {
                return Err(ERROR_INVALID_HANDLE);
            }
            slot.in_use = false;
            slot.saved_priority
        };
        os.set_priority(saved);
        Ok(())
    }

    /// Whether `handle` names a live registration (AvQuerySystemResponsiveness).
    pub fn is_valid(&self, handle: usize) -> bool {
        let table = self.table.lock();
        match decode_handle(handle) {
            Some((index, generation)) => {
                let slot = &table[index];
                slot.in_use && slot.generation == generation
            }
            None => false,
        }
    }
}

/// Process-wide MMCSS instance backing the real exports.
pub static MMCSS: Mmcss = Mmcss::new();

// --- a minimal spinlock (no_std, no allocator) --------------------------------

/// Tiny spinlock guarding the registration table. Table operations are O(1) and
/// contention is near-zero (a handful of audio/render threads), so a spin is the
/// right trade for a freestanding crate with no OS mutex available.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialized by the `locked` flag.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinGuard { lock: self }
    }
}

pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock for its lifetime.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock for its lifetime.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Mock ThreadOs: records the last priority set and reports a fixed initial
    /// priority, so the table/mapping logic can be exercised on the host.
    struct MockOs {
        current: Cell<i32>,
        last_error: Cell<u32>,
    }
    impl MockOs {
        fn new(initial: i32) -> Self {
            Self {
                current: Cell::new(initial),
                last_error: Cell::new(0),
            }
        }
    }
    impl ThreadOs for MockOs {
        fn get_priority(&self) -> i32 {
            self.current.get()
        }
        fn set_priority(&self, prio: i32) {
            self.current.set(prio);
        }
        fn set_last_error(&self, err: u32) {
            self.last_error.set(err);
        }
    }

    #[test]
    fn known_task_names_map_case_insensitively() {
        assert_eq!(category_for_ascii(b"Audio"), Category::Medium);
        assert_eq!(category_for_ascii(b"audio"), Category::Medium);
        assert_eq!(category_for_ascii(b"PRO AUDIO"), Category::High);
        assert_eq!(category_for_ascii(b"Low Latency"), Category::High);
        assert_eq!(category_for_ascii(b"Games"), Category::Medium);
        assert_eq!(category_for_ascii(b"DisplayPostProcessing"), Category::Low);
    }

    #[test]
    fn unknown_task_name_defaults_to_medium() {
        assert_eq!(category_for_ascii(b"Totally Custom Task"), Category::Medium);
        assert_eq!(category_for_ascii(b""), Category::Medium);
        // a prefix of a known name must not match
        assert_eq!(category_for_ascii(b"Aud"), Category::Medium);
    }

    fn to_utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn utf16_lookup_matches_ascii_lookup() {
        for name in ["Audio", "pro audio", "Low Latency", "weird", ""] {
            let w = to_utf16(name);
            let via_utf16 = unsafe { category_for_utf16(w.as_ptr()) };
            assert_eq!(via_utf16, category_for_ascii(name.as_bytes()), "{name}");
        }
    }

    #[test]
    fn category_priorities_are_monotonic() {
        assert!(Category::Low.base_priority() < Category::Medium.base_priority());
        assert!(Category::Medium.base_priority() < Category::High.base_priority());
        assert_eq!(Category::High.base_priority(), THREAD_PRIORITY_TIME_CRITICAL);
    }

    #[test]
    fn avrt_priority_adjustment_within_category() {
        let c = Category::Medium;
        assert_eq!(c.priority_for(AVRT_PRIORITY_CRITICAL), THREAD_PRIORITY_TIME_CRITICAL);
        assert_eq!(c.priority_for(AVRT_PRIORITY_HIGH), THREAD_PRIORITY_HIGHEST);
        assert_eq!(c.priority_for(AVRT_PRIORITY_NORMAL), c.base_priority());
        assert_eq!(c.priority_for(AVRT_PRIORITY_LOW), THREAD_PRIORITY_ABOVE_NORMAL);
        assert_eq!(c.priority_for(AVRT_PRIORITY_VERYLOW), THREAD_PRIORITY_NORMAL);
    }

    #[test]
    fn set_boosts_and_revert_restores() {
        let mmcss = Mmcss::new();
        let os = MockOs::new(THREAD_PRIORITY_NORMAL);

        let mut index = 0u32;
        let handle = mmcss
            .set_characteristics(&os, Category::High, Some(&mut index))
            .expect("registration");
        assert_ne!(handle & INDEX_MASK, 0, "handle never encodes to NULL");
        assert_ne!(index, 0, "task index is non-zero");
        assert_eq!(os.get_priority(), THREAD_PRIORITY_TIME_CRITICAL, "boosted");

        mmcss.revert(&os, handle).expect("revert");
        assert_eq!(os.get_priority(), THREAD_PRIORITY_NORMAL, "restored");
    }

    #[test]
    fn stale_handle_is_rejected_after_revert() {
        let mmcss = Mmcss::new();
        let os = MockOs::new(0);
        let handle = mmcss.set_characteristics(&os, Category::Medium, None).unwrap();
        assert!(mmcss.is_valid(handle));
        mmcss.revert(&os, handle).unwrap();
        assert!(!mmcss.is_valid(handle));
        assert_eq!(mmcss.revert(&os, handle), Err(ERROR_INVALID_HANDLE));
        assert_eq!(
            mmcss.set_priority(&os, handle, AVRT_PRIORITY_HIGH),
            Err(ERROR_INVALID_HANDLE)
        );
    }

    #[test]
    fn reused_slot_invalidates_old_generation() {
        let mmcss = Mmcss::new();
        let os = MockOs::new(0);
        let h1 = mmcss.set_characteristics(&os, Category::Medium, None).unwrap();
        mmcss.revert(&os, h1).unwrap();
        // Next registration reuses slot 0 with a bumped generation.
        let h2 = mmcss.set_characteristics(&os, Category::Medium, None).unwrap();
        assert_eq!(h1 & INDEX_MASK, h2 & INDEX_MASK, "same slot index reused");
        assert_ne!(h1, h2, "generation differs");
        assert!(!mmcss.is_valid(h1));
        assert!(mmcss.is_valid(h2));
    }

    #[test]
    fn malformed_handles_are_rejected() {
        assert_eq!(decode_handle(0), None);
        assert!(decode_handle(encode_handle(0, 7)).is_some());
        let mmcss = Mmcss::new();
        assert!(!mmcss.is_valid(0));
    }

    #[test]
    fn table_full_reports_no_resources() {
        let mmcss = Mmcss::new();
        let os = MockOs::new(0);
        let mut handles = Vec::new();
        for _ in 0..MAX_SLOTS {
            handles.push(mmcss.set_characteristics(&os, Category::Medium, None).unwrap());
        }
        assert_eq!(
            mmcss.set_characteristics(&os, Category::Medium, None),
            Err(ERROR_NO_SYSTEM_RESOURCES)
        );
        // freeing one makes room again
        mmcss.revert(&os, handles[0]).unwrap();
        assert!(mmcss.set_characteristics(&os, Category::Medium, None).is_ok());
    }
}
