use std::rc::Rc;

use crate::core::buffer::EditEvent;

/// A single reversible edit operation.
#[derive(Debug, Clone)]
pub struct EditRecord {
    /// Character offset where the edit starts.
    pub char_offset: usize,
    /// Text that was removed (empty for pure insertion).
    pub old_text: Rc<str>,
    /// Text that was inserted (empty for pure deletion).
    pub new_text: Rc<str>,
    /// EditEvent for tree-sitter (forward direction).
    #[allow(dead_code)]
    pub edit_event: EditEvent,
}

/// A group of edits that form one logical undo step.
#[derive(Debug, Clone)]
pub struct EditTransaction {
    pub records: Vec<EditRecord>,
    pub cursors_before: Vec<usize>,
    pub cursors_after: Vec<usize>,
}

/// Undo/redo history stack.
pub struct History {
    undo_stack: Vec<EditTransaction>,
    redo_stack: Vec<EditTransaction>,
    current_tx: Option<EditTransaction>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_tx: None,
        }
    }

    /// Returns true if a transaction is currently open.
    pub fn has_open_transaction(&self) -> bool {
        self.current_tx.is_some()
    }

    /// Begin a new transaction for grouping edits.
    /// If a transaction is already open, this is a no-op.
    /// Returns true if a new transaction was started, false if one was already open.
    pub fn begin_transaction(&mut self, cursors_before: &[usize]) -> bool {
        if self.current_tx.is_none() {
            self.current_tx = Some(EditTransaction {
                records: Vec::new(),
                cursors_before: cursors_before.to_vec(),
                cursors_after: cursors_before.to_vec(),
            });
            true
        } else {
            false
        }
    }

    /// Record an edit. If a transaction is open, append to it.
    /// Otherwise, push an atomic (single-record) transaction to the undo stack.
    pub fn record(
        &mut self,
        record: EditRecord,
        cursors_before: &[usize],
        cursors_after: &[usize],
    ) {
        if let Some(ref mut tx) = self.current_tx {
            tx.records.push(record);
            tx.cursors_after = cursors_after.to_vec();
        } else {
            self.undo_stack.push(EditTransaction {
                records: vec![record],
                cursors_before: cursors_before.to_vec(),
                cursors_after: cursors_after.to_vec(),
            });
            self.redo_stack.clear();
        }
    }

    /// Update the cursors_after for the current transaction or the last record on the undo stack.
    pub fn update_cursors_after(&mut self, cursors_after: &[usize]) {
        if let Some(ref mut tx) = self.current_tx {
            tx.cursors_after = cursors_after.to_vec();
        } else if let Some(tx) = self.undo_stack.last_mut() {
            tx.cursors_after = cursors_after.to_vec();
        }
    }

    /// Commit the open transaction to the undo stack.
    /// Empty transactions are discarded. Returns true if a transaction was committed.
    pub fn commit_transaction(&mut self) -> bool {
        if let Some(tx) = self.current_tx.take() {
            if tx.records.is_empty() {
                return false;
            }
            self.undo_stack.push(tx);
            self.redo_stack.clear();
            true
        } else {
            false
        }
    }

    /// Flush (commit) any open transaction. Safety valve for mode/buffer switches.
    pub fn flush_transaction(&mut self) {
        self.commit_transaction();
    }

    /// Pop the most recent undo transaction, if any.
    pub fn pop_undo(&mut self) -> Option<EditTransaction> {
        self.undo_stack.pop()
    }

    /// Push a transaction onto the redo stack.
    pub fn push_redo(&mut self, tx: EditTransaction) {
        self.redo_stack.push(tx);
    }

    /// Pop the most recent redo transaction, if any.
    pub fn pop_redo(&mut self) -> Option<EditTransaction> {
        self.redo_stack.pop()
    }

    /// Push a transaction onto the undo stack (used during redo).
    pub fn push_undo(&mut self, tx: EditTransaction) {
        self.undo_stack.push(tx);
    }

    #[cfg(test)]
    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    #[cfg(test)]
    pub fn redo_len(&self) -> usize {
        self.redo_stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_edit_event() -> EditEvent {
        EditEvent {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (0, 1),
        }
    }

    fn dummy_record(new_text: &str) -> EditRecord {
        EditRecord {
            char_offset: 0,
            old_text: Rc::from(""),
            new_text: Rc::from(new_text),
            edit_event: dummy_edit_event(),
        }
    }

    #[test]
    fn record_clears_redo() {
        let mut h = History::new();
        h.push_redo(EditTransaction {
            records: vec![],
            cursors_before: vec![0],
            cursors_after: vec![1],
        });
        assert_eq!(h.redo_len(), 1);

        h.record(
            EditRecord {
                char_offset: 0,
                old_text: Rc::from(""),
                new_text: Rc::from("a"),
                edit_event: dummy_edit_event(),
            },
            &[0],
            &[1],
        );
        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.redo_len(), 0); // cleared
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut h = History::new();
        let tx = EditTransaction {
            records: vec![],
            cursors_before: vec![0],
            cursors_after: vec![5],
        };
        h.undo_stack.push(tx);
        assert_eq!(h.undo_len(), 1);

        let popped = h.pop_undo().unwrap();
        assert_eq!(popped.cursors_after, vec![5]);
        h.push_redo(popped);
        assert_eq!(h.redo_len(), 1);

        let redone = h.pop_redo().unwrap();
        h.push_undo(redone);
        assert_eq!(h.undo_len(), 1);
    }

    #[test]
    fn begin_commit_transaction() {
        let mut h = History::new();
        h.begin_transaction(&[0]);
        h.record(dummy_record("a"), &[0], &[1]);
        h.record(dummy_record("b"), &[1], &[2]);
        h.record(dummy_record("c"), &[2], &[3]);
        assert!(h.commit_transaction());
        assert_eq!(h.undo_len(), 1);
        let tx = h.pop_undo().unwrap();
        assert_eq!(tx.records.len(), 3);
        assert_eq!(tx.cursors_before, vec![0]);
        assert_eq!(tx.cursors_after, vec![3]);
    }

    #[test]
    fn commit_empty_transaction_is_noop() {
        let mut h = History::new();
        h.begin_transaction(&[0]);
        assert!(!h.commit_transaction());
        assert_eq!(h.undo_len(), 0);
    }

    #[test]
    fn record_without_transaction_is_atomic() {
        let mut h = History::new();
        h.record(dummy_record("a"), &[0], &[1]);
        h.record(dummy_record("b"), &[1], &[2]);
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn commit_clears_redo() {
        let mut h = History::new();
        h.push_redo(EditTransaction {
            records: vec![],
            cursors_before: vec![0],
            cursors_after: vec![1],
        });
        assert_eq!(h.redo_len(), 1);

        h.begin_transaction(&[0]);
        // redo still intact during open transaction
        assert_eq!(h.redo_len(), 1);
        h.record(dummy_record("a"), &[0], &[1]);
        // redo still intact - not cleared until commit
        assert_eq!(h.redo_len(), 1);
        h.commit_transaction();
        assert_eq!(h.redo_len(), 0);
    }

    #[test]
    fn double_begin_is_idempotent() {
        let mut h = History::new();
        h.begin_transaction(&[0]);
        h.record(dummy_record("a"), &[0], &[1]);
        h.begin_transaction(&[5]); // should be ignored
        h.record(dummy_record("b"), &[1], &[2]);
        h.commit_transaction();
        assert_eq!(h.undo_len(), 1);
        let tx = h.pop_undo().unwrap();
        assert_eq!(tx.records.len(), 2);
        assert_eq!(tx.cursors_before, vec![0]); // from first begin, not second
    }
}
