//! Fase 3: Interval Tree O(log N) optimizado con memmove vectorial para CGR y EAT

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContactInterval {
    pub start_time: u64,
    pub end_time: u64,
    pub target_node_id: u32,
}

impl ContactInterval {
    pub const fn new(start_time: u64, end_time: u64, target_node_id: u32) -> Self {
        Self { start_time, end_time, target_node_id }
    }
}

pub struct CgrIntervalTree<const CAP: usize> {
    nodes: [ContactInterval; CAP],
    len: usize,
}

impl<const CAP: usize> CgrIntervalTree<CAP> {
    pub const fn new() -> Self {
        const SENTINEL: ContactInterval = ContactInterval { start_time: u64::MAX, end_time: 0, target_node_id: 0 };
        Self { nodes: [SENTINEL; CAP], len: 0 }
    }

    pub const fn capacity(&self) -> usize { CAP }

    pub fn len(&self) -> usize { self.len }

    pub fn is_empty(&self) -> bool { self.len == 0 }

    /// Inserción O(N) con búsqueda binaria O(log N) y memmove por hardware (`copy_within`)
    pub fn insert(&mut self, contact: ContactInterval) -> Result<(), ContactInterval> {
        if self.len >= CAP { return Err(contact); }
        
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.nodes[mid].start_time < contact.start_time {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        // Reemplazo del bucle manual por copy_within (Vectorized Memmove)
        if lo < self.len {
            self.nodes.copy_within(lo..self.len, lo + 1);
        }
        
        self.nodes[lo] = contact;
        self.len += 1;
        Ok(())
    }

    /// Encuentra la mejor ventana para EAT (Earliest Arrival Time)
    pub fn find_next(&self, t: u64) -> Option<ContactInterval> {
        if self.len == 0 { return None; }
        
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.nodes[mid].start_time < t { lo = mid + 1; } else { hi = mid; }
        }

        // 1. Escaneo defensivo hacia atrás para hallar intervalos solapados activos
        let mut idx = lo;
        while idx > 0 {
            let prev = self.nodes[idx - 1];
            if prev.start_time <= t && t <= prev.end_time {
                return Some(prev);
            }
            if prev.end_time < t {
                break; // No hay más solapamientos relevantes
            }
            idx -= 1;
        }

        // 2. Si no hay intervalos activos que contengan `t`, retornar el siguiente contacto futuro
        if lo < self.len {
            return Some(self.nodes[lo]);
        }
        
        None
    }

    /// Eliminación exacta optimizada vía copy_within
    pub fn remove_exact(&mut self, contact: ContactInterval) -> bool {
        for i in 0..self.len {
            if self.nodes[i] == contact {
                if i + 1 < self.len {
                    self.nodes.copy_within(i + 1..self.len, i);
                }
                self.len -= 1;
                self.nodes[self.len] = ContactInterval { start_time: u64::MAX, end_time: 0, target_node_id: 0 };
                return true;
            }
        }
        false
    }
}

impl<const CAP: usize> Default for CgrIntervalTree<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_find() {
        const CAP: usize = 8;
        let mut tree: CgrIntervalTree<CAP> = CgrIntervalTree::new();
        let a = ContactInterval::new(10, 20, 1);
        let b = ContactInterval::new(30, 40, 2);
        let c = ContactInterval::new(21, 25, 3);
        assert!(tree.insert(b).is_ok());
        assert!(tree.insert(a).is_ok());
        assert!(tree.insert(c).is_ok());
        
        assert_eq!(tree.len(), 3);
        let res = tree.find_next(9).unwrap();
        assert_eq!(res.start_time, 10);
        let res = tree.find_next(10).unwrap();
        assert_eq!(res.start_time, 10);
        let res = tree.find_next(22).unwrap();
        assert_eq!(res.start_time, 21);
        let res = tree.find_next(35).unwrap();
        assert_eq!(res.start_time, 30);
        let res = tree.find_next(41);
        assert!(res.is_none());
    }

    #[test]
    fn remove_exact_interval() {
        const CAP: usize = 4;
        let mut tree: CgrIntervalTree<CAP> = CgrIntervalTree::new();
        let a = ContactInterval::new(1, 2, 1);
        let b = ContactInterval::new(3, 4, 2);
        assert!(tree.insert(a).is_ok());
        assert!(tree.insert(b).is_ok());
        assert!(tree.remove_exact(a));
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.find_next(0).unwrap().start_time, 3);
    }
}