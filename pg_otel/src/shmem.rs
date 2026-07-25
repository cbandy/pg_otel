// SPDX-License-Identifier: MIT

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use core::{ffi, mem, ptr};
use pgrx::pg_sys;

#[inline]
const fn align_upward(size: usize) -> usize {
    const ALIGNMENT: usize = mem::align_of::<usize>();
    (size + (ALIGNMENT - 1)) & !(ALIGNMENT - 1)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PushError {
    Full,
    TooLarge,
}

pub struct Queue {
    header: &'static QueueHeader,
    payload: *mut u8,
    capacity: usize,
}

// QueueBuilder populates the `OnceLock` with a `Queue` of the requested size in shared memory.
pub struct QueueBuilder {
    pub queue: &'static std::sync::OnceLock<Queue>,
    pub name: &'static ffi::CStr,
    pub size: fn() -> usize,
}
impl pgrx::PgSharedMemoryInitialization for QueueBuilder {
    type Value = ();

    unsafe fn on_shmem_request(&'static self) {
        let size = (self.size)();
        unsafe { pg_sys::RequestAddinShmemSpace(size) };
    }

    unsafe fn on_shmem_startup(&'static self, _value: Self::Value) {
        self.queue.get_or_init(|| {
            let mut found = false;
            let size = (self.size)();
            let ptr = unsafe { pg_sys::ShmemInitStruct(self.name.as_ptr(), size, &mut found) };
            if found {
                unsafe { Queue::attach(ptr.cast(), size) }
            } else {
                unsafe { Queue::create(ptr.cast(), size) }
            }
        });
    }
}

/// The shared memory header. Derives Default to cleanly reset all fields to zero.
#[derive(Default)]
#[repr(C)]
struct QueueHeader {
    head: AtomicUsize,
    tail: AtomicUsize,
    dropped: AtomicUsize,
    latch: AtomicPtr<pg_sys::Latch>,
    sleeping: AtomicBool,
}

/// Header prepended to every item inside the ring buffer.
#[repr(C)]
struct SlotHeader {
    status: AtomicU32,
    len: u32,
}

#[repr(u32)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum SlotStatus {
    Free = 0,
    Committed = 1,
    Padding = 2,
}

impl SlotHeader {
    /// Read the commit status atomically with Acquire ordering.
    fn status(&self) -> Result<SlotStatus, u32> {
        let raw = self.status.load(Ordering::Acquire);
        match raw {
            0 => Ok(SlotStatus::Free),
            1 => Ok(SlotStatus::Committed),
            2 => Ok(SlotStatus::Padding),
            _ => Err(raw),
        }
    }

    /// Write the commit status atomically with Release ordering.
    fn set_status(&self, status: SlotStatus) {
        self.status.store(status as u32, Ordering::Release);
    }
}

// SAFETY: Queue access is process-safe and atomic across shared memory segments.
unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    /// Initialize a new shared memory queue; called once by Postmaster at startup.
    unsafe fn create(raw_ptr: *mut u8, total_size: usize) -> Self {
        assert!(raw_ptr.is_aligned());
        assert!(total_size > mem::size_of::<QueueHeader>());

        unsafe {
            ptr::write_bytes(raw_ptr, 0, total_size);
            ptr::write(raw_ptr as *mut QueueHeader, QueueHeader::default());
            Queue::attach(raw_ptr, total_size)
        }
    }

    /// Attach to an existing shared memory queue; called by backends and background workers.
    unsafe fn attach(raw_ptr: *mut u8, total_size: usize) -> Self {
        let header_size = mem::size_of::<QueueHeader>();
        assert!(total_size > header_size);

        let capacity = total_size - header_size;
        let header = unsafe { &*(raw_ptr as *const QueueHeader) };
        let payload = unsafe { raw_ptr.add(header_size) };

        Self {
            header,
            payload,
            capacity,
        }
    }

    fn get_head(&self) -> usize {
        self.header.head.load(Ordering::Acquire)
    }
    fn set_head(&self, v: usize) {
        self.header.head.store(v, Ordering::Release);
    }

    fn get_tail(&self) -> usize {
        self.header.tail.load(Ordering::Acquire)
    }
    fn try_advance_tail(&self, current: usize, next: usize) -> bool {
        self.header
            .tail
            .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn get_dropped(&self) -> usize {
        self.header.dropped.load(Ordering::Acquire)
    }
    fn increment_dropped(&self) {
        self.header.dropped.fetch_add(1, Ordering::Relaxed);
    }

    fn get_latch(&self) -> *mut pg_sys::Latch {
        self.header.latch.load(Ordering::Acquire)
    }
    pub fn set_latch(&self, v: *mut pg_sys::Latch) {
        self.header.latch.store(v, Ordering::Release);
    }

    fn is_waiting(&self) -> bool {
        self.header.sleeping.load(Ordering::Acquire)
    }
    pub fn set_waiting(&self, v: bool) {
        self.header.sleeping.store(v, Ordering::Release);
    }

    /// Notify the background consumer process if it is currently waiting.
    pub fn notify(&self) {
        if self.is_waiting() {
            let latch_ptr = self.get_latch();
            if !latch_ptr.is_null() {
                unsafe { pg_sys::SetLatch(latch_ptr) };
            }
        }
    }

    /// Push telemetry bytes to the queue.
    pub fn push(&self, data: &[u8]) -> Result<(), PushError> {
        let data_len = data.len();
        let header_size = mem::size_of::<SlotHeader>();

        // Round the required space up to keep atomic variables aligned; required on ARM and
        // necessary on x86 to perform well. Use checked arithmetic to recognize overflow.
        let Some(required_space) = header_size.checked_add(data_len).map(align_upward) else {
            self.increment_dropped();
            return Err(PushError::TooLarge);
        };
        if required_space > self.capacity {
            self.increment_dropped();
            return Err(PushError::TooLarge);
        }

        // Loop to handle lock-free CAS retries when multiple producers write concurrently.
        loop {
            let head = self.get_head();
            let tail = self.get_tail();
            let write_offset = tail % self.capacity;

            // If the write would cross (wrap around) the end of the circular buffer, try to write a
            // special padding block, then retry.
            if write_offset.saturating_add(required_space) > self.capacity {
                let padding_size = self.capacity - write_offset;
                let next_tail = tail.wrapping_add(padding_size);

                if self.try_advance_tail(tail, next_tail) {
                    // SAFETY: The offset is within the queue allocation and aligned.
                    let slot_ptr = unsafe { self.payload.add(write_offset) };
                    let slot = unsafe { &*(slot_ptr as *const SlotHeader) };
                    slot.set_status(SlotStatus::Padding);
                }

                continue;
            }

            // If the write would exceed the capacity of the queue, drop the item and indicate the
            // queue is full. This is a "drop newest" response to backpressure.
            let next_tail = tail.wrapping_add(required_space);
            if next_tail.wrapping_sub(head) > self.capacity {
                self.increment_dropped();
                return Err(PushError::Full);
            }

            // Try to reserve the slot. When successful, write the length, data, and status (in that
            // order) to atomically indicate the slot is ready.
            if self.try_advance_tail(tail, next_tail) {
                // SAFETY: The offset and header are within the queue allocation.
                let slot_ptr = unsafe { self.payload.add(write_offset) };
                let payload_ptr = unsafe { slot_ptr.add(header_size) };

                // SAFETY: Slot pointers are aligned.
                let slot = unsafe { &mut *(slot_ptr as *mut SlotHeader) };
                slot.len = data_len as u32;
                unsafe { ptr::copy_nonoverlapping(data.as_ptr(), payload_ptr, data_len) };
                slot.set_status(SlotStatus::Committed);

                return Ok(());
            }
        }
    }

    /// Read the next available telemetry message from the queue.
    pub fn pop(&self) -> Option<Vec<u8>> {
        let header_size = mem::size_of::<SlotHeader>();

        // Loop to handle retries during wrap around.
        loop {
            let head = self.get_head();
            let tail = self.get_tail();

            // Queue is empty.
            if head == tail {
                return None;
            }

            let read_offset = head % self.capacity;
            let slot = unsafe { &*(self.payload.add(read_offset) as *const SlotHeader) };
            match slot.status().ok()? {
                // Producer has reserved space but has not finished writing.
                SlotStatus::Free => {
                    return None;
                }
                // A padding block indicates wrap around. Skip it and retry.
                SlotStatus::Padding => {
                    let padding_size = self.capacity - read_offset;
                    let next_head = head.wrapping_add(padding_size);
                    self.set_head(next_head);
                    continue;
                }
                // Producer has finished writing.
                SlotStatus::Committed => {
                    let data_len = slot.len as usize;
                    let mut data = vec![0u8; data_len];

                    // Copy the payload.
                    let payload_ptr = unsafe { self.payload.add(read_offset + header_size) };
                    unsafe { ptr::copy_nonoverlapping(payload_ptr, data.as_mut_ptr(), data_len) };

                    // Free the slot.
                    slot.set_status(SlotStatus::Free);

                    // Advance the head pointer. Round the required space up to keep atomic
                    // variables aligned.
                    let required_space = align_upward(header_size + data_len);
                    let next_head = head.wrapping_add(required_space);
                    self.set_head(next_head);

                    return Some(data);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_align_upward() {
        let alignment = mem::align_of::<usize>();

        // aligned values do not change.
        assert_eq!(align_upward(0), 0);
        assert_eq!(align_upward(alignment), alignment);
        assert_eq!(align_upward(2 * alignment), 2 * alignment);
        assert_eq!(align_upward(10 * alignment), 10 * alignment);

        // rounds upward to the next aligned.
        assert_eq!(align_upward(1), alignment);
        assert_eq!(align_upward(alignment - 1), alignment);
        assert_eq!(align_upward(alignment + 1), 2 * alignment);

        for size in 0..100 {
            let aligned = align_upward(size);
            assert_eq!(aligned % alignment, 0, "always aligned");
            assert!(aligned >= size, "always upward");
            assert!(aligned - size < alignment, "always one step");
        }
    }

    #[test]
    fn test_basic_push_pop() {
        let mut mem = vec![0u8; 128]; // QueueHeader is 40 bytes + 88 bytes capacity
        let queue = unsafe { Queue::create(mem.as_mut_ptr(), 128) };

        assert_eq!(queue.push(b"hello"), Ok(()));
        assert_eq!(queue.push(b"world"), Ok(()));

        assert_eq!(queue.pop(), Some(b"hello".to_vec()));
        assert_eq!(queue.pop(), Some(b"world".to_vec()));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn test_wrap_and_padding() {
        let mut mem = vec![0u8; 80]; // 40 bytes header + 40 bytes capacity
        let queue = unsafe { Queue::create(mem.as_mut_ptr(), 80) };

        // capacity is 40.
        // Message of 8 bytes requires: 8 (header) + 8 (data) = 16 bytes.
        assert_eq!(queue.push(b"12345678"), Ok(())); // occupies offset 0..16

        // Dequeue it to free up head space
        assert_eq!(queue.pop(), Some(b"12345678".to_vec()));

        // Now head is at 16, tail is at 16.
        // We push another 16-byte message (16 bytes space required: 8 header + 8 data).
        // Since only 8 bytes remain to the end of the 24-byte capacity buffer (24 - 16 = 8),
        // it must wrap around and write at the beginning using padding!
        assert_eq!(queue.push(b"abcdefgh"), Ok(()));

        assert_eq!(queue.pop(), Some(b"abcdefgh".to_vec()));
    }

    #[test]
    fn test_concurrency() {
        let mut mem = vec![0u8; 1024]; // 40 bytes header + 984 bytes capacity
        let queue = Arc::new(unsafe { Queue::create(mem.as_mut_ptr(), 1024) });

        let q_producer = queue.clone();
        let writer = thread::spawn(move || {
            for i in 0..50 {
                let msg = format!("msg-{}", i);
                while q_producer.push(msg.as_bytes()).is_err() {
                    thread::yield_now();
                }
            }
        });

        let mut received = Vec::new();
        let q_consumer = queue.clone();
        let reader = thread::spawn(move || {
            while received.len() < 50 {
                if let Some(msg) = q_consumer.pop() {
                    received.push(String::from_utf8(msg).unwrap());
                } else {
                    thread::yield_now();
                }
            }
            received
        });

        writer.join().unwrap();
        let results = reader.join().unwrap();

        for i in 0..50 {
            assert_eq!(results[i], format!("msg-{}", i));
        }
    }
}
