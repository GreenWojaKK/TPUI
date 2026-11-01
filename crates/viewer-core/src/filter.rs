//! Filter: conditions joined in the shape spreadsheet filters use.
//!
//! A [`Filter`] is a list of clauses that must all hold (`and`). A [`FilterClause`] is one or more conditions on
//! one column, of which at least one must hold (`or`); with more than one it is written in parentheses. So `or`
//! joins conditions on one column only and parentheses never nest, as in
//! `(route == "a" or route == "b") and id >= 5 and id < 20`.
//!
//! Clauses on the same column sit next to each other, in the order their columns first appear, so each column's
//! conditions read together.

use crate::{ColumnId, FilterCondition, FilterOp};

/// How two neighbouring conditions combine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LogicOp {
    /// Both hold.
    #[default]
    And,
    /// At least one holds.
    Or,
}

impl LogicOp {
    pub const ALL: [LogicOp; 2] = [LogicOp::And, LogicOp::Or];

    pub fn text(self) -> &'static str {
        match self {
            LogicOp::And => "and",
            LogicOp::Or => "or",
        }
    }
}

/// Conditions on one column, at least one of which must hold. Never empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FilterClause {
    conditions: Vec<FilterCondition>,
}

impl FilterClause {
    pub fn column(&self) -> ColumnId {
        self.conditions[0].column
    }

    pub fn conditions(&self) -> &[FilterCondition] {
        &self.conditions
    }
}

/// Clauses that must all hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Filter {
    clauses: Vec<FilterClause>,
}

impl Filter {
    /// A filter of these clauses, each a list of conditions on one column joined by `or`. Empty clauses are
    /// dropped and clauses on the same column are moved next to each other.
    ///
    /// # Panics
    ///
    /// If a clause holds conditions on more than one column.
    pub fn from_clauses(clauses: Vec<Vec<FilterCondition>>) -> Self {
        let clauses = clauses
            .into_iter()
            .filter(|conditions| !conditions.is_empty())
            .map(|conditions| {
                assert!(
                    conditions.iter().all(|c| c.column == conditions[0].column),
                    "a filter clause joins conditions on one column only"
                );
                FilterClause { conditions }
            })
            .collect();
        let mut filter = Self { clauses };
        filter.normalize();
        filter
    }

    /// Conditions that must all hold, each a clause of its own.
    pub fn all(conditions: Vec<FilterCondition>) -> Self {
        Self::from_clauses(conditions.into_iter().map(|c| vec![c]).collect())
    }

    /// Whether there is no condition, so every row passes.
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    pub fn clauses(&self) -> &[FilterClause] {
        &self.clauses
    }

    /// Every condition, in the order shown.
    pub fn conditions(&self) -> Vec<&FilterCondition> {
        self.clauses
            .iter()
            .flat_map(|clause| &clause.conditions)
            .collect()
    }

    /// The clause that [`Self::add`] puts `condition` in with `or`, or `None` when it adds a clause of its own.
    ///
    /// With `or` asked, that is the last clause on the condition's column. Without, an `==` condition still joins
    /// the last clause on its column that has an `==` condition, since two different `==` values joined by `and`
    /// match no row; other conditions add with `and`.
    pub fn or_clause(&self, condition: &FilterCondition, or: bool) -> Option<usize> {
        let on_column = |clause: &FilterClause| clause.column() == condition.column;
        if or {
            return self.clauses.iter().rposition(on_column);
        }
        if condition.op != FilterOp::Eq {
            return None;
        }
        self.clauses.iter().rposition(|clause| {
            on_column(clause)
                && clause
                    .conditions
                    .iter()
                    .any(|other| other.op == FilterOp::Eq)
        })
    }

    /// Whether [`Self::add`] changes the filter, which it does not when the condition is already where it would go.
    pub fn adds(&self, condition: &FilterCondition, or: bool) -> bool {
        match self.or_clause(condition, or) {
            Some(ix) => !self.clauses[ix].conditions.contains(condition),
            None => !self
                .clauses
                .iter()
                .any(|clause| clause.conditions == [condition.clone()]),
        }
    }

    /// Adds a condition as a click does: into the clause of [`Self::or_clause`] with `or`, otherwise as a clause
    /// of its own with `and`, after the other clauses on its column. A condition already there is not added again.
    pub fn add(&mut self, condition: FilterCondition, or: bool) {
        if !self.adds(&condition, or) {
            return;
        }
        match self.or_clause(&condition, or) {
            Some(ix) => self.clauses[ix].conditions.push(condition),
            None => {
                let at = self
                    .clauses
                    .iter()
                    .rposition(|clause| clause.column() == condition.column)
                    .map_or(self.clauses.len(), |ix| ix + 1);
                self.clauses.insert(
                    at,
                    FilterClause {
                        conditions: vec![condition],
                    },
                );
            }
        }
    }

    /// Removes condition `ix` of clause `clause`, and the clause when it was its last. Returns whether there was
    /// one.
    pub fn remove(&mut self, clause: usize, ix: usize) -> bool {
        let Some(target) = self.clauses.get_mut(clause) else {
            return false;
        };
        if ix >= target.conditions.len() {
            return false;
        }
        target.conditions.remove(ix);
        if target.conditions.is_empty() {
            self.clauses.remove(clause);
        }
        true
    }

    /// Keeps only the conditions for which `keep` holds.
    pub fn retain(&mut self, keep: impl Fn(&FilterCondition) -> bool) {
        for clause in &mut self.clauses {
            clause.conditions.retain(&keep);
        }
        self.clauses.retain(|clause| !clause.conditions.is_empty());
    }

    /// How condition `ix` of clause `clause` combines with the next condition shown, and whether the other operator
    /// can be chosen there: `or` inside a clause, `and` between clauses, where only clauses on the same column can
    /// become `or`. `None` after the last condition.
    pub fn join_after(&self, clause: usize, ix: usize) -> Option<(LogicOp, bool)> {
        let current = self.clauses.get(clause)?;
        if ix + 1 < current.conditions.len() {
            return Some((LogicOp::Or, true));
        }
        let next = self.clauses.get(clause + 1)?;
        Some((LogicOp::And, next.column() == current.column()))
    }

    /// Sets the operator after condition `ix` of clause `clause`. `and` inside a clause splits it there into two;
    /// `or` between two clauses on the same column merges them. Anything else leaves the filter as it is.
    pub fn set_join(&mut self, clause: usize, ix: usize, op: LogicOp) {
        let Some((current, changeable)) = self.join_after(clause, ix) else {
            return;
        };
        if current == op || !changeable {
            return;
        }
        match op {
            LogicOp::And => {
                let rest = self.clauses[clause].conditions.split_off(ix + 1);
                self.clauses
                    .insert(clause + 1, FilterClause { conditions: rest });
            }
            LogicOp::Or => {
                let next = self.clauses.remove(clause + 1);
                self.clauses[clause].conditions.extend(next.conditions);
            }
        }
    }

    /// Whether conditions `from` through `to` in the order shown, at least two, are all on one column, so that
    /// [`Self::join_range`] can make them one clause. The ends may come in either order.
    pub fn can_join_range(&self, from: usize, to: usize) -> bool {
        let (from, to) = (from.min(to), from.max(to));
        let conditions = self.conditions();
        from < to
            && to < conditions.len()
            && conditions[from..=to]
                .iter()
                .all(|condition| condition.column == conditions[from].column)
    }

    /// Makes conditions `from` through `to` in the order shown exactly one clause: joined by `or` among themselves
    /// and by `and` to the conditions just outside them. Returns whether the filter changed; a range that
    /// [`Self::can_join_range`] refuses changes nothing.
    pub fn join_range(&mut self, from: usize, to: usize) -> bool {
        if !self.can_join_range(from, to) {
            return false;
        }
        let (from, to) = (from.min(to), from.max(to));
        // Whether each condition shares a clause with the next one shown.
        let mut joined: Vec<bool> = self
            .clauses
            .iter()
            .flat_map(|clause| {
                let len = clause.conditions.len();
                (0..len).map(move |ix| ix + 1 < len)
            })
            .collect();
        joined[from..to].fill(true);
        if from > 0 {
            joined[from - 1] = false;
        }
        joined[to] = false;
        let mut clauses = Vec::new();
        let mut current = Vec::new();
        let conditions = self.clauses.iter().flat_map(|clause| &clause.conditions);
        for (condition, joined) in conditions.zip(joined) {
            current.push(condition.clone());
            if !joined {
                clauses.push(FilterClause {
                    conditions: std::mem::take(&mut current),
                });
            }
        }
        let changed = clauses != self.clauses;
        self.clauses = clauses;
        changed
    }

    /// Moves clauses on the same column next to each other, in the order their columns first appear.
    fn normalize(&mut self) {
        let mut order: Vec<ColumnId> = Vec::new();
        for clause in &self.clauses {
            if !order.contains(&clause.column()) {
                order.push(clause.column());
            }
        }
        self.clauses.sort_by_key(|clause| {
            order
                .iter()
                .position(|column| *column == clause.column())
                .expect("every column is in the order")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(column: usize, op: FilterOp, value: &str) -> FilterCondition {
        FilterCondition {
            column: ColumnId(column),
            op,
            value: value.into(),
        }
    }

    fn eq(column: usize, value: &str) -> FilterCondition {
        c(column, FilterOp::Eq, value)
    }

    #[test]
    fn keeps_each_columns_clauses_together() {
        let filter = Filter::from_clauses(vec![
            vec![eq(1, "a")],
            vec![],
            vec![eq(0, "x"), eq(0, "y")],
            vec![c(1, FilterOp::NotEq, "b")],
        ]);
        assert_eq!(
            filter,
            Filter::from_clauses(vec![
                vec![eq(1, "a")],
                vec![c(1, FilterOp::NotEq, "b")],
                vec![eq(0, "x"), eq(0, "y")],
            ])
        );
        assert_eq!(filter.clauses().len(), 3);
        assert_eq!(filter.clauses()[2].column(), ColumnId(0));
        assert_eq!(filter.conditions().len(), 4);
        assert!(Filter::default().is_empty());
    }

    #[test]
    #[should_panic(expected = "one column only")]
    fn refuses_or_across_columns() {
        Filter::from_clauses(vec![vec![eq(0, "a"), eq(1, "b")]]);
    }

    #[test]
    fn adds_like_a_spreadsheet_filter() {
        let mut filter = Filter::default();
        filter.add(eq(0, "366"), false);
        filter.add(eq(1, "중구01"), false);
        // A second value of the same column joins with `or`: `and` would match no row.
        assert_eq!(filter.or_clause(&eq(0, "977"), false), Some(0));
        filter.add(eq(0, "977"), false);
        // Other operators add with `and`, next to the column's other clauses.
        filter.add(c(0, FilterOp::NotEq, "1"), false);
        assert_eq!(
            filter,
            Filter::from_clauses(vec![
                vec![eq(0, "366"), eq(0, "977")],
                vec![c(0, FilterOp::NotEq, "1")],
                vec![eq(1, "중구01")],
            ])
        );
        // `or` asked joins the column's last clause.
        filter.add(c(0, FilterOp::Gt, "500"), true);
        assert_eq!(
            filter.clauses()[1].conditions(),
            [c(0, FilterOp::NotEq, "1"), c(0, FilterOp::Gt, "500")]
        );
        // `or` on a column without conditions adds a clause.
        assert_eq!(filter.or_clause(&eq(2, "z"), true), None);
        // Nothing is added twice.
        assert!(!filter.adds(&eq(0, "977"), false));
        assert!(filter.adds(&c(1, FilterOp::NotEq, "중구01"), false));
        let before = filter.clone();
        filter.add(eq(0, "977"), false);
        filter.add(eq(1, "중구01"), false);
        assert_eq!(filter, before);
    }

    #[test]
    fn splits_and_merges_clauses_of_one_column() {
        let mut filter = Filter::from_clauses(vec![
            vec![eq(0, "a"), eq(0, "b"), eq(0, "c")],
            vec![eq(1, "x")],
        ]);
        assert_eq!(filter.join_after(0, 1), Some((LogicOp::Or, true)));
        assert_eq!(filter.join_after(0, 2), Some((LogicOp::And, false)));
        assert_eq!(filter.join_after(1, 0), None);
        // (a or b or c) → (a or b) and c
        filter.set_join(0, 1, LogicOp::And);
        assert_eq!(
            filter,
            Filter::from_clauses(vec![
                vec![eq(0, "a"), eq(0, "b")],
                vec![eq(0, "c")],
                vec![eq(1, "x")],
            ])
        );
        assert_eq!(filter.join_after(0, 1), Some((LogicOp::And, true)));
        // Different columns stay joined by `and`.
        filter.set_join(1, 0, LogicOp::Or);
        assert_eq!(filter.clauses().len(), 3);
        // And back: (a or b or c)
        filter.set_join(0, 1, LogicOp::Or);
        assert_eq!(
            filter.clauses()[0].conditions(),
            [eq(0, "a"), eq(0, "b"), eq(0, "c")]
        );
    }

    #[test]
    fn joins_a_swept_range_into_one_clause() {
        let mut filter = Filter::all(vec![eq(0, "a"), eq(0, "b"), eq(0, "c"), eq(1, "x")]);
        // a, b, c → (a or b or c); the ends may come in either order.
        assert!(filter.join_range(2, 0));
        assert_eq!(
            filter,
            Filter::from_clauses(vec![
                vec![eq(0, "a"), eq(0, "b"), eq(0, "c")],
                vec![eq(1, "x")]
            ])
        );
        // The same range again changes nothing.
        assert!(!filter.join_range(0, 2));
        // a, b inside (a or b or c) → (a or b) and c
        assert!(filter.join_range(0, 1));
        assert_eq!(
            filter,
            Filter::from_clauses(vec![
                vec![eq(0, "a"), eq(0, "b")],
                vec![eq(0, "c")],
                vec![eq(1, "x")]
            ])
        );
        // b, c across the clause boundary → a and (b or c)
        assert!(filter.join_range(1, 2));
        assert_eq!(
            filter,
            Filter::from_clauses(vec![
                vec![eq(0, "a")],
                vec![eq(0, "b"), eq(0, "c")],
                vec![eq(1, "x")]
            ])
        );
        // One condition, another column in the range, or a range past the end changes nothing.
        let before = filter.clone();
        assert!(!filter.can_join_range(1, 1));
        assert!(!filter.join_range(1, 1));
        assert!(!filter.can_join_range(2, 3));
        assert!(!filter.join_range(2, 3));
        assert!(!filter.join_range(2, 9));
        assert_eq!(filter, before);
    }

    #[test]
    fn removes_conditions_and_empty_clauses() {
        let mut filter = Filter::from_clauses(vec![vec![eq(0, "a"), eq(0, "b")], vec![eq(1, "x")]]);
        assert!(filter.remove(0, 0));
        assert_eq!(filter, Filter::all(vec![eq(0, "b"), eq(1, "x")]));
        assert!(filter.remove(1, 0));
        assert!(!filter.remove(1, 0));
        assert!(!filter.remove(0, 3));
        let mut by_column =
            Filter::from_clauses(vec![vec![eq(0, "a"), eq(0, "b")], vec![eq(1, "x")]]);
        by_column.retain(|condition| condition.column != ColumnId(0));
        assert_eq!(by_column, Filter::all(vec![eq(1, "x")]));
    }
}
