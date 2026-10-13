//! Undo and redo.
//!
//! Every edit is a step: the state before it is kept with a short label so
//! the History panel can list what changed. Snapshots are cheap for the
//! project because audio data is shared through `Arc`s.

/// One undoable step: the state before the edit and what the edit was.
#[derive(Debug, Clone)]
pub struct Step<T> {
    pub label: String,
    pub state: T,
}

/// An undo/redo history of states of type `T`.
#[derive(Debug, Clone)]
pub struct History<T> {
    undo: Vec<Step<T>>,
    redo: Vec<Step<T>>,
    limit: usize,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self::new(200)
    }
}

impl<T> History<T> {
    /// A history that keeps at most `limit` undo steps.
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Records an edit: `before` is the state before it. Clears the redo
    /// list, like every editor.
    pub fn record(&mut self, label: impl Into<String>, before: T) {
        self.undo.push(Step {
            label: label.into(),
            state: before,
        });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Undoes the last edit: returns the state to restore and keeps
    /// `current` for redo.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let step = self.undo.pop()?;
        self.redo.push(Step {
            label: step.label,
            state: current,
        });
        Some(step.state)
    }

    /// Redoes the last undone edit.
    pub fn redo(&mut self, current: T) -> Option<T> {
        let step = self.redo.pop()?;
        self.undo.push(Step {
            label: step.label,
            state: current,
        });
        Some(step.state)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the edit Undo would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Labels of done edits, oldest first.
    pub fn done(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(|s| s.label.as_str())
    }

    /// Labels of undone edits, next redo first.
    pub fn undone(&self) -> impl Iterator<Item = &str> {
        self.redo.iter().rev().map(|s| s.label.as_str())
    }

    /// Number of done edits.
    pub fn position(&self) -> usize {
        self.undo.len()
    }

    /// Undoes or redoes until `position` edits are done (the History panel's
    /// "jump here"). Returns the state to restore, if anything changed.
    pub fn jump_to(&mut self, position: usize, mut current: T) -> Option<T> {
        let target = position.min(self.undo.len() + self.redo.len());
        let mut moved = false;
        while self.undo.len() > target {
            current = self.undo(current)?;
            moved = true;
        }
        while self.undo.len() < target {
            current = self.redo(current)?;
            moved = true;
        }
        moved.then_some(current)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_walk_the_states() {
        let mut h = History::new(10);
        let mut doc = 0;
        for i in 1..=3 {
            h.record(format!("set {i}"), doc);
            doc = i;
        }
        assert_eq!(h.undo_label(), Some("set 3"));
        doc = h.undo(doc).unwrap();
        assert_eq!(doc, 2);
        doc = h.undo(doc).unwrap();
        assert_eq!(doc, 1);
        assert_eq!(h.redo_label(), Some("set 2"));
        doc = h.redo(doc).unwrap();
        assert_eq!(doc, 2);
        // A new edit drops the redo list.
        h.record("set 9", doc);
        doc = 9;
        assert!(!h.can_redo());
        assert_eq!(h.done().collect::<Vec<_>>(), vec!["set 1", "set 2", "set 9"]);
        assert_eq!(h.undo(doc), Some(2));
    }

    #[test]
    fn limit_drops_the_oldest_steps() {
        let mut h = History::new(2);
        h.record("a", 0);
        h.record("b", 1);
        h.record("c", 2);
        assert_eq!(h.done().collect::<Vec<_>>(), vec!["b", "c"]);
    }

    #[test]
    fn jump_moves_both_ways() {
        let mut h = History::new(10);
        let mut doc = 0;
        for i in 1..=4 {
            h.record(format!("{i}"), doc);
            doc = i;
        }
        doc = h.jump_to(1, doc).unwrap();
        assert_eq!(doc, 1);
        assert_eq!(h.position(), 1);
        assert_eq!(h.undone().collect::<Vec<_>>(), vec!["2", "3", "4"]);
        doc = h.jump_to(3, doc).unwrap();
        assert_eq!(doc, 3);
        assert!(h.jump_to(3, doc).is_none());
    }
}
