use crate::de::received_packet::ReceivedPacket;

pub(crate) struct RingBuffer<'a> {
    // Underlying storage data.
    data: &'a mut [u8],

    // Pointer to the first index of data, inclusive
    head: usize,

    // The number of elements stored.
    count: usize,
}

pub(crate) struct MqttHeader {
    pub len: usize,
    pub packet_id: u16,
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

        // Set DUP = 1 (bit 3). If this packet is ever read it's just because we want to resend it
        self.data[tail] |= 1 << 3;

        self.count += data.len();

        None
    }

    pub fn slices(&self, offset: usize, len: usize) -> (&[u8], &[u8]) {
        let start = (self.head + offset).rem_euclid(self.data.len());

        let end = (start + len).rem_euclid(self.data.len());

        if start < end {
            (&self.data[start..end], &[])
        } else {
            (&self.data[start..], &self.data[..end])
        }
    }

    pub fn pop(&mut self, len: usize) {
        let len = len.max(self.count);

        self.head = (self.head + len).rem_euclid(self.data.len());
        self.count -= len;
    }

    pub fn probe_header(&mut self, index: usize) -> Option<(usize, MqttHeader)> {
        let mut offset = 0;

        if index > 0 {
            for _ in 0..index - 1 {
                let (head, tail) = self.slices(offset, self.count - offset);
                let Ok(packet) = ReceivedPacket::from_split_buffer(head, tail) else {
                    return None;
                };

                offset += packet.len();
            }
        }

        let (head, tail) = self.slices(offset, self.count - offset);
        let Ok(packet) = ReceivedPacket::from_split_buffer(head, tail) else {
            return None;
        };

        Some((
            offset,
            MqttHeader {
                len: packet.len(),
                packet_id: packet.id().unwrap(),
            },
        ))
    }

    /// Determine the number of remaining bytes in the buffer
    pub fn remainder(&self) -> usize {
        self.data.len() - self.count
    }

    pub fn len(&self) -> usize {
        self.count
    }
}
