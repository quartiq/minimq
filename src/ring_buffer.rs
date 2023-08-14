pub(crate) struct RingBuffer<'a> {
    // Underlying storage data.
    data: &'a mut [u8],

    // Pointer to the first index of data, inclusive
    head: usize,

    // The number of elements stored.
    count: usize,
}

impl<'a> RingBuffer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            data: buffer,
            head: 0,
            count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.count = 0;
    }

    pub fn push_slice<'b>(&mut self, data: &'b [u8]) -> Option<&'b [u8]> {
        if self.data.len() - self.count < data.len() {
            return Some(data);
        }

        let tail = (self.head + self.count).rem_euclid(self.data.len());

        if tail + data.len() <= self.data.len() {
            // Easy mode: Copy directly into tail
            self.data[tail..][..data.len()].copy_from_slice(data);
        } else {
            // Split the buffer, writing the first N bytes to the tail, remainder to start of buffer
            // (wrap)
            let tail_len = self.data.len() - tail;
            self.data[tail..][..tail_len].copy_from_slice(&data[..tail_len]);

            let remainder = data.len() - tail_len;
            self.data[..remainder].copy_from_slice(&data[tail_len..])
        }

        self.count += data.len();

        None
    }

    pub fn slices_mut(&mut self, offset: usize, max_len: usize) -> (&mut [u8], &mut [u8]) {
        let start = (self.head + offset).rem_euclid(self.data.len());
        let len = max_len.min(self.count);

        let end = (start + len).rem_euclid(self.data.len());

        // When length is zero, the start and end indices are indentical because of wrap. As
        // such, we have to check for this condition.
        if len == 0 {
            (&mut [], &mut [])
        } else if start < end {
            (&mut self.data[start..end], &mut [])
        } else {
            let (front, back) = self.data.split_at_mut(end);
            (&mut back[(start - end)..], &mut front[..end])
        }
    }

    pub fn pop(&mut self, len: usize) {
        let len = len.min(self.count);

        self.head = (self.head + len).rem_euclid(self.data.len());
        self.count -= len;
    }

    /// Determine the number of remaining bytes in the buffer
    pub fn remainder(&self) -> usize {
        self.data.len() - self.count
    }

    pub fn len(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::RingBuffer;

    #[test]
    fn empty() {
        let mut data = [0; 256];
        let mut buffer = RingBuffer::new(&mut data);

        assert!(buffer.len() == 0);
        assert!(buffer.remainder() == 256);

        // No data in the head or tail
        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(head.is_empty());
        assert!(tail.is_empty());
    }

    #[test]
    fn push() {
        let mut backing = [0; 256];
        let mut buffer = RingBuffer::new(&mut backing);

        let data = [1, 2, 3];
        assert!(buffer.push_slice(&data).is_none());

        assert!(buffer.len() == 3);
        assert!(buffer.remainder() == 253);

        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(head == [1, 2, 3]);
        assert!(tail.is_empty());

        assert!(buffer.push_slice(&data).is_none());
        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(head == [1, 2, 3, 1, 2, 3]);
        assert!(tail.is_empty());
    }

    #[test]
    fn push_overflow() {
        let mut backing = [0; 256];
        let mut buffer = RingBuffer::new(&mut backing);

        let data = [0; 256];
        assert!(buffer.push_slice(&data).is_none());

        assert!(buffer.len() == 256);
        assert!(buffer.remainder() == 0);

        assert!(buffer.push_slice(&[1]).is_some());
        assert!(buffer.len() == 256);
        assert!(buffer.remainder() == 0);

        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(head == [0; 256]);
        assert!(tail.is_empty());
    }

    #[test]
    fn pop() {
        let mut backing = [0; 256];
        let mut buffer = RingBuffer::new(&mut backing);

        let data = [1, 2, 3];
        assert!(buffer.push_slice(&data).is_none());

        assert!(buffer.len() == 3);
        buffer.pop(1);
        assert!(buffer.len() == 2);
        buffer.pop(1);
        assert!(buffer.len() == 1);

        buffer.pop(1);
        assert!(buffer.len() == 0);
    }

    #[test]
    fn pop_underflow() {
        let mut backing = [0; 256];
        let mut buffer = RingBuffer::new(&mut backing);

        let data = [1, 2, 3];
        assert!(buffer.push_slice(&data).is_none());

        buffer.pop(1);
        assert!(buffer.len() == 2);

        buffer.pop(10);
        assert!(buffer.len() == 0);
    }

    #[test]
    fn pop_wrap() {
        let mut backing = [0; 256];
        let mut buffer = RingBuffer::new(&mut backing);

        // Fill the buffer
        let data = [0; 256];
        assert!(buffer.push_slice(&data).is_none());

        // Pop a few bytes to cause the buffer to wrap.
        buffer.pop(10);

        assert!(buffer.push_slice(&[1, 2, 3]).is_none());

        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(head == [0; 246]);
        assert!(tail == [1, 2, 3]);

        // Pop over the boundary
        buffer.pop(247);
        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(head == [2, 3]);
        assert!(tail.is_empty());
    }

    #[test]
    fn push_wrap() {
        let mut backing = [0; 256];
        let mut buffer = RingBuffer::new(&mut backing);

        // Fill the buffer
        let data = [0; 255];
        assert!(buffer.push_slice(&data).is_none());

        // Pop a few bytes from the front to free up space
        buffer.pop(10);

        // Push across the boundary
        assert!(buffer.push_slice(&[1, 2, 3]).is_none());
        let (head, tail) = buffer.slices_mut(0, buffer.len());
        assert!(*head.last().unwrap() == 1);
        assert!(tail == [2, 3]);
    }
}
