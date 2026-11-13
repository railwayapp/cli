//! The one way a list is picked from in this TUI: a centred card a dozen rows
//! tall that scrolls under the cursor, and that narrows as you type once the
//! list is long enough to need it. The card is `ui::render_panel`; this is the
//! filter behind it, shared by every list that card draws.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::wizard::Step;

/// A list this long or shorter is read at a glance; past it, typing filters.
pub const SEARCHABLE_AFTER: usize = 8;

/// Which list a filter was typed into. Another list starts with an empty one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListId {
    /// Where new agents go: the ⌥n / ⌥p project picker.
    Target,
    /// The settings card's project sub-picker.
    SettingsProject,
    /// One step of the setup flow.
    Wizard(Step),
}

#[derive(Clone, Debug, Default)]
pub struct ListFilter {
    list: Option<ListId>,
    query: String,
}

impl ListFilter {
    /// What has been typed into `list`; empty for any other.
    pub fn query(&self, list: ListId) -> &str {
        if self.list == Some(list) {
            &self.query
        } else {
            ""
        }
    }

    /// A key for a list of `len` rows, with the cursor at `cursor`, whose
    /// labels are `labels`: typing filters (on a list long enough to search),
    /// backspace takes a character back, esc clears what was typed, and ↑/↓
    /// move among the rows the filter keeps. Returns the cursor to use when
    /// the key was the list's; `None` leaves it to the caller — enter, and esc
    /// with nothing typed.
    pub fn on_key(
        &mut self,
        list: ListId,
        labels: &[String],
        cursor: usize,
        key: &KeyEvent,
    ) -> Option<usize> {
        if self.list != Some(list) {
            self.list = Some(list);
            self.query.clear();
        }
        let searchable = labels.len() > SEARCHABLE_AFTER || !self.query.is_empty();
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Up => Some(step(&visible(&self.query, labels), cursor, -1)),
            KeyCode::Down => Some(step(&visible(&self.query, labels), cursor, 1)),
            KeyCode::Char(c) if searchable && plain => {
                self.query.push(c);
                Some(settle(&visible(&self.query, labels), cursor))
            }
            KeyCode::Backspace if !self.query.is_empty() => {
                self.query.pop();
                Some(settle(&visible(&self.query, labels), cursor))
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                Some(cursor)
            }
            _ => None,
        }
    }
}

/// Every word typed appears somewhere in the label, in any order and case:
/// `prod mono` finds `mono (production)`.
pub fn matches(query: &str, label: &str) -> bool {
    let label = label.to_lowercase();
    query
        .split_whitespace()
        .all(|term| label.contains(&term.to_lowercase()))
}

/// The indices of the labels `query` keeps, in order.
pub fn visible(query: &str, labels: &[String]) -> Vec<usize> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, label)| matches(query, label))
        .map(|(i, _)| i)
        .collect()
}

/// The cursor `delta` rows on among `visible`, stopping at either end; from a
/// row the filter hid, the first one it kept.
fn step(visible: &[usize], cursor: usize, delta: isize) -> usize {
    let Some(at) = visible.iter().position(|&i| i == cursor) else {
        return visible.first().copied().unwrap_or(cursor);
    };
    let to = at.saturating_add_signed(delta).min(visible.len() - 1);
    visible[to]
}

/// The cursor where it is if the filter kept it, else on the first row kept.
fn settle(visible: &[usize], cursor: usize) -> usize {
    if visible.contains(&cursor) {
        cursor
    } else {
        visible.first().copied().unwrap_or(cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn labels(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("project-{i} (production)"))
            .collect()
    }

    #[test]
    fn words_match_anywhere_in_any_case() {
        assert!(matches("", "anything"));
        assert!(matches("PROD mono", "mono (production)"));
        assert!(!matches("staging", "mono (production)"));
    }

    #[test]
    fn typing_narrows_a_long_list_and_moves_onto_what_is_left() {
        let mut filter = ListFilter::default();
        let labels = labels(12);
        let mut cursor = 0;
        for c in "-1".chars() {
            cursor = filter
                .on_key(ListId::Target, &labels, cursor, &key(KeyCode::Char(c)))
                .unwrap();
        }
        // project-1, project-10, project-11 — and the cursor on the first.
        assert_eq!(
            visible(filter.query(ListId::Target), &labels),
            vec![1, 10, 11]
        );
        assert_eq!(cursor, 1);
        cursor = filter
            .on_key(ListId::Target, &labels, cursor, &key(KeyCode::Down))
            .unwrap();
        assert_eq!(cursor, 10, "down skips what the filter hid");
        cursor = filter
            .on_key(ListId::Target, &labels, cursor, &key(KeyCode::Down))
            .unwrap();
        cursor = filter
            .on_key(ListId::Target, &labels, cursor, &key(KeyCode::Down))
            .unwrap();
        assert_eq!(cursor, 11, "and stops at the end");

        // Esc clears the query first; only a second esc is the caller's.
        assert!(
            filter
                .on_key(ListId::Target, &labels, cursor, &key(KeyCode::Esc))
                .is_some()
        );
        assert_eq!(filter.query(ListId::Target), "");
        assert!(
            filter
                .on_key(ListId::Target, &labels, cursor, &key(KeyCode::Esc))
                .is_none()
        );
    }

    #[test]
    fn a_short_list_does_not_take_typing() {
        let mut filter = ListFilter::default();
        let labels = labels(SEARCHABLE_AFTER);
        assert_eq!(
            filter.on_key(ListId::Target, &labels, 0, &key(KeyCode::Char('x'))),
            None
        );
        assert_eq!(
            filter.on_key(ListId::Target, &labels, 0, &key(KeyCode::Down)),
            Some(1)
        );
    }

    #[test]
    fn each_list_starts_with_an_empty_filter() {
        let mut filter = ListFilter::default();
        let labels = labels(12);
        filter.on_key(ListId::Target, &labels, 0, &key(KeyCode::Char('9')));
        assert_eq!(filter.query(ListId::Target), "9");
        assert_eq!(filter.query(ListId::SettingsProject), "");
        filter.on_key(ListId::SettingsProject, &labels, 0, &key(KeyCode::Down));
        assert_eq!(filter.query(ListId::Target), "", "moving on forgets it");
    }
}
