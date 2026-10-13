//! MSB-first bit writer.

#[derive(Default)]
pub struct BitWriter {
    pub bytes: Vec<u8>,
    acc: u64,
    nacc: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the low `n` bits of `value` (n <= 32), most significant first.
    #[inline]
    pub fn put(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        debug_assert!(n == 32 || value >> n == 0, "value {value} does not fit in {n} bits");
        self.acc = (self.acc << n) | u64::from(value);
        self.nacc += n;
        while self.nacc >= 8 {
            self.nacc -= 8;
            self.bytes.push((self.acc >> self.nacc) as u8);
        }
        self.acc &= (1u64 << self.nacc) - 1;
    }

    /// Total number of bits written so far.
    pub fn bit_len(&self) -> usize {
        self.bytes.len() * 8 + self.nacc as usize
    }

    /// Pads with zero bits to a byte boundary and returns the bytes.
    pub fn into_bytes(mut self) -> Vec<u8> {
        if self.nacc > 0 {
            let pad = 8 - self.nacc;
            self.put(0, pad);
        }
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_msb_first() {
        let mut w = BitWriter::new();
        w.put(0b101, 3);
        w.put(0b11110, 5);
        w.put(0xABCD, 16);
        w.put(1, 1);
        assert_eq!(w.bit_len(), 25);
        assert_eq!(w.into_bytes(), vec![0b1011_1110, 0xAB, 0xCD, 0x80]);
    }
}
