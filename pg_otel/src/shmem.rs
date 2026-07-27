// SPDX-License-Identifier: MIT

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
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

pub trait Queue {
    fn push(&self, data: &[u8]) -> Result<(), PushError>;
    fn pop(&self) -> Option<Vec<u8>>;
    fn notify(&self);

    fn get_dropped(&self) -> usize;
    fn set_latch(&self, latch: *mut pg_sys::Latch);
}

/// [QueueAtomic] is a thread-safe [Queue] that uses atomics.
struct QueueAtomic {
    header: &'static QueueAtomicHeader,
    payload: *mut u8,
    capacity: usize,
}

/// The atomic fields of [QueueAtomic]. Derives [Default] to easily reset all fields to zero.
#[derive(Default)]
#[repr(C)]
struct QueueAtomicHeader {
    head: AtomicUsize,
    tail: AtomicUsize,
    dropped: AtomicUsize,
    latch: AtomicPtr<pg_sys::Latch>,
}

// SAFETY: QueueAtomic access is process-safe and atomic across shared memory segments.
unsafe impl Send for QueueAtomic {}
unsafe impl Sync for QueueAtomic {}

impl QueueAtomic {
    /// Create a new queue allocated in Rust.
    #[cfg(test)]
    fn with_capacity(capacity: usize) -> Self {
        let align = mem::align_of::<QueueAtomicHeader>();
        let size = capacity + mem::size_of::<QueueAtomicHeader>();

        let layout = std::alloc::Layout::from_size_align(size, align).unwrap();
        unsafe { Self::create(std::alloc::alloc(layout), size) }
    }

    /// Initialize a new queue.
    ///
    /// # Safety
    ///
    /// The memory at `raw_ptr` must be at least `total_size` bytes.
    unsafe fn create(raw_ptr: *mut u8, total_size: usize) -> Self {
        assert!(raw_ptr.is_aligned() && !raw_ptr.is_null());
        assert!(total_size > mem::size_of::<QueueAtomicHeader>());

        unsafe {
            ptr::write_bytes(raw_ptr, 0, total_size);
            ptr::write(
                raw_ptr as *mut QueueAtomicHeader,
                QueueAtomicHeader::default(),
            );
            QueueAtomic::attach(raw_ptr, total_size)
        }
    }

    /// Attach to an existing queue.
    ///
    /// # Safety
    ///
    /// The memory at `raw_ptr` must be an initialized [QueueAtomic] occupying `total_size` bytes.
    unsafe fn attach(raw_ptr: *mut u8, total_size: usize) -> Self {
        let header_size = mem::size_of::<QueueAtomicHeader>();
        assert!(raw_ptr.is_aligned() && !raw_ptr.is_null());
        assert!(total_size > header_size);

        let capacity = total_size - header_size;
        let header = unsafe { &*(raw_ptr as *const QueueAtomicHeader) };
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

    fn increment_dropped(&self) {
        self.header.dropped.fetch_add(1, Ordering::Relaxed);
    }
    fn get_latch(&self) -> *mut pg_sys::Latch {
        self.header.latch.load(Ordering::Acquire)
    }
}

impl Queue for QueueAtomic {
    fn get_dropped(&self) -> usize {
        self.header.dropped.load(Ordering::Acquire)
    }
    fn set_latch(&self, v: *mut pg_sys::Latch) {
        self.header.latch.store(v, Ordering::Release);
    }

    /// Push a message into the queue.
    fn push(&self, data: &[u8]) -> Result<(), PushError> {
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

    /// Read the next available message from the queue.
    fn pop(&self) -> Option<Vec<u8>> {
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

    /// Notify the queue consumer.
    fn notify(&self) {
        let latch_ptr = self.get_latch();
        if !latch_ptr.is_null() {
            unsafe { pg_sys::SetLatch(latch_ptr) };
        }
    }
}

/// [QueueSharedMemory] is a thread-safe [Queue] that resides in Postgres shared memory. Create a
/// static instance using [QueueSharedMemory::new], then pass it to [pgrx::pg_shmem_init!] to hook
/// it into Postgres shared memory callbacks.
pub struct QueueSharedMemory {
    name: &'static ffi::CStr,
    size: std::sync::LazyLock<usize>,
    inner: std::sync::OnceLock<QueueAtomic>,
}

impl pgrx::PgSharedMemoryInitialization for QueueSharedMemory {
    type Value = ();

    unsafe fn on_shmem_request(&'static self) {
        unsafe { pg_sys::RequestAddinShmemSpace(*self.size) };
    }

    unsafe fn on_shmem_startup(&'static self, _value: Self::Value) {
        self.queue();
    }
}

impl QueueSharedMemory {
    pub const fn new(name: &'static ffi::CStr, size: fn() -> usize) -> Self {
        Self {
            name,
            size: std::sync::LazyLock::new(size),
            inner: std::sync::OnceLock::new(),
        }
    }

    fn queue(&self) -> &QueueAtomic {
        self.inner.get_or_init(|| {
            let mut found = false;
            let size = *self.size;
            let ptr = unsafe { pg_sys::ShmemInitStruct(self.name.as_ptr(), size, &mut found) };
            if found {
                // SAFETY: Postgres returned a pointer to `size` bytes.
                unsafe { QueueAtomic::attach(ptr.cast(), size) }
            } else {
                // SAFETY: Postgres returned a pointer to `size` bytes.
                unsafe { QueueAtomic::create(ptr.cast(), size) }
            }
        })
    }
}

impl Queue for QueueSharedMemory {
    fn push(&self, data: &[u8]) -> Result<(), PushError> {
        self.queue().push(data)
    }

    fn pop(&self) -> Option<Vec<u8>> {
        self.queue().pop()
    }

    fn notify(&self) {
        self.queue().notify()
    }

    fn get_dropped(&self) -> usize {
        self.queue().get_dropped()
    }

    fn set_latch(&self, latch: *mut pg_sys::Latch) {
        self.queue().set_latch(latch)
    }
}

/// Header prepended to every item inside the [QueueAtomic] ring buffer.
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
    fn test_queue_atomic_basic_push_pop() {
        let queue = QueueAtomic::with_capacity(88);

        assert_eq!(queue.push(b"hello"), Ok(()));
        assert_eq!(queue.push(b"world"), Ok(()));

        assert_eq!(queue.pop(), Some(b"hello".to_vec()));
        assert_eq!(queue.pop(), Some(b"world".to_vec()));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn test_queue_atomic_wrap_and_padding() {
        let queue = QueueAtomic::with_capacity(40);

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
    fn test_queue_atomic_concurrency() {
        let queue = Arc::new(QueueAtomic::with_capacity(984));

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
