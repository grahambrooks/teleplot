//! KQL parser: a rule-for-rule port of Kibana's PEG grammar
//! (`kbn-es-query/src/kuery/grammar/grammar.peggy`), without the cursor
//! (autocomplete) rules.
//!
//! PEG semantics matter for compatibility: alternatives are tried in order
//! and the first match wins, so each rule below backtracks exactly like the
//! generated parser does. That reproduces Kibana's quirks too, e.g. an
//! unquoted value may contain spaces (`status: in progress`) but stops at a
//! keyword (`a or b`).

use super::{Literal, Node, Piece, RangeOp, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Character offset where parsing could go no further.
    pub position: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at character {}", self.message, self.position + 1)
    }
}

impl std::error::Error for ParseError {}

pub fn parse(input: &str) -> Result<Node, ParseError> {
    let mut p = Parser {
        chars: input.chars().collect(),
        furthest: 0,
    };
    let mut pos = p.spaces(0);
    let query = p.or_query(pos);
    if let Some((_, end)) = &query {
        pos = *end;
    }
    pos = p.spaces(pos);
    if pos != p.chars.len() {
        let at = p.furthest.max(pos);
        let message = match p.chars.get(at) {
            Some(c) => format!("unexpected {c:?}"),
            None => "unexpected end of query".into(),
        };
        return Err(ParseError {
            position: at,
            message,
        });
    }
    // An empty query matches everything: Kibana builds is(*, *).
    Ok(query.map(|(q, _)| q).unwrap_or_else(|| Node::Is {
        field: Some(Literal::wildcard()),
        value: Literal::wildcard(),
    }))
}

/// A list of values awaiting the field it applies to (`field:(a or b)`).
enum Partial {
    Value(Literal),
    Or(Vec<Partial>),
    And(Vec<Partial>),
    Not(Box<Partial>),
}

impl Partial {
    fn apply(self, field: &Option<Literal>) -> Node {
        match self {
            Partial::Value(value) => Node::Is {
                field: field.clone(),
                value,
            },
            Partial::Or(ps) => Node::Or(ps.into_iter().map(|p| p.apply(field)).collect()),
            Partial::And(ps) => Node::And(ps.into_iter().map(|p| p.apply(field)).collect()),
            Partial::Not(p) => Node::Not(Box::new(p.apply(field))),
        }
    }
}

struct Parser {
    chars: Vec<char>,
    /// Furthest position any rule failed at, for error messages.
    furthest: usize,
}

type Parsed<T> = Option<(T, usize)>;

const SPECIAL: &[char] = &['\\', '(', ')', ':', '<', '>', '"', '*', '{', '}'];

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{a0}')
}

impl Parser {
    fn at(&self, pos: usize) -> Option<char> {
        self.chars.get(pos).copied()
    }

    fn fail<T>(&mut self, pos: usize) -> Option<T> {
        self.furthest = self.furthest.max(pos);
        None
    }

    fn spaces(&self, mut pos: usize) -> usize {
        while self.at(pos).is_some_and(is_space) {
            pos += 1;
        }
        pos
    }

    fn char(&mut self, pos: usize, c: char) -> Option<usize> {
        if self.at(pos) == Some(c) {
            Some(pos + 1)
        } else {
            self.fail(pos)
        }
    }

    /// Case-insensitive ASCII word.
    fn word(&self, pos: usize, word: &str) -> Option<usize> {
        let mut p = pos;
        for w in word.chars() {
            if !self.at(p)?.eq_ignore_ascii_case(&w) {
                return None;
            }
            p += 1;
        }
        Some(p)
    }

    /// `Space+ word Space+` (Or, And).
    fn conjunction(&mut self, pos: usize, word: &str) -> Option<usize> {
        let p = self.spaces(pos);
        if p == pos {
            return self.fail(pos);
        }
        let Some(p) = self.word(p, word) else {
            return self.fail(p);
        };
        let q = self.spaces(p);
        if q == p { self.fail(p) } else { Some(q) }
    }

    /// `'not'i Space+`
    fn not(&mut self, pos: usize) -> Option<usize> {
        let p = self.word(pos, "not")?;
        let q = self.spaces(p);
        if q == p { self.fail(p) } else { Some(q) }
    }

    fn or_query(&mut self, pos: usize) -> Parsed<Node> {
        self.list(pos, "or", Self::and_query, Node::Or)
    }

    fn and_query(&mut self, pos: usize) -> Parsed<Node> {
        self.list(pos, "and", Self::not_query, Node::And)
    }

    /// `head (Conj item)+` building `wrap`, or just `head`.
    fn list<T>(
        &mut self,
        pos: usize,
        word: &str,
        item: fn(&mut Self, usize) -> Parsed<T>,
        wrap: fn(Vec<T>) -> T,
    ) -> Parsed<T> {
        let (head, mut pos) = item(self, pos)?;
        let mut items = vec![head];
        while let Some(p) = self.conjunction(pos, word) {
            let Some((next, p)) = item(self, p) else {
                break;
            };
            items.push(next);
            pos = p;
        }
        Some((
            if items.len() == 1 {
                items.pop().unwrap()
            } else {
                wrap(items)
            },
            pos,
        ))
    }

    fn not_query(&mut self, pos: usize) -> Parsed<Node> {
        if let Some(p) = self.not(pos)
            && let Some((q, end)) = self.sub_query(p)
        {
            return Some((Node::Not(Box::new(q)), end));
        }
        self.sub_query(pos)
    }

    fn sub_query(&mut self, pos: usize) -> Parsed<Node> {
        if let Some(p) = self.char(pos, '(') {
            let p = self.spaces(p);
            if let Some((q, p)) = self.or_query(p) {
                let p = self.spaces(p);
                if let Some(end) = self.char(p, ')') {
                    return Some((q, end));
                }
            }
        }
        self.nested_query(pos)
    }

    fn nested_query(&mut self, pos: usize) -> Parsed<Node> {
        let attempt = |s: &mut Self| -> Parsed<Node> {
            let (field, p) = s.literal(pos)?;
            let p = s.spaces(p);
            let p = s.char(p, ':')?;
            let p = s.spaces(p);
            let p = s.char(p, '{')?;
            let p = s.spaces(p);
            let (query, p) = s.or_query(p)?;
            let p = s.spaces(p);
            let end = s.char(p, '}')?;
            Some((
                Node::Nested {
                    path: field,
                    query: Box::new(query),
                },
                end,
            ))
        };
        attempt(self).or_else(|| self.expression(pos))
    }

    fn expression(&mut self, pos: usize) -> Parsed<Node> {
        // FieldRangeExpression
        if let Some((field, p)) = self.literal(pos) {
            let p = self.spaces(p);
            if let Some((op, p)) = self.range_op(p) {
                let p = self.spaces(p);
                if let Some((value, end)) = self.literal(p) {
                    return Some((Node::Range { field, op, value }, end));
                }
            }
        }
        // FieldValueExpression
        if let Some((field, p)) = self.literal(pos) {
            let p = self.spaces(p);
            if let Some(p) = self.char(p, ':') {
                let p = self.spaces(p);
                if let Some((partial, end)) = self.list_of_values(p) {
                    return Some((partial.apply(&Some(field)), end));
                }
            }
        }
        // ValueExpression
        let (value, end) = self.literal(pos)?;
        Some((Node::Is { field: None, value }, end))
    }

    fn range_op(&mut self, pos: usize) -> Parsed<RangeOp> {
        match (self.at(pos), self.at(pos + 1)) {
            (Some('<'), Some('=')) => Some((RangeOp::Lte, pos + 2)),
            (Some('>'), Some('=')) => Some((RangeOp::Gte, pos + 2)),
            (Some('<'), _) => Some((RangeOp::Lt, pos + 1)),
            (Some('>'), _) => Some((RangeOp::Gt, pos + 1)),
            _ => self.fail(pos),
        }
    }

    fn list_of_values(&mut self, pos: usize) -> Parsed<Partial> {
        if let Some(p) = self.char(pos, '(') {
            let p = self.spaces(p);
            if let Some((partial, p)) = self.or_values(p) {
                let p = self.spaces(p);
                if let Some(end) = self.char(p, ')') {
                    return Some((partial, end));
                }
            }
        }
        let (value, end) = self.literal(pos)?;
        Some((Partial::Value(value), end))
    }

    fn or_values(&mut self, pos: usize) -> Parsed<Partial> {
        self.list(pos, "or", Self::and_values, Partial::Or)
    }

    fn and_values(&mut self, pos: usize) -> Parsed<Partial> {
        self.list(pos, "and", Self::not_values, Partial::And)
    }

    fn not_values(&mut self, pos: usize) -> Parsed<Partial> {
        if let Some(p) = self.not(pos)
            && let Some((v, end)) = self.list_of_values(p)
        {
            return Some((Partial::Not(Box::new(v)), end));
        }
        self.list_of_values(pos)
    }

    /// `Literal = QuotedString / UnquotedLiteral`
    fn literal(&mut self, pos: usize) -> Parsed<Literal> {
        self.quoted(pos).or_else(|| self.unquoted(pos))
    }

    fn quoted(&mut self, pos: usize) -> Parsed<Literal> {
        let mut p = self.char(pos, '"')?;
        let mut text = String::new();
        loop {
            match self.at(p) {
                None => return self.fail(p),
                Some('"') => break,
                Some('\\') => {
                    if let Some((c, next)) = self
                        .escaped_whitespace(p)
                        .or_else(|| self.escaped_unicode(p))
                    {
                        text.push(c);
                        p = next;
                    } else if let Some(c @ ('\\' | '"')) = self.at(p + 1) {
                        text.push(c);
                        p += 2;
                    } else {
                        text.push('\\');
                        p += 1;
                    }
                }
                Some(c) => {
                    text.push(c);
                    p += 1;
                }
            }
        }
        Some((
            Literal {
                pieces: vec![Piece::Text(text)],
                quoted: true,
                value: None,
            },
            p + 1,
        ))
    }

    fn unquoted(&mut self, pos: usize) -> Parsed<Literal> {
        let mut pieces: Vec<Piece> = Vec::new();
        let mut p = pos;
        let push = |pieces: &mut Vec<Piece>, c: char| match pieces.last_mut() {
            Some(Piece::Text(t)) => t.push(c),
            _ => pieces.push(Piece::Text(c.to_string())),
        };
        while let Some(c) = self.at(p) {
            if c == '\\' {
                if let Some((c, next)) = self.escaped_whitespace(p) {
                    push(&mut pieces, c);
                    p = next;
                    continue;
                }
                if let Some(next) = self.at(p + 1).filter(|n| SPECIAL.contains(n)) {
                    push(&mut pieces, next);
                    p += 2;
                    continue;
                }
                if let Some((c, next)) = self.escaped_unicode(p) {
                    push(&mut pieces, c);
                    p = next;
                    continue;
                }
                if let Some(next) = ["or", "and", "not"]
                    .iter()
                    .find_map(|w| self.word(p + 1, w))
                {
                    for &c in &self.chars[p + 1..next] {
                        push(&mut pieces, c);
                    }
                    p = next;
                    continue;
                }
                break; // a lone backslash is special
            }
            if c == '*' {
                pieces.push(Piece::Wildcard);
                p += 1;
                continue;
            }
            if SPECIAL.contains(&c) || self.keyword_at(p) {
                break;
            }
            push(&mut pieces, c);
            p += 1;
        }
        if p == pos {
            return self.fail(pos);
        }
        trim(&mut pieces);
        let value = match pieces.as_slice() {
            [Piece::Text(t)] if t == "null" => Some(Value::Null),
            [Piece::Text(t)] if t == "true" => Some(Value::Bool(true)),
            [Piece::Text(t)] if t == "false" => Some(Value::Bool(false)),
            _ => None,
        };
        Some((
            Literal {
                pieces,
                quoted: false,
                value,
            },
            p,
        ))
    }

    /// `Keyword = Space 'or'i Space / Space 'and'i Space / 'not'i Space`
    fn keyword_at(&self, pos: usize) -> bool {
        let followed_by_space = |p: Option<usize>| p.and_then(|p| self.at(p)).is_some_and(is_space);
        if self.at(pos).is_some_and(is_space)
            && (followed_by_space(self.word(pos + 1, "or"))
                || followed_by_space(self.word(pos + 1, "and")))
        {
            return true;
        }
        followed_by_space(self.word(pos, "not"))
    }

    fn escaped_whitespace(&self, pos: usize) -> Parsed<char> {
        let c = match (self.at(pos), self.at(pos + 1)) {
            (Some('\\'), Some('t')) => '\t',
            (Some('\\'), Some('r')) => '\r',
            (Some('\\'), Some('n')) => '\n',
            _ => return None,
        };
        Some((c, pos + 2))
    }

    fn escaped_unicode(&self, pos: usize) -> Parsed<char> {
        if self.at(pos) != Some('\\') || self.at(pos + 1) != Some('u') {
            return None;
        }
        let digits: String = self.chars.get(pos + 2..pos + 6)?.iter().collect();
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let c = char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?;
        Some((c, pos + 6))
    }
}

/// `chars.join('').trim()` over pieces: JavaScript's trim, applied to the
/// text at either end.
fn trim(pieces: &mut Vec<Piece>) {
    if let Some(Piece::Text(t)) = pieces.first_mut() {
        *t = t.trim_start().to_string();
    }
    if let Some(Piece::Text(t)) = pieces.last_mut() {
        *t = t.trim_end().to_string();
    }
    pieces.retain(|p| !matches!(p, Piece::Text(t) if t.is_empty()));
}
