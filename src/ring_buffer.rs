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

    pub fn push_slice(&mut self, data: &[u8]) -> Result<(), ()> {
        if self.data.len() - self.count < data.len() {
            return Err(());
        }

        let tail = (self.head + self.count).rem_euclid(self.data.len());

        if tail + self.data.len() <= data.len() {
            // Easy mode: Copy directly into tail
            self.data[tail..][..data.len()].copy_from_slice(data);
        } else {
            // Split the buffer, writing the first N bytes to the tail, remainder to start of buffer
            // (wrap)
            let tail_len = self.data.len() - tail;
            self.data[tail..].copy_from_slice(data[..tail_len]);

            let remainder = data.len() - tail_len;
            self.data[..remainder].copy_from_slice(data[tail_len..])
        }

        self.count += data.len();

        Ok(())
    }

    pub fn pop(&mut self, len: usize) -> Result<(), ()> {
        if len > self.count {
            return Err(());
        }

        self.head = (self.head + len).rem_euclid(self.data.len());
        self.count -= len;

        Ok(())
    }

    pub fn probe_header(&mut self) -> Result<(), ()> {
        // TODO: Deserialize the headers of the MQTT packet from the buffer
    }
}
