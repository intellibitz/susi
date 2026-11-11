//! Matcher for the fixed claim patterns of the truth gate
//! (T-SMALLFACADES-1: replaces the `regex` facade in the pillar closure).
//!
//! This is not a general regex engine. It compiles the small syntax the
//! static patterns in `truth.rs` and `verification.rs` are written in, and
//! refuses everything else with an error, so a pattern outside the subset
//! fails at its first use instead of matching differently:
//!
//! - a leading `(?i)` (case-insensitive for the whole pattern), nowhere else;
//! - ASCII literals and escaped ASCII punctuation (`\.`, `\-`, ...);
//! - `.` (any char but `\n`), `\s` (Unicode `White_Space`, which is
//!   `char::is_whitespace`), `\n`, `\t`;
//! - classes `[...]` and `[^...]` of ASCII chars, ASCII ranges and `\s`;
//! - `(...)` capture groups, `(?:...)` groups, `|`;
//! - `*`, `+`, `?`, each greedy or lazy (`*?`, `+?`, `??`);
//! - `\b` and `$` (end of text: the patterns set no multi-line flag).
//!
//! Matching follows the `regex` crate's semantics for this subset:
//! leftmost-first (a Pike VM keeps threads in priority order, so the match
//! and its groups are the ones a backtracker would find first, in time linear
//! in the text), iteration resumes at the end of the previous match, and
//! positions are byte offsets on char boundaries. Case-insensitive matching
//! applies Unicode simple case folding, which for ASCII letters adds exactly
//! two non-ASCII chars: U+212A KELVIN SIGN (k) and U+017F LONG S (s).
//!
//! One documented difference: `\b` treats a char as a word char when it is
//! alphanumeric (`char::is_alphanumeric`) or `_`. The `regex` crate uses
//! Unicode `\w`, which also counts combining marks, connector punctuation
//! other than `_` and the zero-width joiners, and does not count numbers
//! outside `Nd` (for example `²`, `½`). The two agree on every ASCII char;
//! they can disagree only where one of those chars sits directly against a
//! keyword of a pattern.

/// One compiled pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    prog: Vec<Inst>,
    groups: usize,
    /// The char tests a match can begin with, when every match consumes a
    /// char: positions no test accepts cannot start a match and are skipped.
    first: Option<Vec<CharTest>>,
}

#[derive(Debug, Clone)]
enum Inst {
    Char(CharTest),
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    WordBoundary,
    End,
    Match,
}

#[derive(Debug, Clone)]
enum CharTest {
    /// Any char except `\n`.
    Any,
    /// `ascii` holds the answer for every ASCII char (bit `c`), already
    /// negated; `items` answer the rest.
    Set {
        ascii: u128,
        items: Vec<Item>,
        negated: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Range(char, char),
    Space,
}

impl CharTest {
    fn matches(&self, c: char) -> bool {
        match self {
            CharTest::Any => c != '\n',
            CharTest::Set {
                ascii,
                items,
                negated,
            } => match u32::from(c) {
                code if code < 128 => ascii & (1 << code) != 0,
                _ => in_items(items, c) != *negated,
            },
        }
    }
}

fn in_items(items: &[Item], c: char) -> bool {
    items.iter().any(|item| match *item {
        Item::Range(lo, hi) => lo <= c && c <= hi,
        Item::Space => c.is_whitespace(),
    })
}

#[derive(Debug)]
enum Node {
    Char(CharTest),
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max_one: bool,
        greedy: bool,
    },
    WordBoundary,
    End,
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    fold: bool,
    groups: usize,
}

fn unsupported(what: &str) -> String {
    format!("pattern syntax outside the supported subset: {what}")
}

impl Parser<'_> {
    fn alt(&mut self) -> Result<Node, String> {
        let mut branches = vec![self.concat()?];
        while self.chars.peek() == Some(&'|') {
            self.chars.next();
            branches.push(self.concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.remove(0)
        } else {
            Node::Alt(branches)
        })
    }

    fn concat(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(&c) = self.chars.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantified(atom)?);
        }
        Ok(Node::Concat(items))
    }

    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max_one) = match self.chars.peek() {
            Some('*') => (0, false),
            Some('+') => (1, false),
            Some('?') => (0, true),
            Some('{') => return Err(unsupported("counted repetition")),
            _ => return Ok(atom),
        };
        if matches!(atom, Node::WordBoundary | Node::End) {
            return Err(unsupported("repeated assertion"));
        }
        self.chars.next();
        let greedy = if self.chars.peek() == Some(&'?') {
            self.chars.next();
            false
        } else {
            true
        };
        if matches!(self.chars.peek(), Some('*' | '+' | '?' | '{')) {
            return Err(unsupported("stacked repetition"));
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max_one,
            greedy,
        })
    }

    fn atom(&mut self) -> Result<Node, String> {
        let Some(c) = self.chars.next() else {
            return Err(unsupported("unexpected end"));
        };
        match c {
            '(' => {
                let index = if self.chars.peek() == Some(&'?') {
                    self.chars.next();
                    if self.chars.next() != Some(':') {
                        return Err(unsupported("group flag other than a leading (?i)"));
                    }
                    None
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.alt()?;
                if self.chars.next() != Some(')') {
                    return Err(unsupported("unclosed group"));
                }
                Ok(Node::Group(Box::new(inner), index))
            }
            '[' => self.class(),
            '.' => Ok(Node::Char(CharTest::Any)),
            '$' => Ok(Node::End),
            '\\' => match self.escape()? {
                Escaped::WordBoundary => Ok(Node::WordBoundary),
                Escaped::Item(item) => Ok(Node::Char(self.set(vec![item], false))),
            },
            '*' | '+' | '?' | '{' | '}' | ')' | ']' | '^' => {
                Err(unsupported(&format!("unescaped `{c}`")))
            }
            c if c.is_ascii() => Ok(Node::Char(self.set(vec![Item::Range(c, c)], false))),
            _ => Err(unsupported("non-ASCII literal")),
        }
    }

    fn escape(&mut self) -> Result<Escaped, String> {
        match self.chars.next() {
            Some('b') => Ok(Escaped::WordBoundary),
            Some('s') => Ok(Escaped::Item(Item::Space)),
            Some('n') => Ok(Escaped::Item(Item::Range('\n', '\n'))),
            Some('t') => Ok(Escaped::Item(Item::Range('\t', '\t'))),
            Some(c) if c.is_ascii_punctuation() => Ok(Escaped::Item(Item::Range(c, c))),
            Some(c) => Err(unsupported(&format!("escape `\\{c}`"))),
            None => Err(unsupported("trailing backslash")),
        }
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = if self.chars.peek() == Some(&'^') {
            self.chars.next();
            true
        } else {
            false
        };
        let mut items = Vec::new();
        loop {
            let lo = match self.chars.next() {
                None => return Err(unsupported("unclosed class")),
                Some(']') if !items.is_empty() => break,
                Some('[') => return Err(unsupported("nested class")),
                Some('\\') => match self.escape()? {
                    Escaped::Item(Item::Range(c, _)) => c,
                    Escaped::Item(Item::Space) => {
                        items.push(Item::Space);
                        continue;
                    }
                    Escaped::WordBoundary => return Err(unsupported("\\b in a class")),
                },
                Some(c) if c.is_ascii() => c,
                Some(_) => return Err(unsupported("non-ASCII class member")),
            };
            let mut ahead = self.chars.clone();
            if ahead.next() == Some('-') && ahead.peek().is_some_and(|&hi| hi != ']') {
                self.chars.next();
                let hi = match self.chars.next() {
                    Some(hi) if hi.is_ascii() && hi != '\\' && hi != '[' && lo <= hi => hi,
                    _ => return Err(unsupported("class range")),
                };
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
        Ok(Node::Char(self.set(items, negated)))
    }

    /// A char set, closed under simple case folding when `(?i)` is on.
    fn set(&self, mut items: Vec<Item>, negated: bool) -> CharTest {
        if self.fold {
            let mut extra = Vec::new();
            let covers = |items: &[Item], c: char| {
                items
                    .iter()
                    .any(|item| matches!(*item, Item::Range(lo, hi) if lo <= c && c <= hi))
            };
            for item in &items {
                if let Item::Range(lo, hi) = *item {
                    for (from, to) in [(b'a', b'A'), (b'A', b'a')] {
                        let start = (lo as u8).max(from);
                        let end = (hi as u8).min(from + 25);
                        if start <= end {
                            extra.push(Item::Range(
                                char::from(start - from + to),
                                char::from(end - from + to),
                            ));
                        }
                    }
                }
            }
            if covers(&items, 'k') || covers(&items, 'K') {
                extra.push(Item::Range('\u{212A}', '\u{212A}'));
            }
            if covers(&items, 's') || covers(&items, 'S') {
                extra.push(Item::Range('\u{17F}', '\u{17F}'));
            }
            items.extend(extra);
        }
        let ascii = (0u8..128)
            .filter(|&b| in_items(&items, char::from(b)) != negated)
            .fold(0u128, |bits, b| bits | (1 << b));
        CharTest::Set {
            ascii,
            items,
            negated,
        }
    }
}

enum Escaped {
    WordBoundary,
    Item(Item),
}

fn compile(node: &Node, prog: &mut Vec<Inst>) {
    match node {
        Node::Char(test) => prog.push(Inst::Char(test.clone())),
        Node::WordBoundary => prog.push(Inst::WordBoundary),
        Node::End => prog.push(Inst::End),
        Node::Concat(items) => items.iter().for_each(|item| compile(item, prog)),
        Node::Group(inner, None) => compile(inner, prog),
        Node::Group(inner, Some(index)) => {
            prog.push(Inst::Save(2 * index));
            compile(inner, prog);
            prog.push(Inst::Save(2 * index + 1));
        }
        Node::Alt(branches) => {
            let mut jumps = Vec::new();
            for (i, branch) in branches.iter().enumerate() {
                if i + 1 < branches.len() {
                    let split = prog.len();
                    prog.push(Inst::Split(split + 1, 0));
                    compile(branch, prog);
                    jumps.push(prog.len());
                    prog.push(Inst::Jmp(0));
                    let next = prog.len();
                    prog[split] = Inst::Split(split + 1, next);
                } else {
                    compile(branch, prog);
                }
            }
            let end = prog.len();
            for jump in jumps {
                prog[jump] = Inst::Jmp(end);
            }
        }
        Node::Repeat {
            node,
            min,
            max_one,
            greedy,
        } => {
            for _ in 0..*min {
                compile(node, prog);
            }
            // After the required copy: `e?` is one optional copy, `e*` and
            // the tail of `e+` a loop.
            let split = prog.len();
            prog.push(Inst::Split(0, 0));
            compile(node, prog);
            if !*max_one {
                prog.push(Inst::Jmp(split));
            }
            let after = prog.len();
            prog[split] = if *greedy {
                Inst::Split(split + 1, after)
            } else {
                Inst::Split(after, split + 1)
            };
        }
    }
}

/// Every char test reachable from the start without consuming a char,
/// or `None` when the start can reach `Match` (an empty match is possible).
fn first_tests(prog: &[Inst]) -> Option<Vec<CharTest>> {
    let mut seen = vec![false; prog.len()];
    let mut stack = vec![0];
    let mut tests = Vec::new();
    while let Some(pc) = stack.pop() {
        if std::mem::replace(&mut seen[pc], true) {
            continue;
        }
        match &prog[pc] {
            Inst::Char(test) => tests.push(test.clone()),
            Inst::Match => return None,
            Inst::Jmp(to) => stack.push(*to),
            Inst::Split(first, second) => stack.extend([*first, *second]),
            Inst::Save(_) | Inst::WordBoundary | Inst::End => stack.push(pc + 1),
        }
    }
    Some(tests)
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

type Slots = Vec<Option<usize>>;

/// The threads of one step, in priority order. Capture slots live in one
/// flat buffer (`width` per thread) so a step allocates nothing.
struct Threads {
    pcs: Vec<usize>,
    slots: Slots,
    width: usize,
    /// `on[pc] == stamp` once `pc` joined this step (first, highest priority
    /// wins); bumping `stamp` empties the set without touching it.
    on: Vec<u64>,
    stamp: u64,
}

impl Threads {
    fn new(len: usize, width: usize) -> Self {
        Self {
            pcs: Vec::new(),
            slots: Vec::new(),
            width,
            on: vec![0; len],
            stamp: 1,
        }
    }

    fn clear(&mut self) {
        self.pcs.clear();
        self.slots.clear();
        self.stamp += 1;
    }

    fn push(&mut self, pc: usize, slots: &[Option<usize>]) {
        self.pcs.push(pc);
        self.slots.extend_from_slice(slots);
    }

    fn slots(&self, index: usize) -> &[Option<usize>] {
        &self.slots[index * self.width..(index + 1) * self.width]
    }
}

impl Pattern {
    /// Compiles `pattern`, refusing syntax outside the supported subset.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let (fold, body) = match pattern.strip_prefix("(?i)") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let mut parser = Parser {
            chars: body.chars().peekable(),
            fold,
            groups: 0,
        };
        let node = parser.alt()?;
        if parser.chars.next().is_some() {
            return Err(unsupported("unbalanced `)`"));
        }
        let mut prog = vec![Inst::Save(0)];
        compile(&node, &mut prog);
        prog.push(Inst::Save(1));
        prog.push(Inst::Match);
        let first = first_tests(&prog);
        Ok(Self {
            prog,
            groups: parser.groups,
            first,
        })
    }

    /// Adds `pc`'s epsilon closure at `pos` (byte offset, text) to `threads`.
    fn add(&self, threads: &mut Threads, pc: usize, pos: (usize, &str), slots: &mut Slots) {
        let (at, text) = pos;
        if threads.on[pc] == threads.stamp {
            return;
        }
        threads.on[pc] = threads.stamp;
        match self.prog[pc] {
            Inst::Jmp(to) => self.add(threads, to, (at, text), slots),
            Inst::Split(first, second) => {
                self.add(threads, first, (at, text), slots);
                self.add(threads, second, (at, text), slots);
            }
            Inst::Save(slot) => {
                let old = slots[slot];
                slots[slot] = Some(at);
                self.add(threads, pc + 1, (at, text), slots);
                slots[slot] = old;
            }
            Inst::WordBoundary => {
                let before = text[..at].chars().next_back();
                let after = text[at..].chars().next();
                if is_word(before) != is_word(after) {
                    self.add(threads, pc + 1, (at, text), slots);
                }
            }
            Inst::End => {
                if at == text.len() {
                    self.add(threads, pc + 1, (at, text), slots);
                }
            }
            Inst::Char(_) | Inst::Match => threads.push(pc, slots),
        }
    }

    /// Leftmost-first match starting the search at byte offset `start`.
    fn search(&self, text: &str, start: usize) -> Option<Slots> {
        let width = 2 * (self.groups + 1);
        let mut current = Threads::new(self.prog.len(), width);
        let mut next = Threads::new(self.prog.len(), width);
        let mut scratch: Slots = vec![None; width];
        let mut matched: Option<Slots> = None;
        let mut at = start;
        loop {
            if matched.is_none() {
                scratch.iter_mut().for_each(|slot| *slot = None);
                self.add(&mut current, 0, (at, text), &mut scratch);
            }
            let c = text[at..].chars().next();
            if current.pcs.is_empty() {
                match (c, &matched) {
                    (Some(c), None) => {
                        at += c.len_utf8();
                        if let Some(first) = &self.first {
                            at = text[at..]
                                .char_indices()
                                .find(|&(_, c)| first.iter().any(|test| test.matches(c)))
                                .map_or(text.len(), |(offset, _)| at + offset);
                        }
                        current.clear();
                        continue;
                    }
                    _ => break,
                }
            }
            next.clear();
            for index in 0..current.pcs.len() {
                let pc = current.pcs[index];
                match &self.prog[pc] {
                    Inst::Match => {
                        // Lower-priority threads are cut; higher ones live on.
                        matched = Some(current.slots(index).to_vec());
                        break;
                    }
                    Inst::Char(test) => {
                        if let Some(c) = c.filter(|&c| test.matches(c)) {
                            scratch.copy_from_slice(current.slots(index));
                            self.add(&mut next, pc + 1, (at + c.len_utf8(), text), &mut scratch);
                        }
                    }
                    Inst::Split(..)
                    | Inst::Jmp(_)
                    | Inst::Save(_)
                    | Inst::WordBoundary
                    | Inst::End => {}
                }
            }
            std::mem::swap(&mut current, &mut next);
            match c {
                Some(c) => at += c.len_utf8(),
                None => break,
            }
        }
        matched
    }

    /// The leftmost-first match and its groups, if any.
    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        self.search(text, 0).map(|slots| Captures { text, slots })
    }

    /// Every non-overlapping match, left to right.
    pub fn captures_iter<'p, 't>(&'p self, text: &'t str) -> CaptureMatches<'p, 't> {
        CaptureMatches {
            pattern: self,
            text,
            at: Some(0),
            last_end: None,
        }
    }

    /// Every non-overlapping match, left to right (whole-match spans only).
    pub fn find_iter<'p, 't>(&'p self, text: &'t str) -> impl Iterator<Item = Match<'t>> + 'p
    where
        't: 'p,
    {
        self.captures_iter(text)
            .filter_map(|captures| captures.get(0))
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.search(text, 0).is_some()
    }
}

/// Iterator of [`Captures`] for [`Pattern::captures_iter`].
pub struct CaptureMatches<'p, 't> {
    pattern: &'p Pattern,
    text: &'t str,
    at: Option<usize>,
    last_end: Option<usize>,
}

impl<'t> Iterator for CaptureMatches<'_, 't> {
    type Item = Captures<'t>;

    fn next(&mut self) -> Option<Captures<'t>> {
        let mut start = self.at?;
        loop {
            let slots = self.pattern.search(self.text, start)?;
            let (begin, end) = (slots[0]?, slots[1]?);
            if begin == end && Some(end) == self.last_end {
                // An empty match right where the previous match ended is
                // skipped, and the search resumes one char later, as the
                // `regex` crate does.
                start = end + self.text[end..].chars().next()?.len_utf8();
                continue;
            }
            self.at = Some(end);
            self.last_end = Some(end);
            return Some(Captures {
                text: self.text,
                slots,
            });
        }
    }
}

/// The groups of one match; group 0 is the whole match.
#[derive(Debug, Clone)]
pub struct Captures<'t> {
    text: &'t str,
    slots: Slots,
}

impl<'t> Captures<'t> {
    /// Group `index`, or `None` when it did not take part in the match.
    pub fn get(&self, index: usize) -> Option<Match<'t>> {
        let start = (*self.slots.get(2 * index)?)?;
        let end = (*self.slots.get(2 * index + 1)?)?;
        Some(Match {
            text: self.text,
            start,
            end,
        })
    }
}

/// One matched span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match<'t> {
    text: &'t str,
    start: usize,
    end: usize,
}

impl<'t> Match<'t> {
    pub fn as_str(&self) -> &'t str {
        &self.text[self.start..self.end]
    }

    pub fn start(&self) -> usize {
        self.start
    }

    pub fn end(&self) -> usize {
        self.end
    }
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    /// The static patterns this matcher serves, copied verbatim;
    /// `patterns_are_the_ones_in_use` fails if a source copy drifts.
    const PATTERNS: [&str; 11] = [
        r#"(?i)\b(?:create|write|save)\s+(?:a\s+|an\s+)?(?:new\s+)?file(?:\s+named|\s+called)?\s+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;]+))"#,
        r#"(?i)\bcontaining(?:\s+exactly)?(?::\s*(.+)$|\s+(?:"([^"]+)"|'([^']+)'|`([^`]+)`))"#,
        r#"(?i)\b(?:write|save|export|dump|store|output)\b[^.;\n]*?\b(?:to|into)\s+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;]*(?:\.[A-Za-z0-9]|/)[^\s,;]*))"#,
        r#"(?i)\b(?:create|created|make|made|mkdir)\s+(?:a\s+|an\s+|the\s+)?(?:new\s+)?(?:directory|folder|dir)(?:\s+named|\s+called)?\s+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;]+))"#,
        r#"(?i)\b(?:delete|remove)\s+(?:the\s+)?(?:file\s+)?(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;]*(?:\.[A-Za-z0-9]|/)[^\s,;]*))"#,
        r#"(?i)\b(?:wrote to|saved to)\b"#,
        r#"(?i)\b(?:wrote to|saved to)[ \t]+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s]+))"#,
        r#"(?i)\b(?:deleted|removed)[ \t]+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;`"']*(?:\.[A-Za-z0-9]|/)[^\s,;`"']*))"#,
        r#"(?i)\b(?:created|added|generated|wrote|updated)\s+(?:the\s+|a\s+|new\s+)*(?:file\s+)?(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;`"']*/[^\s,;`"']*\.[A-Za-z0-9]+))"#,
        r#"(?i)\b(?:wrote|saved|put|added|appended|inserted)\s+(?:`([^`]+)`|"([^"]+)"|'([^']+)')\s+(?:to|into|in)\s+(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;`"']*(?:\.[A-Za-z0-9]|/)[^\s,;`"']*))"#,
        r"\bFAILURE\b",
    ];
    const MOVE_PATH: &str =
        r#"(?:`([^`]+)`|"([^"]+)"|'([^']+)'|([^\s,;`"']*(?:\.[A-Za-z0-9]|/)[^\s,;`"']*))"#;
    const MOVE_TEMPLATE: &str =
        r"(?i)\b(moved|renamed|copied)\s+(?:the\s+)?(?:file\s+)?{PATH}\s+(?:to|as|into)\s+{PATH}";

    fn compiled(index: usize) -> Pattern {
        let source = if index == 11 {
            MOVE_TEMPLATE.replace("{PATH}", MOVE_PATH)
        } else {
            PATTERNS[index].to_string()
        };
        Pattern::new(&source).expect("supported pattern")
    }

    type Spans = &'static [Option<(usize, usize)>];

    /// Every group of every match, as the `regex` crate 1.13.1 reports them
    /// for the same pattern and text (generated with it, then frozen here).
    const CASES: &[(usize, &str, &[Spans])] = &[
        (
            0,
            "Create a file named `my notes.txt` containing: hello world",
            &[&[Some((0, 34)), Some((21, 33)), None, None, None]],
        ),
        (
            0,
            "please WRITE A NEW FILE called \"out.md\", then stop",
            &[&[Some((7, 39)), None, Some((32, 38)), None, None]],
        ),
        (
            0,
            "save an file report.csv; done",
            &[&[Some((0, 23)), None, None, None, Some((13, 23))]],
        ),
        (0, "creat file x.txt and rewrite file y.txt", &[]),
        (
            0,
            "Keep: ſave file kelvin.txt",
            &[&[Some((8, 29)), None, None, None, Some((19, 29))]],
        ),
        (1, "write file a.txt containing: two\nlines", &[]),
        (
            1,
            "write file a.txt containing exactly 'hi there' and containing: last",
            &[
                &[Some((17, 46)), None, None, Some((37, 45)), None],
                &[Some((51, 67)), Some((63, 67)), None, None, None],
            ],
        ),
        (1, "containing the results of the analysis", &[]),
        (
            2,
            "save the report to out.md. Then export results into data/x.csv",
            &[
                &[Some((0, 26)), None, None, None, Some((19, 26))],
                &[Some((32, 62)), None, None, None, Some((52, 62))],
            ],
        ),
        (
            2,
            "write to disk, store into `a b.txt`; output to stdout",
            &[&[Some((0, 35)), Some((27, 34)), None, None, None]],
        ),
        (
            2,
            "rewrite it to x.txt; dump everything into dir/ now",
            &[&[Some((21, 46)), None, None, None, Some((42, 46))]],
        ),
        (2, "Save everything; to out.md", &[]),
        (
            3,
            "mkdir dir build/out, then made the folder called \"My Docs\"",
            &[
                &[Some((0, 19)), None, None, None, Some((10, 19))],
                &[Some((26, 58)), None, Some((50, 57)), None, None],
            ],
        ),
        (
            3,
            "Created a new directory named src.",
            &[&[Some((0, 34)), None, None, None, Some((30, 34))]],
        ),
        (
            4,
            "delete old.log and remove the file `a b`; delete the cache",
            &[
                &[Some((0, 14)), None, None, None, Some((7, 14))],
                &[Some((19, 40)), Some((36, 39)), None, None, None],
            ],
        ),
        (
            4,
            "Remove data/, deleted x.txt",
            &[&[Some((0, 12)), None, None, None, Some((7, 12))]],
        ),
        (
            5,
            "I wrote to notes.txt and SAVED TO x; wrote tox",
            &[&[Some((2, 10))], &[Some((25, 33))]],
        ),
        (
            6,
            "I wrote to notes.txt: then saved to `a b.txt` and wrote to\tc.md",
            &[
                &[Some((2, 21)), None, None, None, Some((11, 21))],
                &[Some((27, 45)), Some((37, 44)), None, None, None],
                &[Some((50, 63)), None, None, None, Some((59, 63))],
            ],
        ),
        (
            6,
            "wrote to\nnext.txt, saved to   \"q.txt\"",
            &[&[Some((19, 37)), None, Some((31, 36)), None, None]],
        ),
        (
            7,
            "Deleted branch feature/login, removed 2 packages, deleted old.log)",
            &[&[Some((50, 66)), None, None, None, Some((58, 66))]],
        ),
        (
            7,
            "removed `x y` and deleted \"z\", Removed a.b,c",
            &[
                &[Some((0, 13)), Some((9, 12)), None, None, None],
                &[Some((18, 29)), None, Some((27, 28)), None, None],
                &[Some((31, 42)), None, None, None, Some((39, 42))],
            ],
        ),
        (
            8,
            "created src/utils/mod.rs and added the new file `x.txt`; created main.rs",
            &[
                &[Some((0, 24)), None, None, None, Some((8, 24))],
                &[Some((29, 55)), Some((49, 54)), None, None, None],
            ],
        ),
        (
            8,
            "Updated a new the file docs/a.md.",
            &[&[Some((0, 32)), None, None, None, Some((23, 32))]],
        ),
        (
            9,
            "wrote \"hello\" to notes.txt. Put 'x' into `a b`; appended `y` in dir/",
            &[
                &[
                    Some((0, 27)),
                    None,
                    Some((7, 12)),
                    None,
                    None,
                    None,
                    None,
                    Some((17, 27)),
                ],
                &[
                    Some((28, 46)),
                    None,
                    None,
                    Some((33, 34)),
                    Some((42, 45)),
                    None,
                    None,
                    None,
                ],
                &[
                    Some((48, 68)),
                    Some((58, 59)),
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some((64, 68)),
                ],
            ],
        ),
        (9, "added \"a\" to b", &[]),
        (
            10,
            "TEST FAILURE: 2 FAILURES, _FAILURE, FAILURE_X, (FAILURE)",
            &[&[Some((5, 12))], &[Some((48, 55))]],
        ),
        (10, "FAILURE", &[&[Some((0, 7))]]),
        (
            11,
            "moved a.txt to b.txt, copied `x y` as \"z w\", renamed the file c/d into e.md.",
            &[
                &[
                    Some((0, 20)),
                    Some((0, 5)),
                    None,
                    None,
                    None,
                    Some((6, 11)),
                    None,
                    None,
                    None,
                    Some((15, 20)),
                ],
                &[
                    Some((22, 43)),
                    Some((22, 28)),
                    Some((30, 33)),
                    None,
                    None,
                    None,
                    None,
                    Some((39, 42)),
                    None,
                    None,
                ],
                &[
                    Some((45, 76)),
                    Some((45, 52)),
                    None,
                    None,
                    None,
                    Some((62, 65)),
                    None,
                    None,
                    None,
                    Some((71, 76)),
                ],
            ],
        ),
        (
            11,
            "Moved a to b; renamed x.rs.bak into x.rs",
            &[&[
                Some((14, 40)),
                Some((14, 21)),
                None,
                None,
                None,
                Some((22, 30)),
                None,
                None,
                None,
                Some((36, 40)),
            ]],
        ),
        (
            11,
            "copied   a.b\u{a0}to\u{2003}c.d",
            &[&[
                Some((0, 22)),
                Some((0, 6)),
                None,
                None,
                None,
                Some((9, 12)),
                None,
                None,
                None,
                Some((19, 22)),
            ]],
        ),
    ];

    #[test]
    fn matches_and_groups_equal_the_regex_crate_on_the_frozen_cases() {
        for (index, text, expected) in CASES {
            let pattern = compiled(*index);
            let groups = expected.first().map_or(0, |groups| groups.len());
            let found: Vec<Vec<Option<(usize, usize)>>> = pattern
                .captures_iter(text)
                .map(|captures| {
                    (0..groups)
                        .map(|group| captures.get(group).map(|m| (m.start(), m.end())))
                        .collect()
                })
                .collect();
            let expected: Vec<Vec<Option<(usize, usize)>>> =
                expected.iter().map(|groups| groups.to_vec()).collect();
            assert_eq!(found, expected, "pattern {index} on {text:?}");
        }
    }

    #[test]
    fn patterns_are_the_ones_in_use() {
        let sources = [include_str!("truth.rs"), include_str!("verification.rs")];
        // PATTERNS[10] stays as an engine case: GAWD's accountability check
        // matches that token by hand, with the same word chars.
        for pattern in PATTERNS[..10].iter().chain([&MOVE_PATH, &MOVE_TEMPLATE]) {
            assert!(
                sources.iter().any(|source| source.contains(pattern)),
                "{pattern} is no longer in use; update the frozen cases"
            );
        }
    }

    #[test]
    fn match_text_and_captures_agree() {
        let pattern = compiled(6);
        let text = "I wrote to notes.txt then saved to `a b`";
        let first = pattern.captures(text).expect("a match");
        assert_eq!(first.get(4).map(|m| m.as_str()), Some("notes.txt"));
        assert_eq!(pattern.find_iter(text).count(), 2);
        assert!(compiled(10).is_match("A FAILURE."));
        assert!(!compiled(10).is_match("FAILURES"));
    }

    #[test]
    fn case_folding_adds_only_the_kelvin_sign_and_the_long_s() {
        let pattern = Pattern::new("(?i)[a-z]").expect("class");
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            let expected = c.is_ascii_alphabetic() || c == '\u{212A}' || c == '\u{17F}';
            assert_eq!(pattern.is_match(&c.to_string()), expected, "{c:?}");
        }
        let negated = Pattern::new("(?i)[^k]").expect("class");
        assert!(!negated.is_match("\u{212A}"));
        assert!(Pattern::new("[^k]").expect("class").is_match("K"));
    }

    #[test]
    fn word_boundary_uses_alphanumeric_or_underscore() {
        let pattern = compiled(10);
        assert!(pattern.is_match("x FAILURE"));
        assert!(!pattern.is_match("_FAILURE"));
        assert!(!pattern.is_match("\u{e9}FAILURE"));
        // The documented difference from Unicode `\w`: a combining mark is
        // not a word char here (the regex crate sees no boundary).
        assert!(pattern.is_match("\u{301}FAILURE"));
    }

    #[test]
    fn lazy_and_greedy_repetition_keep_their_priority() {
        let lazy = Pattern::new("a(.*?)b").expect("lazy");
        let greedy = Pattern::new("a(.*)b").expect("greedy");
        let text = "a1b2b";
        let group = |pattern: &Pattern| pattern.captures(text).and_then(|c| c.get(1));
        assert_eq!(group(&lazy).map(|m| m.as_str()), Some("1"));
        assert_eq!(group(&greedy).map(|m| m.as_str()), Some("1b2"));
        let optional = Pattern::new("ab??").expect("lazy optional");
        let found: Vec<&str> = optional.find_iter("abab").map(|m| m.as_str()).collect();
        assert_eq!(found, ["a", "a"]);
        assert_eq!(
            Pattern::new("x$").expect("end").find_iter("x\nx").count(),
            1
        );
    }

    #[test]
    fn empty_matches_follow_the_regex_crate() {
        let pattern = Pattern::new("a*").expect("star");
        let spans: Vec<(usize, usize)> = pattern
            .find_iter("baa\u{e9}")
            .map(|m| (m.start(), m.end()))
            .collect();
        // The empty match at 3, where the previous match ended, is skipped.
        assert_eq!(spans, [(0, 0), (1, 3), (5, 5)]);
    }

    #[test]
    fn syntax_outside_the_subset_is_refused() {
        for pattern in [
            "a{2}", "^a", "(?m)a", "a(?i)b", r"\d", r"\w", r"\B", "[[a]]", "(a", "a)", "\u{e9}",
            "a**", "[z-a]",
        ] {
            assert!(
                Pattern::new(pattern).is_err(),
                "{pattern:?} must be refused"
            );
        }
    }

    #[test]
    fn a_long_text_without_a_match_is_scanned_in_linear_time() {
        let text = "save word ".repeat(100_000);
        let started = std::time::Instant::now();
        assert_eq!(compiled(2).captures_iter(&text).count(), 0);
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }
}
