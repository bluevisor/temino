//! Scrollback search (Command+F): every block's output searched for the
//! query, the matches marked in the grid and one of them the current one,
//! scrolled into view. Smart case: lowercase queries ignore case.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::Flags;

use crate::session::Block;

/// How often a block still printing is searched again.
const RESCAN: Duration = Duration::from_millis(250);

/// One match: output line and columns [start, end) of a block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub block: u64,
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Default)]
pub struct Find {
    pub query: String,
    /// The whole query is selected: typing replaces it.
    pub selected: bool,
    /// Per block: the output generation searched, when, and its matches.
    scanned: HashMap<u64, (u64, Instant, Vec<Match>)>,
    /// The query the cache was built for.
    scanned_query: String,
    /// Every match, oldest first, and the current one.
    pub matches: Vec<Match>,
    pub current: Option<Match>,
    /// The current match moved: scroll it into view on the next frame.
    pub reveal: bool,
}

impl Find {
    /// Opens with the last query selected, so typing starts a new one.
    pub fn new(query: String) -> Find {
        Find { selected: !query.is_empty(), query, ..Find::default() }
    }

    /// Searches the blocks again where their output changed (or the query
    /// did), keeping the current match where it was. A new query starts at
    /// the newest match.
    pub fn refresh(&mut self, blocks: &[Block]) {
        let now = Instant::now();
        let query_changed = self.scanned_query != self.query;
        if query_changed {
            self.scanned.clear();
            self.scanned_query = self.query.clone();
        }
        let needle: Vec<char> = self.query.chars().collect();
        let fold = !self.query.chars().any(char::is_uppercase);
        self.scanned.retain(|id, _| blocks.iter().any(|b| b.id == *id));
        let mut changed = query_changed;
        for block in blocks {
            let stale = match self.scanned.get(&block.id) {
                None => true,
                Some((generation, at, _)) => *generation != block.generation && now.duration_since(*at) >= RESCAN,
            };
            if stale {
                let found = if needle.is_empty() { Vec::new() } else { search_block(block, &needle, fold) };
                self.scanned.insert(block.id, (block.generation, now, found));
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.matches = blocks.iter().flat_map(|b| self.scanned[&b.id].2.iter().copied()).collect();
        let keep = self.current.filter(|c| !query_changed && self.matches.contains(c));
        if keep.is_none() {
            self.current = self.matches.last().copied();
            self.reveal = self.current.is_some();
        }
    }

    /// Moves to the next match (`older` goes up the scrollback), wrapping.
    pub fn step(&mut self, older: bool) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        let index = self.current.and_then(|c| self.matches.iter().position(|m| *m == c));
        let next = match (index, older) {
            (Some(i), true) => (i + n - 1) % n,
            (Some(i), false) => (i + 1) % n,
            (None, _) => n - 1,
        };
        self.current = Some(self.matches[next]);
        self.reveal = true;
    }

    /// "3 of 12", "No matches", or nothing before anything's typed.
    pub fn status(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        match self.current.and_then(|c| self.matches.iter().position(|m| *m == c)) {
            Some(i) => format!("{} of {}", i + 1, self.matches.len()),
            None => "No matches".to_owned(),
        }
    }

    /// The matches in one block's output lines [first, last), with whether
    /// each is the current one: (line, start, end, current).
    pub fn marks(&self, block: u64, first: usize, last: usize) -> Vec<(usize, usize, usize, bool)> {
        let Some((_, _, found)) = self.scanned.get(&block) else { return Vec::new() };
        found
            .iter()
            .filter(|m| (first..last).contains(&m.line))
            .map(|m| (m.line, m.start, m.end, Some(*m) == self.current))
            .collect()
    }
}

fn search_block(block: &Block, needle: &[char], fold: bool) -> Vec<Match> {
    let grid = block.term.grid();
    let columns = grid.columns();
    let lines = grid.history_size() + grid.screen_lines();
    let mut found = Vec::new();
    let mut row: Vec<(char, usize)> = Vec::with_capacity(columns);
    for line in 0..lines {
        row.clear();
        let cells = &grid[block.grid_line(line)];
        for column in 0..columns {
            let cell = &cells[Column(column)];
            if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                row.push((cell.c, column));
            }
        }
        for (start, end) in find_in(&row, needle, fold) {
            let last = row[end - 1];
            let wide = grid[block.grid_line(line)][Column(last.1)].flags.contains(Flags::WIDE_CHAR);
            found.push(Match { block: block.id, line, start: row[start].1, end: last.1 + 1 + wide as usize });
        }
    }
    found
}

/// Where `needle` occurs in `row`, as [start, end) indices into it, not
/// overlapping.
fn find_in(row: &[(char, usize)], needle: &[char], fold: bool) -> Vec<(usize, usize)> {
    let same = |a: char, b: char| if fold { a.to_lowercase().eq(b.to_lowercase()) } else { a == b };
    let mut found = Vec::new();
    if needle.is_empty() || row.len() < needle.len() {
        return found;
    }
    let mut i = 0;
    while i + needle.len() <= row.len() {
        if needle.iter().enumerate().all(|(k, &c)| same(row[i + k].0, c)) {
            found.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<(char, usize)> {
        text.chars().enumerate().map(|(i, c)| (c, i)).collect()
    }

    #[test]
    fn lowercase_ignores_case_and_matches_do_not_overlap() {
        let needle: Vec<char> = "aa".chars().collect();
        assert_eq!(find_in(&row("AAAa"), &needle, true), vec![(0, 2), (2, 4)]);
        assert_eq!(find_in(&row("AAAa"), &needle, false), vec![]);
    }

    #[test]
    fn stepping_wraps_both_ways() {
        let m = |line| Match { block: 1, line, start: 0, end: 1 };
        let mut find = Find { query: "x".into(), matches: vec![m(0), m(1), m(2)], ..Find::default() };
        find.current = Some(m(2));
        find.step(false);
        assert_eq!(find.current, Some(m(0)));
        find.step(true);
        assert_eq!(find.current, Some(m(2)));
        assert_eq!(find.status(), "3 of 3");
    }
}
