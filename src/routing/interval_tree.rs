//! Fase 3: Interval Tree O(log N) para CGR y EAT


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

/// Static-capacity interval index optimized for Earliest Arrival Time (EAT).
/// Internally keeps intervals sorted by `start_time` in a preallocated array.
/// Query `find_next(t)` runs in O(log N) to locate the interval that contains
/// `t` (immediate arrival) or the next interval with `start_time >= t`.
pub struct CgrIntervalTree<const CAP: usize> {
    nodes: [ContactInterval; CAP],
    len: usize,
}

impl<const CAP: usize> CgrIntervalTree<CAP> {
    /// Create an empty tree. Requires `CAP > 0`.
    pub const fn new() -> Self {
        // sentinel interval with start_time = u64::MAX sorts at the end
        const SENTINEL: ContactInterval = ContactInterval { start_time: u64::MAX, end_time: 0, target_node_id: 0 };
        // initialize array with sentinel values
        let nodes = [SENTINEL; CAP];
        Self { nodes, len: 0 }
    }

    pub const fn capacity(&self) -> usize { CAP }

    pub fn len(&self) -> usize { self.len }

    /// Insert a contact interval. Maintains ordering by `start_time`.
    /// If capacity is full, returns Err(contact) back to caller.
    pub fn insert(&mut self, contact: ContactInterval) -> Result<(), ContactInterval> {
        if self.len >= CAP { return Err(contact); }
        // find insertion index (lower_bound by start_time)
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
        // shift right
        let mut i = self.len;
        while i > lo {
            self.nodes[i] = self.nodes[i - 1];
            i -= 1;
        }
        self.nodes[lo] = contact;
        self.len += 1;
        Ok(())
    }

    /// Find best contact for EAT given current time `t`.
    /// Returns `Some(interval)` if a contact containing `t` exists or the next
    /// interval with start_time >= t. Returns `None` if no future contact.
    pub fn find_next(&self, t: u64) -> Option<ContactInterval> {
        if self.len == 0 { return None; }
        // binary search lower_bound on start_time
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.nodes[mid].start_time < t { lo = mid + 1; } else { hi = mid; }
        }
        // check if previous interval contains t
        if lo > 0 {
            let prev = self.nodes[lo - 1];
            if prev.start_time <= t && t <= prev.end_time {
                return Some(prev);
            }
        }
        // check current lower_bound
        if lo < self.len {
            return Some(self.nodes[lo]);
        }
        None
    }

    /// Optionally remove an interval by exact match; returns true if removed.
    pub fn remove_exact(&mut self, contact: ContactInterval) -> bool {
        let mut i = 0usize;
        while i < self.len {
            if self.nodes[i] == contact {
                // shift left
                let mut j = i;
                while j + 1 < self.len {
                    self.nodes[j] = self.nodes[j + 1];
                    j += 1;
                }
                self.len -= 1;
                // put sentinel at end
                self.nodes[self.len] = ContactInterval { start_time: u64::MAX, end_time: 0, target_node_id: 0 };
                return true;
            }
            i += 1;
        }
        false
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
        // tree should be ordered by start_time: a(10), c(21), b(30)
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
