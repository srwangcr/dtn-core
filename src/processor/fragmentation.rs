//! BPv7 Bundle Fragmentation & Reassembly Engine (RFC 9171 Section 5.8)
//! Cero alocaciones dinámicas (no_std).

/// Representa el estado de reensamblado de un payload fragmentado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleFragmentHeader {
    pub is_fragment: bool,
    pub fragment_offset: u64,
    pub total_app_data_len: u64,
}

/// Slot estático para mantener partes de un bundle en proceso de reensamblado.
pub struct ReassemblySlot<const BUF_SIZE: usize> {
    pub payload_buffer: [u8; BUF_SIZE],
    pub bytes_received: usize,
    pub total_expected_len: u64,
    pub active: bool,
}

impl<const BUF_SIZE: usize> ReassemblySlot<BUF_SIZE> {
    pub const fn new() -> Self {
        Self {
            payload_buffer: [0u8; BUF_SIZE],
            bytes_received: 0,
            total_expected_len: 0,
            active: false,
        }
    }

    /// Borra el estado del slot para reutilización.
    pub fn reset(&mut self) {
        self.bytes_received = 0;
        self.total_expected_len = 0;
        self.active = false;
    }

    /// Inserta un fragmento de payload en el offset correspondiente.
    pub fn insert_fragment(
        &mut self,
        offset: u64,
        total_len: u64,
        fragment_data: &[u8],
    ) -> Result<bool, &'static str> {
        let offset = offset as usize;
        let frag_len = fragment_data.len();

        if offset + frag_len > BUF_SIZE {
            return Err("fragment exceeds reassembly buffer capacity");
        }

        if !self.active {
            self.total_expected_len = total_len;
            self.active = true;
        } else if self.total_expected_len != total_len {
            return Err("fragment total length mismatch");
        }

        // Copia zero-alloc dentro del buffer estático de reensamblado
        self.payload_buffer[offset..offset + frag_len].copy_from_slice(fragment_data);
        self.bytes_received += frag_len;

        // Retorna true si el bundle completo fue reensamblado
        Ok(self.bytes_received as u64 >= self.total_expected_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reassembly_two_fragments() {
        let mut slot = ReassemblySlot::<1024>::new();
        
        let frag1 = b"Hello, ";
        let frag2 = b"World!";
        let total_len = (frag1.len() + frag2.len()) as u64;

        // Ingesta fragmento 1 (offset 0)
        let complete1 = slot.insert_fragment(0, total_len, frag1).unwrap();
        assert!(!complete1);

        // Ingesta fragmento 2 (offset 7)
        let complete2 = slot.insert_fragment(frag1.len() as u64, total_len, frag2).unwrap();
        assert!(complete2);

        assert_eq!(&slot.payload_buffer[..total_len as usize], b"Hello, World!");
    }
}
