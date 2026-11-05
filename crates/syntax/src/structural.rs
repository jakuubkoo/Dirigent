//! Structural diff of two syntax trees: which tokens changed, which blocks moved, and which line on the other side
//! each line corresponds to. Clean-room implementation of GumTree-style matching (Falleri et al., "Fine-grained and
//! accurate source code differencing", ASE 2014), anchored by the line diff:
//! 1. subtrees lying wholly on unchanged lines map to the subtree at the same place on the other side;
//! 2. containers whose first and last lines are unchanged map to the same-kind node spanning the same place;
//! 3. top-down: the remaining identical subtrees (height ≥ 2), tallest first, preferring the candidate under the
//!    same mapped ancestor, then the nearest one;
//! 4. bottom-up: an unmatched container maps to the same-kind ancestor of its mapped descendants' partners with the
//!    best dice (≥ 0.5); inside every mapped container pair, unmatched children map by hash, then by kind, in order;
//! 5. moves: a mapped subtree under a different mapped ancestor, or out of order among its siblings. Only big ones
//!    (≥ 2 lines, ≥ 16 tokens) are moves; smaller ones are unmapped (changed on both sides).
//!
//! Whitespace isn't part of any token and trailing commas aren't hashed, so reformatting maps completely.
//! Pure and blocking: run it off the UI thread.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use imara_diff::{Algorithm, InternedInput};
use tree_sitter::Tree;

/// Matching time per file, on top of parsing.
pub const BUDGET: Duration = Duration::from_millis(150);
const MAX_NODES: usize = 400_000;
const MIN_HEIGHT: u32 = 2;
const MIN_DICE: f32 = 0.5;
const MOVE_MIN_LEAVES: u32 = 16;
/// Child lists bigger than this (product of both sides) aren't recovered by LCS.
const MAX_LCS_CELLS: usize = 200_000;
const NONE: u32 = u32::MAX;

/// Why the structural diff wasn't made (the view falls back to the word diff).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    /// No grammar worth diffing structurally (or no grammar at all).
    Unsupported,
    ParseError,
    TooBig,
    TooSlow,
    Cancelled,
    /// The texts don't fit the line diff (the file changed meanwhile).
    Mismatch,
    /// A new or deleted file: nothing to match.
    OneSided,
}

impl Fail {
    pub fn reason(self) -> &'static str {
        match self {
            Fail::Unsupported => "No structural diff for this file type",
            Fail::ParseError => "Syntax error in one of the versions",
            Fail::TooBig => "Too big for a structural diff",
            Fail::TooSlow => "Structural diff took too long",
            Fail::Cancelled => "Cancelled",
            Fail::Mismatch => "File changed while diffing",
            Fail::OneSided => "New or deleted file: nothing to match",
        }
    }
}

/// One version of the file: its text, its parse tree, and which of its lines the line diff marks as changed
/// (removed on the old side, added on the new side).
#[derive(Clone, Copy)]
pub struct Side<'a> {
    pub text: &'a str,
    pub tree: &'a Tree,
    pub changed: &'a [bool],
}

/// Per line: (byte range within the raw line, moved). moved = false: a changed token; true: part of a moved block.
pub type Marks = Vec<(Range<u32>, bool)>;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SideResult {
    pub marks: Vec<Marks>,
    /// The other side's line (0-based) holding most of this line's unchanged tokens.
    pub partner: Vec<Option<u32>>,
    /// (first line of a moved block on this side, its first line on the other side), 0-based.
    pub moved: Vec<(u32, u32)>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Structural {
    pub old: SideResult,
    pub new: SideResult,
}

struct Budget<'a> {
    cancel: &'a AtomicUsize,
    deadline: Instant,
    steps: u32,
}

impl Budget<'_> {
    fn tick(&mut self) -> Result<(), Fail> {
        self.steps += 1;
        if self.steps.is_multiple_of(4096) {
            if self.cancel.load(Ordering::Relaxed) != 0 {
                return Err(Fail::Cancelled);
            }
            if Instant::now() > self.deadline {
                return Err(Fail::TooSlow);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Node {
    kind: u16,
    start: u32,
    end: u32,
    /// Lines of the first and the last byte.
    line0: u32,
    line1: u32,
    parent: u32,
    /// Nodes in the subtree, itself included (pre-order: the subtree is `ix..ix + size`).
    size: u32,
    height: u32,
    leaves: u32,
    hash: u64,
    leaf: bool,
    /// Neighboring siblings (trailing commas left out), NONE at the ends: an attribute or doc comment belongs to
    /// the item after it.
    prev: u32,
    next: u32,
    /// A trailing comma: not hashed, never mapped, never marked.
    skip: bool,
}

struct Arena<'a> {
    text: &'a str,
    nodes: Vec<Node>,
    line_starts: Vec<u32>,
    /// changed_before[l] = changed lines before line l.
    changed_before: Vec<u32>,
}

impl Arena<'_> {
    fn unchanged(&self, a: u32, b: u32) -> bool {
        self.changed_before[b as usize + 1] == self.changed_before[a as usize]
    }

    fn line_changed(&self, l: u32) -> bool {
        !self.unchanged(l, l)
    }

    fn text(&self, ix: u32) -> &str {
        let n = &self.nodes[ix as usize];
        &self.text[n.start as usize..n.end as usize]
    }

    fn children(&self, ix: u32) -> Children<'_> {
        let n = &self.nodes[ix as usize];
        Children { nodes: &self.nodes, next: ix + 1, end: ix + n.size }
    }

    /// Children that take part in matching (no trailing commas).
    fn kids(&self, ix: u32) -> Vec<u32> {
        self.children(ix).filter(|&c| !self.nodes[c as usize].skip).collect()
    }

    /// Nodes starting at `byte`, outermost first.
    fn at(&self, byte: u32) -> impl Iterator<Item = u32> + '_ {
        let first = self.nodes.partition_point(|n| n.start < byte);
        (first..self.nodes.len()).take_while(move |&i| self.nodes[i].start == byte).map(|i| i as u32)
    }
}

struct Children<'a> {
    nodes: &'a [Node],
    next: u32,
    end: u32,
}

impl Iterator for Children<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        (self.next < self.end).then(|| {
            let c = self.next;
            self.next += self.nodes[c as usize].size;
            c
        })
    }
}

/// Kinds diffed as one token (sub-diffed by words when both sides changed).
fn atom_kind(kind: &str) -> bool {
    kind.contains("string") || kind.contains("comment") || kind == "char_literal" || kind == "heredoc_body"
}

const ATOM: u8 = 1;
const COMMENT: u8 = 2;
const KNOWN: u8 = 4;

fn flatten<'a>(text: &'a str, tree: &Tree, changed: &[bool], budget: &mut Budget) -> Result<Arena<'a>, Fail> {
    let bytes = text.as_bytes();
    let line_starts: Vec<u32> =
        std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i as u32 + 1)).collect();
    let mut changed_before = Vec::with_capacity(line_starts.len() + 1);
    changed_before.push(0);
    for l in 0..line_starts.len() {
        let c = changed.get(l).copied().unwrap_or(false) as u32;
        changed_before.push(changed_before[l] + c);
    }
    // Per kind id: ATOM / COMMENT flags, looked up once per kind.
    let mut kinds = vec![0u8; tree.language().node_kind_count() + 1];
    let mut nodes: Vec<Node> = Vec::new();
    let mut cursor = tree.walk();
    let mut stack: Vec<u32> = Vec::new();
    let mut kids: Vec<u32> = Vec::new();
    let gap = |a: u32, b: u32| bytes[a as usize..b as usize].iter().any(|c| !c.is_ascii_whitespace());
    let leaf_hash = |kind: u16, comment: bool, start: u32, end: u32| {
        let t = &text[start as usize..end as usize];
        let mut h = DefaultHasher::new();
        kind.hash(&mut h);
        if comment {
            t.split_whitespace().for_each(|w| w.hash(&mut h));
        } else {
            t.hash(&mut h);
        }
        h.finish()
    };

    // Called once a node's subtree is complete (its size set): text no child covers turns it into one token
    // (e.g. a scalar's characters around its escapes); else trailing commas, hash, height, leaf count.
    let finish = |nodes: &mut Vec<Node>, ix: u32, kids: &mut Vec<u32>, comment: bool| {
        let n = nodes[ix as usize];
        kids.clear();
        let mut j = ix + 1;
        while j < ix + n.size {
            kids.push(j);
            j += nodes[j as usize].size;
        }
        let mut pos = n.start;
        let mut uncovered = false;
        for &c in kids.iter() {
            let c = &nodes[c as usize];
            uncovered |= c.start > pos && gap(pos, c.start);
            pos = pos.max(c.end);
        }
        if uncovered || (n.end > pos && gap(pos, n.end)) {
            nodes.truncate(ix as usize + 1);
            let n = &mut nodes[ix as usize];
            (n.size, n.height, n.leaves, n.leaf) = (1, 1, 1, true);
            n.hash = leaf_hash(n.kind, comment, n.start, n.end);
            return;
        }
        // A trailing comma (before a closing delimiter, or last) isn't part of the structure.
        let tok = |nodes: &Vec<Node>, c: u32| {
            let c = nodes[c as usize];
            if c.leaf && c.end - c.start == 1 { bytes[c.start as usize] } else { 0 }
        };
        for k in 0..kids.len() {
            if tok(nodes, kids[k]) == b','
                && kids.get(k + 1).is_none_or(|&nx| matches!(tok(nodes, nx), b')' | b']' | b'}' | b'>' | b'|'))
            {
                nodes[kids[k] as usize].skip = true;
            }
        }
        let mut last = NONE;
        for &c in kids.iter() {
            if nodes[c as usize].skip {
                continue;
            }
            nodes[c as usize].prev = last;
            if last != NONE {
                nodes[last as usize].next = c;
            }
            last = c;
        }
        let mut h = DefaultHasher::new();
        n.kind.hash(&mut h);
        let (mut height, mut leaves) = (0, 0);
        for &c in kids.iter() {
            let c = &nodes[c as usize];
            leaves += c.leaves;
            if !c.skip {
                c.hash.hash(&mut h);
                height = height.max(c.height);
            }
        }
        let n = &mut nodes[ix as usize];
        n.hash = h.finish();
        n.height = height + 1;
        n.leaves = leaves;
    };

    loop {
        budget.tick()?;
        let node = cursor.node();
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        let mut descended = false;
        if end > start {
            if nodes.len() >= MAX_NODES {
                return Err(Fail::TooBig);
            }
            let ix = nodes.len() as u32;
            let kind = node.kind_id();
            let flags = kinds.get_mut(kind as usize).map_or(KNOWN, |f| {
                if *f == 0 {
                    let k = node.kind();
                    *f = KNOWN | if atom_kind(k) { ATOM } else { 0 } | if k.contains("comment") { COMMENT } else { 0 };
                }
                *f
            });
            let leaf = flags & ATOM != 0 || node.child_count() == 0;
            let (p0, p1) = (node.start_position(), node.end_position());
            let line1 = if p1.column == 0 && p1.row > p0.row { p1.row - 1 } else { p1.row };
            nodes.push(Node {
                kind,
                start,
                end,
                line0: p0.row as u32,
                line1: line1 as u32,
                parent: stack.last().copied().unwrap_or(NONE),
                size: 1,
                height: 1,
                leaves: 1,
                hash: if leaf { leaf_hash(kind, flags & COMMENT != 0, start, end) } else { 0 },
                leaf,
                skip: false,
                prev: NONE,
                next: NONE,
            });
            if !leaf && cursor.goto_first_child() {
                stack.push(ix);
                descended = true;
            }
        }
        if descended {
            continue;
        }
        // Next sibling, else close parents until one has a next sibling.
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(Arena { text, nodes, line_starts, changed_before });
            }
            if let Some(ix) = stack.pop() {
                nodes[ix as usize].size = nodes.len() as u32 - ix;
                let comment = kinds.get(nodes[ix as usize].kind as usize).is_some_and(|f| f & COMMENT != 0);
                finish(&mut nodes, ix, &mut kids, comment);
            }
        }
    }
}

struct Matcher<'a> {
    a: Arena<'a>,
    b: Arena<'a>,
    /// old node → new node, new node → old node.
    pa: Vec<u32>,
    pb: Vec<u32>,
    /// Old nodes mapped as part of an identical subtree (no recovery needed inside).
    iso: Vec<bool>,
    /// Old nodes anchored by position only (small subtrees on lines git paired, which can be wrong when code
    /// moved): top-down matching may take them over.
    weak: Vec<bool>,
    /// Old line → new line for unchanged lines; for changed ones, the expected new line (for distances).
    line_map: Vec<u32>,
    approx: Vec<u32>,
}

impl Matcher<'_> {
    fn link(&mut self, a: u32, b: u32) {
        self.pa[a as usize] = b;
        self.pb[b as usize] = a;
    }

    /// Maps two identical subtrees node by node.
    /// Both pre-orders are the same tree once trailing commas (always leaves) are skipped, so they zip.
    fn link_iso(&mut self, a: u32, b: u32) {
        let (ea, eb) = (a + self.a.nodes[a as usize].size, b + self.b.nodes[b as usize].size);
        let (mut i, mut j) = (a, b);
        loop {
            while i < ea && self.a.nodes[i as usize].skip {
                i += 1;
            }
            while j < eb && self.b.nodes[j as usize].skip {
                j += 1;
            }
            if i >= ea || j >= eb {
                break;
            }
            self.link(i, j);
            self.iso[i as usize] = true;
            (i, j) = (i + 1, j + 1);
        }
    }

    /// Forgets every mapping inside an old subtree and inside a new subtree (both directions).
    fn unlink_within(&mut self, a: u32, b: u32) {
        for x in a..a + self.a.nodes[a as usize].size {
            let y = std::mem::replace(&mut self.pa[x as usize], NONE);
            if y != NONE {
                self.pb[y as usize] = NONE;
            }
            self.iso[x as usize] = false;
            self.weak[x as usize] = false;
        }
        for y in b..b + self.b.nodes[b as usize].size {
            let x = std::mem::replace(&mut self.pb[y as usize], NONE);
            if x != NONE {
                self.pa[x as usize] = NONE;
                self.iso[x as usize] = false;
                self.weak[x as usize] = false;
            }
        }
    }

    fn strong_a(&self, x: u32) -> bool {
        self.pa[x as usize] != NONE && !self.weak[x as usize]
    }

    fn strong_b(&self, y: u32) -> bool {
        let x = self.pb[y as usize];
        x != NONE && !self.weak[x as usize]
    }

    /// Where an old byte would be on the new side, if its line is unchanged.
    fn map_byte(&self, byte: u32, line: u32) -> Option<u32> {
        let nl = *self.line_map.get(line as usize)?;
        (nl != NONE).then(|| self.b.line_starts[nl as usize] + (byte - self.a.line_starts[line as usize]))
    }

    /// Nearest mapped proper ancestor (old side / new side).
    fn mapped_up_a(&self, mut x: u32) -> u32 {
        loop {
            x = self.a.nodes[x as usize].parent;
            if x == NONE || self.pa[x as usize] != NONE {
                return x;
            }
        }
    }

    fn mapped_up_b(&self, mut y: u32) -> u32 {
        loop {
            y = self.b.nodes[y as usize].parent;
            if y == NONE || self.pb[y as usize] != NONE {
                return y;
            }
        }
    }

    fn consistent(&self, a: u32, b: u32) -> bool {
        let (ua, ub) = (self.mapped_up_a(a), self.mapped_up_b(b));
        match (ua, ub) {
            (NONE, NONE) => true,
            (NONE, _) | (_, NONE) => false,
            _ => self.pa[ua as usize] == ub,
        }
    }

    fn distance(&self, a: u32, b: u32) -> u32 {
        let expected = self.approx[self.a.nodes[a as usize].line0 as usize];
        expected.abs_diff(self.b.nodes[b as usize].line0)
    }

    /// 1: identical subtrees on unchanged lines.
    fn anchor_identical(&mut self, budget: &mut Budget) -> Result<(), Fail> {
        let mut stack = vec![0u32];
        while let Some(x) = stack.pop() {
            budget.tick()?;
            let n = self.a.nodes[x as usize];
            if !n.skip && self.a.unchanged(n.line0, n.line1) {
                let target = self.map_byte(n.start, n.line0);
                let found = target.and_then(|t| {
                    self.b.at(t).find(|&y| {
                        let m = &self.b.nodes[y as usize];
                        m.hash == n.hash && m.leaf == n.leaf && self.pb[y as usize] == NONE && self.b.unchanged(m.line0, m.line1)
                    })
                });
                if let Some(y) = found {
                    self.link_iso(x, y);
                    if n.leaves < MOVE_MIN_LEAVES {
                        self.weak[x as usize..(x + n.size) as usize].fill(true);
                    }
                    continue;
                }
            }
            if !n.leaf {
                stack.extend(self.a.children(x));
            }
        }
        Ok(())
    }

    /// 2: containers whose first and last lines are unchanged ↔ the same kind spanning the same place.
    fn anchor_containers(&mut self, budget: &mut Budget) -> Result<(), Fail> {
        if self.pa[0] == NONE && self.a.nodes[0].kind == self.b.nodes[0].kind {
            self.link(0, 0);
        }
        let mut x = 0u32;
        while (x as usize) < self.a.nodes.len() {
            budget.tick()?;
            let n = self.a.nodes[x as usize];
            if self.iso[x as usize] {
                x += n.size;
                continue;
            }
            let found = (self.pa[x as usize] == NONE
                && !n.leaf
                && !self.a.line_changed(n.line0)
                && !self.a.line_changed(n.line1))
            .then(|| self.map_byte(n.start, n.line0).zip(self.map_byte(n.end - 1, n.line1)))
            .flatten()
            .and_then(|(s, e)| {
                self.b.at(s).find(|&y| {
                    let m = &self.b.nodes[y as usize];
                    m.kind == n.kind && !m.leaf && m.end == e + 1 && self.pb[y as usize] == NONE
                })
            });
            if let Some(y) = found {
                self.link(x, y);
            }
            x += 1;
        }
        Ok(())
    }

    /// 3: identical subtrees anywhere (tallest first).
    fn top_down(&mut self, budget: &mut Budget) -> Result<(), Fail> {
        // Prefix counts of strongly mapped nodes: a subtree is free when it holds none.
        let free = |n: usize, strong: &dyn Fn(u32) -> bool| {
            let mut c = Vec::with_capacity(n + 1);
            c.push(0u32);
            for i in 0..n as u32 {
                c.push(c.last().unwrap() + strong(i) as u32);
            }
            c
        };
        let ca = free(self.a.nodes.len(), &|x| self.strong_a(x));
        let cb = free(self.b.nodes.len(), &|y| self.strong_b(y));
        let unmapped = |c: &[u32], ix: usize, size: u32| c[ix + size as usize] == c[ix];
        let mut index: HashMap<u64, Vec<u32>> = HashMap::new();
        for (y, m) in self.b.nodes.iter().enumerate() {
            if m.height >= MIN_HEIGHT && !m.skip && unmapped(&cb, y, m.size) {
                index.entry(m.hash).or_default().push(y as u32);
            }
        }
        if index.is_empty() {
            return Ok(());
        }
        let mut todo: Vec<u32> = (0..self.a.nodes.len() as u32)
            .filter(|&x| {
                let n = &self.a.nodes[x as usize];
                n.height >= MIN_HEIGHT && !n.skip && unmapped(&ca, x as usize, n.size) && index.contains_key(&n.hash)
            })
            .collect();
        todo.sort_by_key(|&x| std::cmp::Reverse(self.a.nodes[x as usize].height));
        // Unambiguous ones first; with several identical candidates (repeated `#[derive(Clone)]`), wait until the
        // neighbors had their turn, then take the twin whose neighbor (the item it's attached to) is the partner of
        // ours; only then the nearest.
        let mut ambiguous = Vec::new();
        for x in todo {
            budget.tick()?;
            if self.strong_a(x) {
                continue;
            }
            let cands = self.candidates(x, &index, budget)?;
            // Here only a matching attached item decides: the previous sibling may not be mapped right yet.
            let attached = self.best_agreeing(x, &cands).filter(|&y| self.neighbors_agree(x, y) == 2);
            match (cands.len(), attached) {
                (0, _) => {}
                (1, _) => self.take(x, cands[0].2),
                (_, Some(y)) => self.take(x, y),
                _ => ambiguous.push(x),
            }
        }
        loop {
            let before = ambiguous.len();
            let mut left = Vec::new();
            for x in ambiguous {
                if self.strong_a(x) {
                    continue;
                }
                let cands = self.candidates(x, &index, budget)?;
                match self.best_agreeing(x, &cands) {
                    Some(y) => self.take(x, y),
                    None => left.push(x),
                }
            }
            ambiguous = left;
            if ambiguous.len() == before {
                break;
            }
        }
        for x in ambiguous {
            if !self.strong_a(x)
                && let Some(c) = self.candidates(x, &index, budget)?.into_iter().min_by_key(|c| (c.0, c.1))
            {
                self.take(x, c.2);
            }
        }
        Ok(())
    }

    /// Free identical candidates for `x`: (under another mapped ancestor, distance, node). Far ones only for
    /// move-sized subtrees.
    fn candidates(&self, x: u32, index: &HashMap<u64, Vec<u32>>, budget: &mut Budget) -> Result<Vec<(bool, u32, u32)>, Fail> {
        let n = self.a.nodes[x as usize];
        let big = n.leaves >= MOVE_MIN_LEAVES && n.line1 > n.line0;
        let mut out = Vec::new();
        for &y in index.get(&n.hash).map_or(&[][..], Vec::as_slice) {
            budget.tick()?;
            if self.strong_b(y) {
                continue;
            }
            let ok = self.consistent(x, y);
            if ok || big {
                out.push((!ok, self.distance(x, y), y));
            }
        }
        Ok(out)
    }

    /// How well y's neighbors match x's: 2 = the next sibling (the item an attribute or doc comment belongs to)
    /// is mapped to y's next one, 1 = only the previous ones agree, 0 = neither.
    fn neighbors_agree(&self, x: u32, y: u32) -> u8 {
        let (m, n) = (&self.a.nodes[x as usize], &self.b.nodes[y as usize]);
        let agree = |p: u32, q: u32| p != NONE && q != NONE && self.pa[p as usize] == q;
        if agree(m.next, n.next) {
            2
        } else {
            agree(m.prev, n.prev) as u8
        }
    }

    /// The candidate with the best-agreeing neighbors (if any agree), then the nearest.
    fn best_agreeing(&self, x: u32, cands: &[(bool, u32, u32)]) -> Option<u32> {
        cands
            .iter()
            .map(|c| (self.neighbors_agree(x, c.2), c))
            .filter(|(agree, _)| *agree > 0)
            .min_by_key(|(agree, c)| (std::cmp::Reverse(*agree), c.0, c.1))
            .map(|(_, c)| c.2)
    }

    /// Maps identical subtrees, replacing weak anchors inside either.
    fn take(&mut self, x: u32, y: u32) {
        if self.pa[x as usize] != y {
            self.unlink_within(x, y);
            self.link_iso(x, y);
        }
        let size = self.a.nodes[x as usize].size;
        self.weak[x as usize..(x + size) as usize].fill(false);
    }

    /// 4: containers by their mapped descendants, then recovery inside every mapped container pair.
    fn bottom_up(&mut self, budget: &mut Budget) -> Result<(), Fail> {
        let mut votes: HashMap<u32, u32> = HashMap::new();
        for x in (0..self.a.nodes.len() as u32).rev() {
            budget.tick()?;
            let n = self.a.nodes[x as usize];
            if n.leaf || self.iso[x as usize] {
                continue;
            }
            if self.pa[x as usize] == NONE {
                votes.clear();
                let mut d = x + 1;
                while d < x + n.size {
                    budget.tick()?;
                    let p = self.pa[d as usize];
                    if p == NONE {
                        d += 1;
                        continue;
                    }
                    let weight = self.a.nodes[d as usize].size;
                    let mut y = self.b.nodes[p as usize].parent;
                    let mut steps = 0;
                    while y != NONE && steps < 64 {
                        let m = &self.b.nodes[y as usize];
                        if m.kind == n.kind && self.pb[y as usize] == NONE {
                            *votes.entry(y).or_default() += weight;
                        }
                        y = m.parent;
                        steps += 1;
                    }
                    d += weight;
                }
                let best = votes
                    .iter()
                    .map(|(&y, &v)| (2. * v as f32 / (n.size + self.b.nodes[y as usize].size) as f32, y))
                    .filter(|&(dice, _)| dice >= MIN_DICE)
                    .max_by(|p, q| p.0.total_cmp(&q.0).then_with(|| self.distance(x, q.1).cmp(&self.distance(x, p.1))));
                if let Some((_, y)) = best {
                    self.link(x, y);
                }
            }
            let y = self.pa[x as usize];
            if y != NONE {
                self.recover(x, y, budget)?;
            }
        }
        Ok(())
    }

    /// Maps the unmatched children of a mapped pair: identical ones first, then same-kind ones, in order, between
    /// the children already mapped to each other. Same-kind containers are recovered in turn.
    fn recover(&mut self, a: u32, b: u32, budget: &mut Budget) -> Result<(), Fail> {
        let mut stack = vec![(a, b)];
        while let Some((x, y)) = stack.pop() {
            budget.tick()?;
            let (ka, kb) = (self.a.kids(x), self.b.kids(y));
            if ka.iter().all(|&c| self.pa[c as usize] != NONE) || kb.iter().all(|&c| self.pb[c as usize] != NONE) {
                continue;
            }
            let pos_b: HashMap<u32, usize> = kb.iter().enumerate().map(|(i, &c)| (c, i)).collect();
            let mapped: Vec<(usize, usize)> = ka
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| pos_b.get(&self.pa[c as usize]).map(|&k| (i, k)))
                .collect();
            let keep = lis(&mapped.iter().map(|p| p.1).collect::<Vec<_>>());
            let anchors: Vec<(usize, usize)> = keep.into_iter().map(|i| mapped[i]).collect();
            let (mut i0, mut k0) = (0, 0);
            for (i1, k1) in anchors.into_iter().chain(std::iter::once((ka.len(), kb.len()))) {
                let ua: Vec<u32> = ka[i0..i1].iter().copied().filter(|&c| self.pa[c as usize] == NONE).collect();
                let ub: Vec<u32> = kb[k0..k1].iter().copied().filter(|&c| self.pb[c as usize] == NONE).collect();
                (i0, k0) = (i1 + 1, k1 + 1);
                if ua.is_empty() || ub.is_empty() || ua.len() * ub.len() > MAX_LCS_CELLS {
                    continue;
                }
                for (p, q) in lcs(&ua, &ub, |p, q| self.a.nodes[p as usize].hash == self.b.nodes[q as usize].hash) {
                    self.link_iso(ua[p], ub[q]);
                }
                let ua: Vec<u32> = ua.into_iter().filter(|&c| self.pa[c as usize] == NONE).collect();
                let ub: Vec<u32> = ub.into_iter().filter(|&c| self.pb[c as usize] == NONE).collect();
                let same_kind = |p: u32, q: u32| {
                    let (m, n) = (&self.a.nodes[p as usize], &self.b.nodes[q as usize]);
                    m.kind == n.kind && m.leaf == n.leaf
                };
                for (p, q) in lcs(&ua, &ub, same_kind) {
                    self.link(ua[p], ub[q]);
                    if !self.a.nodes[ua[p] as usize].leaf {
                        stack.push((ua[p], ub[q]));
                    }
                }
            }
        }
        Ok(())
    }

    /// 4b: inside every mapped container pair, unmatched tokens that only gained or lost a wrapper (`to` →
    /// `Some(to)`, a comment moved into a new block): identical tokens in order, where at least one side's token
    /// sits directly in the mapped container.
    fn recover_leaves(&mut self, budget: &mut Budget) -> Result<(), Fail> {
        for x in 0..self.a.nodes.len() as u32 {
            let y = self.pa[x as usize];
            if y == NONE || self.iso[x as usize] || self.a.nodes[x as usize].leaf {
                continue;
            }
            let (fa, fb) = (frontier(&self.a, &self.pa, x, budget)?, frontier(&self.b, &self.pb, y, budget)?);
            if fa.is_empty() || fb.is_empty() || fa.len() * fb.len() > MAX_LCS_CELLS {
                continue;
            }
            let pairs = lcs(&fa, &fb, |p, q| {
                let (m, n) = (&self.a.nodes[p as usize], &self.b.nodes[q as usize]);
                m.hash == n.hash && (m.parent == x || n.parent == y)
            });
            for (p, q) in pairs {
                self.link(fa[p], fb[q]);
            }
        }
        Ok(())
    }

    /// 5: moved code (marked, listed); "moves" too small to show as such are unmapped (changed on both sides).
    fn moves(&mut self, budget: &mut Budget) -> Result<(Vec<bool>, MoveList), Fail> {
        let n = self.a.nodes.len();
        let (mut moved, mut list, mut handled) = (vec![false; n], Vec::new(), vec![false; n]);
        // Children out of order within mapped parents. The siblings that stay are the heaviest increasing
        // subsequence, where siblings on unchanged lines weigh most (git shows the others as removed/added, so the
        // move is shown there); the rest, in runs adjacent on both sides, moved.
        for x in 0..n as u32 {
            budget.tick()?;
            let y = self.pa[x as usize];
            if y == NONE || self.iso[x as usize] || self.a.nodes[x as usize].leaf {
                continue;
            }
            let kb = self.b.kids(y);
            let pos_b: HashMap<u32, usize> = kb.iter().enumerate().map(|(i, &c)| (c, i)).collect();
            let mapped: Vec<(u32, usize)> =
                self.a.kids(x).into_iter().filter_map(|c| pos_b.get(&self.pa[c as usize]).map(|&k| (c, k))).collect();
            if mapped.len() < 2 {
                continue;
            }
            let weight = |c: u32| {
                let m = &self.a.nodes[c as usize];
                if self.a.unchanged(m.line0, m.line1) { 1 << 32 } else { m.leaves as u64 }
            };
            let keep = heaviest_increasing(&mapped.iter().map(|p| (p.1, weight(p.0))).collect::<Vec<_>>());
            let mut i = 0;
            while i < mapped.len() {
                if keep[i] {
                    i += 1;
                    continue;
                }
                let mut j = i + 1;
                while j < mapped.len() && !keep[j] && mapped[j].1 == mapped[j - 1].1 + 1 {
                    j += 1;
                }
                let group: Vec<u32> = mapped[i..j].iter().map(|p| p.0).collect();
                self.move_or_unlink(&group, &mut moved, &mut list, &mut handled);
                i = j;
            }
        }
        // Subtrees under another mapped ancestor than their partner.
        let mut x = 1u32;
        while (x as usize) < n {
            budget.tick()?;
            let size = self.a.nodes[x as usize].size;
            if handled[x as usize] {
                x += size;
            } else if self.pa[x as usize] != NONE && !self.consistent(x, self.pa[x as usize]) {
                self.move_or_unlink(&[x], &mut moved, &mut list, &mut handled);
                x += size;
            } else {
                x += 1;
            }
        }
        Ok((moved, list))
    }

    /// Siblings (old side, adjacent, mapped to adjacent siblings) that moved together: a move when big enough
    /// (≥ 2 lines on both sides, ≥ `MOVE_MIN_LEAVES` tokens), else their mappings are dropped.
    fn move_or_unlink(&mut self, group: &[u32], moved: &mut [bool], list: &mut Vec<(u32, u32)>, handled: &mut [bool]) {
        let (first, last) = (group[0], *group.last().unwrap());
        let (fa, la) = (self.a.nodes[first as usize], self.a.nodes[last as usize]);
        let (fb, lb) = (self.b.nodes[self.pa[first as usize] as usize], self.b.nodes[self.pa[last as usize] as usize]);
        let leaves: u32 = group.iter().map(|&g| self.a.nodes[g as usize].leaves).sum();
        let big = leaves >= MOVE_MIN_LEAVES && la.line1 > fa.line0 && lb.line1.max(fb.line1) > fb.line0.min(lb.line0);
        if big {
            list.push((fa.line0, fb.line0));
        }
        for &g in group {
            let (size, y) = (self.a.nodes[g as usize].size, self.pa[g as usize]);
            let ysize = self.b.nodes[y as usize].size;
            handled[g as usize..(g + size) as usize].fill(true);
            if big {
                moved[g as usize..(g + size) as usize].fill(true);
                continue;
            }
            for d in g..g + size {
                let p = self.pa[d as usize];
                if p != NONE && (y..y + ysize).contains(&p) {
                    self.pa[d as usize] = NONE;
                    self.pb[p as usize] = NONE;
                }
            }
        }
    }
}

/// Unmapped leaves in `ix`'s subtree that aren't inside another mapped node, in order.
fn frontier(arena: &Arena, partner: &[u32], ix: u32, budget: &mut Budget) -> Result<Vec<u32>, Fail> {
    let end = ix + arena.nodes[ix as usize].size;
    let (mut d, mut out) = (ix + 1, Vec::new());
    while d < end {
        budget.tick()?;
        let n = &arena.nodes[d as usize];
        if partner[d as usize] != NONE {
            d += n.size;
            continue;
        }
        if n.leaf && !n.skip {
            out.push(d);
        }
        d += 1;
    }
    Ok(out)
}

/// Which items to keep: a strictly increasing subsequence of the positions with the highest total weight.
/// Quadratic, so long lists keep a plain longest one.
fn heaviest_increasing(items: &[(usize, u64)]) -> Vec<bool> {
    let n = items.len();
    let mut keep = vec![false; n];
    if n > 3000 {
        for i in lis(&items.iter().map(|p| p.0).collect::<Vec<_>>()) {
            keep[i] = true;
        }
        return keep;
    }
    let (mut best, mut prev) = (vec![0u64; n], vec![usize::MAX; n]);
    for i in 0..n {
        best[i] = items[i].1;
        for j in 0..i {
            if items[j].0 < items[i].0 && best[j] + items[i].1 > best[i] {
                best[i] = best[j] + items[i].1;
                prev[i] = j;
            }
        }
    }
    let mut i = (0..n).max_by_key(|&i| best[i]).unwrap_or(usize::MAX);
    while i != usize::MAX {
        keep[i] = true;
        i = prev[i];
    }
    keep
}

/// (first line on the old side, first line on the new side) per moved block.
type MoveList = Vec<(u32, u32)>;

/// Indices (into `v`) of a longest strictly increasing subsequence.
fn lis(v: &[usize]) -> Vec<usize> {
    let mut tails: Vec<usize> = Vec::new(); // index into v of the smallest tail for each length
    let mut prev = vec![usize::MAX; v.len()];
    for i in 0..v.len() {
        let k = tails.partition_point(|&t| v[t] < v[i]);
        if k > 0 {
            prev[i] = tails[k - 1];
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut i = tails.last().copied().unwrap_or(usize::MAX);
    while i != usize::MAX {
        out.push(i);
        i = prev[i];
    }
    out.reverse();
    out
}

/// Index pairs of a longest common subsequence under `eq`.
fn lcs(a: &[u32], b: &[u32], eq: impl Fn(u32, u32) -> bool) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if eq(a[i], b[j]) { dp[(i + 1) * w + j + 1] + 1 } else { dp[(i + 1) * w + j].max(dp[i * w + j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if eq(a[i], b[j]) && dp[i * w + j] == dp[(i + 1) * w + j + 1] + 1 {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Word-level changed ranges (relative byte ranges) of two differing atoms, e.g. a comment with one word edited.
fn atom_ranges(a: &str, b: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (ta, tb) = (word_tokens(a), word_tokens(b));
    if ta.len() <= 1 || tb.len() <= 1 || ta.len() * tb.len() > MAX_LCS_CELLS {
        return (std::iter::once(0..a.len()).collect(), std::iter::once(0..b.len()).collect());
    }
    let mut input = InternedInput::default();
    for r in &ta {
        let t = input.interner.intern(&a[r.clone()]);
        input.before.push(t);
    }
    for r in &tb {
        let t = input.interner.intern(&b[r.clone()]);
        input.after.push(t);
    }
    let diff = imara_diff::Diff::compute(Algorithm::Histogram, &input);
    let (mut ra, mut rb) = (Vec::new(), Vec::new());
    for h in diff.hunks() {
        ra.extend(ta[h.before.start as usize..h.before.end as usize].iter().cloned());
        rb.extend(tb[h.after.start as usize..h.after.end as usize].iter().cloned());
    }
    (ra, rb)
}

/// Word runs and single other characters; whitespace isn't a token.
fn word_tokens(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut in_word = false;
    for (i, ch) in s.char_indices() {
        let word = ch.is_alphanumeric() || ch == '_';
        if ch.is_whitespace() {
            in_word = false;
        } else if word && in_word {
            out.last_mut().unwrap().end = i + ch.len_utf8();
        } else {
            out.push(i..i + ch.len_utf8());
            in_word = word;
        }
    }
    out
}

/// Adds a whole-text byte range to the per-line marks (line breaks excluded).
fn mark(marks: &mut [Marks], arena: &Arena, range: Range<u32>, moved: bool) {
    let starts = &arena.line_starts;
    let mut line = starts.partition_point(|&s| s <= range.start) - 1;
    let mut pos = range.start;
    while pos < range.end && line < marks.len() {
        let ls = starts[line];
        let mut le = starts.get(line + 1).map_or(arena.text.len() as u32, |&s| s - 1);
        if le > ls && arena.text.as_bytes()[le as usize - 1] == b'\r' {
            le -= 1;
        }
        let end = range.end.min(le);
        if end > pos {
            marks[line].push((pos - ls..end - ls, moved));
        }
        line += 1;
        pos = starts.get(line).copied().unwrap_or(range.end);
    }
}

/// Sorts each line's marks and merges neighbors of the same kind separated only by whitespace.
fn tidy(marks: &mut [Marks], arena: &Arena) {
    for (l, m) in marks.iter_mut().enumerate() {
        if m.is_empty() {
            continue;
        }
        m.sort_by_key(|(r, _)| r.start);
        let base = arena.line_starts[l] as usize;
        let mut out: Marks = Vec::with_capacity(m.len());
        for (r, moved) in m.drain(..) {
            match out.last_mut() {
                Some((last, lm))
                    if *lm == moved
                        && last.end <= r.start
                        && arena.text[base + last.end as usize..base + r.start as usize].trim().is_empty() =>
                {
                    last.end = last.end.max(r.end)
                }
                Some((last, _)) if r.start < last.end => {} // overlap (nested atoms): keep the first
                _ => out.push((r, moved)),
            }
        }
        *m = out;
    }
}

/// Most frequent other-side line per line, from (this line, other line) votes.
fn partners(lines: usize, mut votes: Vec<(u32, u32)>) -> Vec<Option<u32>> {
    votes.sort_unstable();
    let mut out = vec![None; lines];
    let mut i = 0;
    while i < votes.len() {
        let line = votes[i].0;
        let (mut best, mut best_n) = (votes[i].1, 0);
        while i < votes.len() && votes[i].0 == line {
            let other = votes[i].1;
            let mut n = 0;
            while i < votes.len() && votes[i] == (line, other) {
                n += 1;
                i += 1;
            }
            if n > best_n {
                (best, best_n) = (other, n);
            }
        }
        if let Some(slot) = out.get_mut(line as usize) {
            *slot = Some(best);
        }
    }
    out
}

/// Whether an error or missing node spans one of the changed lines.
fn errors_touch(tree: &Tree, changed: &[bool]) -> bool {
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            let (p0, p1) = (n.start_position(), n.end_position());
            let last = if p1.column == 0 && p1.row > p0.row { p1.row - 1 } else { p1.row };
            if (p0.row..=last).any(|l| changed.get(l).copied().unwrap_or(false)) {
                return true;
            }
        } else if n.has_error() {
            stack.extend(n.children(&mut cursor));
        }
    }
    false
}

/// Diffs two versions of a file. `changed` must come from the line diff of exactly these texts.
pub fn diff(old: Side, new: Side, cancel: &AtomicUsize, budget: Duration) -> Result<Structural, Fail> {
    // Syntax errors elsewhere are fine (grammars miss some valid code too); one touching a changed line would
    // make the matching around the change unreliable.
    if errors_touch(old.tree, old.changed) || errors_touch(new.tree, new.changed) {
        return Err(Fail::ParseError);
    }
    let mut budget = Budget { cancel, deadline: Instant::now() + budget, steps: 0 };
    // Both sides at once (each with its own step counter, same deadline and cancel flag).
    let (a, b) = std::thread::scope(|sc| {
        let deadline = budget.deadline;
        let a = sc.spawn(move || flatten(old.text, old.tree, old.changed, &mut Budget { cancel, deadline, steps: 0 }));
        let b = flatten(new.text, new.tree, new.changed, &mut Budget { cancel, deadline, steps: 0 });
        (a.join().unwrap_or(Err(Fail::Unsupported)), b)
    });
    let (a, b) = (a?, b?);
    if a.nodes.is_empty() || b.nodes.is_empty() {
        return Err(Fail::Unsupported);
    }
    // Unchanged lines correspond in order.
    let keep = |arena: &Arena| (0..arena.line_starts.len() as u32).filter(|&l| !arena.line_changed(l)).collect::<Vec<_>>();
    let (ka, kb) = (keep(&a), keep(&b));
    if ka.len().abs_diff(kb.len()) > 1 {
        return Err(Fail::Mismatch);
    }
    let mut line_map = vec![NONE; a.line_starts.len()];
    for (&o, &n) in ka.iter().zip(&kb) {
        line_map[o as usize] = n;
    }
    let mut approx = vec![0u32; a.line_starts.len()];
    let (mut last_o, mut last_n) = (0i64, 0i64);
    for l in 0..approx.len() {
        if line_map[l] != NONE {
            (last_o, last_n) = (l as i64, line_map[l] as i64);
        }
        approx[l] = (last_n + (l as i64 - last_o)).max(0) as u32;
    }
    let (na, nb) = (a.nodes.len(), b.nodes.len());
    let mut m = Matcher { a, b, pa: vec![NONE; na], pb: vec![NONE; nb], iso: vec![false; na], weak: vec![false; na], line_map, approx };
    m.anchor_identical(&mut budget)?;
    m.anchor_containers(&mut budget)?;
    m.top_down(&mut budget)?;
    m.bottom_up(&mut budget)?;
    m.recover_leaves(&mut budget)?;
    let (moved_a, moves) = m.moves(&mut budget)?;

    let (la, lb) = (m.a.line_starts.len(), m.b.line_starts.len());
    let (mut marks_a, mut marks_b) = (vec![Marks::new(); la], vec![Marks::new(); lb]);
    let (mut votes_a, mut votes_b) = (Vec::new(), Vec::new());
    for (x, (&n, &y)) in m.a.nodes.iter().zip(&m.pa).enumerate() {
        budget.tick()?;
        if !n.leaf || n.skip {
            continue;
        }
        if y == NONE {
            mark(&mut marks_a, &m.a, n.start..n.end, false);
            continue;
        }
        let o = m.b.nodes[y as usize];
        let (ta, tb) = (m.a.text(x as u32), m.b.text(y));
        if ta != tb && n.hash != o.hash {
            let (ra, rb) = atom_ranges(ta, tb);
            for r in ra {
                mark(&mut marks_a, &m.a, n.start + r.start as u32..n.start + r.end as u32, false);
            }
            for r in rb {
                mark(&mut marks_b, &m.b, o.start + r.start as u32..o.start + r.end as u32, false);
            }
        } else if moved_a[x] {
            mark(&mut marks_a, &m.a, n.start..n.end, true);
            mark(&mut marks_b, &m.b, o.start..o.end, true);
        } else {
            votes_a.push((n.line0, o.line0));
            votes_b.push((o.line0, n.line0));
        }
    }
    for y in 0..nb {
        budget.tick()?;
        let n = m.b.nodes[y];
        if n.leaf && !n.skip && m.pb[y] == NONE {
            mark(&mut marks_b, &m.b, n.start..n.end, false);
        }
    }
    tidy(&mut marks_a, &m.a);
    tidy(&mut marks_b, &m.b);
    Ok(Structural {
        old: SideResult { marks: marks_a, partner: partners(la, votes_a), moved: moves.clone() },
        new: SideResult { marks: marks_b, partner: partners(lb, votes_b), moved: moves.iter().map(|&(o, n)| (n, o)).collect() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lang;

    /// Changed lines per side, from a line diff (what git reports for the same texts).
    pub(crate) fn changed_lines(old: &str, new: &str) -> (Vec<bool>, Vec<bool>) {
        let input = InternedInput::new(old, new);
        let diff = imara_diff::Diff::compute(Algorithm::Histogram, &input);
        let (mut a, mut b) = (vec![false; input.before.len() + 1], vec![false; input.after.len() + 1]);
        for h in diff.hunks() {
            a[h.before.start as usize..h.before.end as usize].fill(true);
            b[h.after.start as usize..h.after.end as usize].fill(true);
        }
        (a, b)
    }

    pub(crate) fn run(lang: Lang, old: &str, new: &str) -> Result<Structural, Fail> {
        let flag = AtomicUsize::new(0);
        let (ta, tb) = (crate::parse(lang, old, &flag, Instant::now()).unwrap(), crate::parse(lang, new, &flag, Instant::now()).unwrap());
        let (ca, cb) = changed_lines(old, new);
        diff(Side { text: old, tree: &ta, changed: &ca }, Side { text: new, tree: &tb, changed: &cb }, &flag, Duration::from_secs(5))
    }

    /// Marked text per side: (changed, moved).
    fn marked(text: &str, side: &SideResult) -> (Vec<String>, Vec<String>) {
        let lines: Vec<&str> = text.split('\n').collect();
        let (mut c, mut m) = (Vec::new(), Vec::new());
        for (l, marks) in side.marks.iter().enumerate() {
            for (r, moved) in marks {
                let s = lines[l][r.start as usize..r.end as usize].to_string();
                if *moved { m.push(s) } else { c.push(s) }
            }
        }
        (c, m)
    }

    fn changed(text: &str, side: &SideResult) -> Vec<String> {
        marked(text, side).0
    }

    #[test]
    fn rename_marks_only_the_identifier() {
        let old = "fn a() {\n    let foo = 1;\n    bar(foo);\n}\n";
        let new = "fn a() {\n    let baz = 1;\n    bar(baz);\n}\n";
        let s = run(Lang::Rust, old, new).unwrap();
        assert_eq!(changed(old, &s.old), ["foo", "foo"]);
        assert_eq!(changed(new, &s.new), ["baz", "baz"]);
        assert_eq!(s.old.partner[1], Some(1));
    }

    #[test]
    fn reformatting_marks_nothing() {
        let old = "fn a() {\n    let x = call(first_argument, second_argument, third_argument);\n}\n";
        let new = "fn a() {\n    let x = call(\n        first_argument,\n        second_argument,\n        third_argument,\n    );\n}\n";
        let s = run(Lang::Rust, old, new).unwrap();
        assert!(s.old.marks.iter().all(Vec::is_empty), "{:?}", s.old.marks);
        assert!(s.new.marks.iter().all(Vec::is_empty), "{:?}", s.new.marks);
        assert_eq!(s.old.partner[1], Some(1), "the old line pairs with the first new line");
    }

    #[test]
    fn added_argument_and_wrapper() {
        let old = "fn a() {\n    call(a, b);\n    let x = foo(y);\n}\n";
        let new = "fn a() {\n    call(a, b, c);\n    let x = Some(foo(y));\n}\n";
        let s = run(Lang::Rust, old, new).unwrap();
        assert!(changed(old, &s.old).is_empty(), "{:?}", changed(old, &s.old));
        assert_eq!(changed(new, &s.new), [", c", "Some(", ")"]);
    }

    #[test]
    fn moved_function_is_a_move_not_a_change() {
        let f = "fn moved(a: u32, b: u32) -> u32 {\n    let c = a + b * 2;\n    c - 1\n}\n";
        let g = "fn other() {\n    println!(\"x\");\n}\n";
        let h = "fn third() -> bool {\n    true\n}\n";
        let old = format!("{f}\n{g}\n{h}");
        let new = format!("{g}\n{h}\n{f}");
        let s = run(Lang::Rust, &old, &new).unwrap();
        let (c_old, m_old) = marked(&old, &s.old);
        let (c_new, m_new) = marked(&new, &s.new);
        assert!(c_old.is_empty() && c_new.is_empty(), "{c_old:?} {c_new:?}");
        assert!(!m_old.is_empty() && !m_new.is_empty());
        assert_eq!(s.old.moved.len(), 1, "{:?}", s.old.moved);
        assert_eq!(s.new.moved, [(s.old.moved[0].1, s.old.moved[0].0)]);
    }

    /// A moved group of items each with the same `#[derive(Clone)]`: every attribute stays with its item (the twin
    /// next to the item's partner), and the group is one move with nothing changed.
    #[test]
    fn moved_group_keeps_attributes_with_their_items() {
        let group = "#[derive(Clone)]\npub struct Buffer {\n    text: String,\n    version: u64,\n}\n\n\
                     #[derive(Clone)]\npub struct Snapshot {\n    text: String,\n    lines: Vec<u32>,\n}\n\n\
                     #[derive(Clone)]\npub struct Selection {\n    start: usize,\n    end: usize,\n}\n";
        let other = "#[derive(Clone)]\npub struct UndoMap {\n    entries: Vec<(u64, u64)>,\n}\n\n\
                     impl UndoMap {\n    pub fn new() -> Self {\n        Self { entries: Vec::new() }\n    }\n}\n";
        let old = format!("use std::sync::Arc;\n\n{other}\n{group}");
        let new = format!("use std::sync::Arc;\n\n{group}\n{other}");
        let s = run(Lang::Rust, &old, &new).unwrap();
        let (c_old, m_old) = marked(&old, &s.old);
        let (c_new, _) = marked(&new, &s.new);
        assert!(c_old.is_empty() && c_new.is_empty(), "changed: {c_old:?} {c_new:?}");
        assert!(!m_old.is_empty());
        assert_eq!(s.old.moved.len(), 1, "one block: {:?}", s.old.moved);
    }

    #[test]
    fn small_swaps_are_changes_not_moves() {
        let old = "fn a() {\n    x();\n    y();\n}\n";
        let new = "fn a() {\n    y();\n    x();\n}\n";
        let s = run(Lang::Rust, old, new).unwrap();
        assert!(s.old.moved.is_empty());
        assert!(!changed(old, &s.old).is_empty());
    }

    #[test]
    fn comment_edit_marks_the_word() {
        let old = "// the quick brown fox\nfn a() {}\n";
        let new = "// the slow brown fox\nfn a() {}\n";
        let s = run(Lang::Rust, old, new).unwrap();
        assert_eq!(changed(old, &s.old), ["quick"]);
        assert_eq!(changed(new, &s.new), ["slow"]);
    }

    #[test]
    fn edit_in_one_of_two_twins_stays_there() {
        let body = "    let v = compute(1, 2, 3);\n    v + 1\n";
        let old = format!("fn f() -> u32 {{\n{body}}}\n\nfn g() -> u32 {{\n{body}}}\n");
        let new = format!("fn f() -> u32 {{\n{body}}}\n\nfn g() -> u32 {{\n{}}}\n", body.replace("v + 1", "v + 2"));
        let s = run(Lang::Rust, &old, &new).unwrap();
        assert_eq!(changed(&old, &s.old), ["1"]);
        assert_eq!(changed(&new, &s.new), ["2"]);
        assert!(s.old.marks[7].len() == 1, "on g's line");
    }

    #[test]
    fn other_languages() {
        let s = run(Lang::Php, "<?php\n$a = foo($b);\n", "<?php\n$a = foo($b, $c);\n").unwrap();
        assert_eq!(changed("<?php\n$a = foo($b, $c);\n", &s.new), [", $c"]);
        let ts_old = "const x = { a: 1, b: \"two words\" };\n";
        let ts_new = "const x = { a: 1, b: \"two birds\" };\n";
        let s = run(Lang::TypeScript, ts_old, ts_new).unwrap();
        assert_eq!(changed(ts_old, &s.old), ["words"]);
        assert_eq!(changed(ts_new, &s.new), ["birds"]);
    }

    #[test]
    fn parse_errors_and_cancel() {
        assert_eq!(run(Lang::Rust, "fn a() {}\n", "fn a( {}\n").err(), Some(Fail::ParseError));
        // An error away from the change doesn't matter.
        let broken = "fn a() {\n    let x = 1;\n}\n\nfn b( {}\n";
        let s = run(Lang::Rust, broken, &broken.replace("x = 1", "x = 2")).unwrap();
        assert_eq!(changed(broken, &s.old), ["1"]);
        let flag = AtomicUsize::new(1);
        let big: String = (0..3000).map(|i| format!("fn f{i}() {{ let x = {i}; }}\n")).collect();
        let t = crate::parse(Lang::Rust, &big, &AtomicUsize::new(0), Instant::now()).unwrap();
        let ch = vec![false; 3001];
        let side = Side { text: &big, tree: &t, changed: &ch };
        assert_eq!(diff(side, side, &flag, Duration::from_secs(5)).err(), Some(Fail::Cancelled));
        assert_eq!(diff(side, side, &AtomicUsize::new(0), Duration::ZERO).err(), Some(Fail::TooSlow));
    }
}
