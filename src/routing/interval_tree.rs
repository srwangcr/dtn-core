//! Fixed-capacity **Contact Plan Scheduler / Interval Selector** for next-hop
//! Earliest Arrival Time (EAT) queries.
//!
//! This module selects the active contact with the longest remaining window.
//! It is a single-hop interval selector, **not** full Contact Graph Routing:
//! multi-hop CGR (graph search, alternate paths and future-contact scheduling)
//! is future work. The `CgrIntervalTree` name is kept for API compatibility.
//!
//! Contacts are kept sorted by `start_time` in a `const`-sized array. Insertion
//! and removal use a binary search plus `slice::copy_within` (vectorised
//! memmove on most targets); all operations are allocation-free and bounded by
//! `CAP`.
//!
//! ```
//! use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
//! let mut tree = CgrIntervalTree::<8>::new();
//! tree.insert(ContactInterval::new(10, 20, 42)).unwrap();
//! assert_eq!(tree.find_next(15).unwrap().target_node_id, 42);
//! assert!(tree.find_next(21).is_none());
//! ```

/// One scheduled contact opportunity in the network.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContactInterval {
    /// Start of the contact window, in DTN milliseconds.
    pub start_time: u64,
    /// End of the contact window (inclusive), in DTN milliseconds.
    pub end_time: u64,
    /// Identifier of the neighbouring node reachable during the contact.
    pub target_node_id: u32,
}

impl ContactInterval {
    /// Creates a contact interval.
    ///
    /// An interval with `end_time < start_time` is never active but can still
    /// be stored; callers are responsible for supplying sane contact plans.
    pub const fn new(start_time: u64, end_time: u64, target_node_id: u32) -> Self {
        Self { start_time, end_time, target_node_id }
    }

    /// Returns `true` when this contact covers time `t`.
    pub const fn contains(&self, t: u64) -> bool {
        self.start_time <= t && t <= self.end_time
    }
}

const fn sentinel() -> ContactInterval {
    ContactInterval { start_time: u64::MAX, end_time: 0, target_node_id: 0 }
}

/// Static, sorted set of [`ContactInterval`] values with O(log N) lookup.
pub struct CgrIntervalTree<const CAP: usize> {
    nodes: [ContactInterval; CAP],
    len: usize,
}

impl<const CAP: usize> CgrIntervalTree<CAP> {
    /// Creates an empty tree in `const` context.
    pub const fn new() -> Self {
        Self { nodes: [sentinel(); CAP], len: 0 }
    }

    /// Maximum number of contacts held by this tree.
    pub const fn capacity(&self) -> usize {
        CAP
    }

    /// Number of contacts currently stored.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when no contacts are stored.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Inserts a contact, keeping the array sorted by start time.
    ///
    /// Returns the rejected contact when the tree is full. Duplicate intervals
    /// are allowed and are distinguished by their full value.
    pub fn insert(&mut self, contact: ContactInterval) -> Result<(), ContactInterval> {
        if self.len >= CAP {
            return Err(contact);
        }

        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.nodes[mid].start_time < contact.start_time {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        if lo < self.len {
            self.nodes.copy_within(lo..self.len, lo + 1);
        }
        self.nodes[lo] = contact;
        self.len += 1;
        Ok(())
    }

    /// Finds the best contact for time `t`.
    ///
    /// Active contacts (`start <= t <= end`) take precedence over future ones.
    /// When several contacts are active (overlapping windows), the one with
    /// the latest `end_time` is chosen because it offers the longest remaining
    /// transmission opportunity. If none is active, the earliest future
    /// contact is returned.
    pub fn find_next(&self, t: u64) -> Option<ContactInterval> {
        if self.len == 0 {
            return None;
        }

        // Binary search for the first contact with start_time >= t.
        let mut lo = 0usize;
        let mut hi = self.len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.nodes[mid].start_time < t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        // Scan every contact starting before `t`. A contact that starts early
        // may still be active after a later, shorter contact has ended, so we
        // cannot break early on `end_time < t`.
        let mut best: Option<ContactInterval> = None;
        for candidate in self.nodes.iter().take(lo).copied() {
            if candidate.contains(t)
                && best.map_or(true, |current| candidate.end_time > current.end_time)
            {
                best = Some(candidate);
            }
        }
        if let Some(active) = best {
            return Some(active);
        }

        self.nodes.get(lo).filter(|_| lo < self.len).copied()
    }

    /// Removes the first contact equal to `contact`.
    ///
    /// Returns `true` when a matching contact was found and removed.
    pub fn remove_exact(&mut self, contact: ContactInterval) -> bool {
        for i in 0..self.len {
            if self.nodes[i] == contact {
                if i + 1 < self.len {
                    self.nodes.copy_within(i + 1..self.len, i);
                }
                self.len -= 1;
                self.nodes[self.len] = sentinel();
                return true;
            }
        }
        false
    }

    /// Removes all contacts, restoring the tree to its initial state.
    pub fn clear(&mut self) {
        for node in self.nodes.iter_mut() {
            *node = sentinel();
        }
        self.len = 0;
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
        assert_eq!(tree.find_next(9).unwrap().start_time, 10);
        assert_eq!(tree.find_next(10).unwrap().start_time, 10);
        assert_eq!(tree.find_next(22).unwrap().start_time, 21);
        assert_eq!(tree.find_next(35).unwrap().start_time, 30);
        assert!(tree.find_next(41).is_none());
    }

    #[test]
    fn overlapping_contacts_prefer_longest_remaining_window() {
        let mut tree = CgrIntervalTree::<4>::new();
        // Long contact starts first but is stored before the short one.
        tree.insert(ContactInterval::new(0, 100, 7)).unwrap();
        tree.insert(ContactInterval::new(10, 12, 99)).unwrap();
        let chosen = tree.find_next(11).unwrap();
        assert_eq!(chosen.target_node_id, 7);
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
        assert!(!tree.remove_exact(a));
    }

    #[test]
    fn full_tree_rejects_contact_and_clear_restores_capacity() {
        let mut tree = CgrIntervalTree::<1>::new();
        let a = ContactInterval::new(1, 2, 1);
        let b = ContactInterval::new(3, 4, 2);
        assert_eq!(tree.insert(a), Ok(()));
        assert_eq!(tree.insert(b), Err(b));
        tree.clear();
        assert!(tree.is_empty());
        assert_eq!(tree.insert(b), Ok(()));
    }

    #[test]
    fn contains_is_inclusive_on_both_ends() {
        let contact = ContactInterval::new(5, 7, 1);
        assert!(!contact.contains(4));
        assert!(contact.contains(5));
        assert!(contact.contains(7));
        assert!(!contact.contains(8));
    }
}
