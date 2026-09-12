use std::sync::Mutex;

/// A small pool of reusable byte buffers.
///
/// Every logged `Packet` needs its own owned copy of the payload bytes
/// (the original chunk lives in a stack buffer that gets overwritten on the
/// next read). Under heavy traffic with payload logging on, that's a lot of
/// fresh `Vec` allocations — one per chunk, on every pipeline. A
/// `BufferPool` hands out pre-sized, already-allocated buffers and takes
/// them back instead, so steady-state logging doesn't keep pressuring the
/// allocator.
///
/// This is a size hint, not a hard cap: `acquire()` still allocates when the
/// pool is empty, it just reuses what's already been freed first.
pub struct BufferPool {
    buf_size: usize,
    max_idle: usize,
    free: Mutex<Vec<Vec<u8>>>,
}

impl BufferPool {
    pub fn new(buf_size: usize, max_idle: usize) -> Self {
        Self {
            buf_size,
            max_idle,
            free: Mutex::new(Vec::new()),
        }
    }

    /// Borrow a buffer, empty and ready to be filled.
    pub fn acquire(&self) -> Vec<u8> {
        let mut free = self.free.lock().unwrap();
        free.pop().unwrap_or_else(|| Vec::with_capacity(self.buf_size))
    }

    /// Return a buffer for reuse. Dropped instead of pooled once `max_idle`
    /// buffers are already parked, so a traffic spike doesn't leave the pool
    /// permanently holding megabytes nothing is using.
    pub fn release(&self, mut buf: Vec<u8>) {
        buf.clear();
        let mut free = self.free.lock().unwrap();
        if free.len() < self.max_idle {
            free.push(buf);
        }
    }
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new(4096, 64)
    }
}
