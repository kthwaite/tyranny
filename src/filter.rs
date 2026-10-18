//! Track-level literal filtering shared by library projections and saved occurrences.
//! Bare atoms retain the complete persisted search corpus, including album
//! artist and path. Multiple bare words now imply AND; quote a phrase to require
//! contiguous text. Qualified atoms use only their named normalized field.
use rusqlite::types::Value;
use std::fmt::{self, Write};

const MAX_BYTES: usize = 1024;
const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 32;

#[derive(Debug)]
pub(crate) struct InvalidFilter {
    position: usize,
    message: &'static str,
}
impl fmt::Display for InvalidFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid filter at UTF-8 byte {}: {}",
            self.position, self.message
        )
    }
}
impl std::error::Error for InvalidFilter {}
type Result<T, E = InvalidFilter> = std::result::Result<T, E>;
fn invalid(position: usize, message: &'static str) -> InvalidFilter {
    InvalidFilter { position, message }
}

#[derive(Clone, Copy)]
enum Field {
    Any,
    Title,
    Artist,
    Album,
}
enum Expr {
    Term(Field, String),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}
#[derive(PartialEq)]
enum Kind {
    Word(String),
    Quoted(String),
    Not,
    And,
    Or,
    Open,
    Close,
    Colon,
    ListOpen,
    ListClose,
    Comma,
}
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut chars = input.char_indices().peekable();
    let mut tokens = Vec::new();
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        let kind = match ch {
            '!' => Kind::Not,
            '&' => Kind::And,
            '|' => Kind::Or,
            '(' => Kind::Open,
            ')' => Kind::Close,
            ':' => Kind::Colon,
            '[' => Kind::ListOpen,
            ']' => Kind::ListClose,
            ',' => Kind::Comma,
            '"' => {
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some((_, '"')) => break,
                        Some((position, '\\')) => match chars.next() {
                            Some((_, escaped @ ('"' | '\\'))) => value.push(escaped),
                            _ => {
                                return Err(invalid(
                                    position,
                                    "expected escaped quote or backslash",
                                ));
                            }
                        },
                        Some((_, ch)) => value.push(ch),
                        None => return Err(invalid(start, "unterminated quoted value")),
                    }
                }
                Kind::Quoted(value)
            }
            _ => {
                let mut end = start + ch.len_utf8();
                while let Some(&(position, next)) = chars.peek() {
                    if next.is_whitespace() || "!&|():[],\"".contains(next) {
                        break;
                    }
                    end = position + next.len_utf8();
                    chars.next();
                }
                match &input[start..end] {
                    "NOT" => Kind::Not,
                    "AND" => Kind::And,
                    "OR" => Kind::Or,
                    word => Kind::Word(word.to_owned()),
                }
            }
        };
        let end = chars.peek().map_or(input.len(), |(position, _)| *position);
        tokens.push(Token { kind, start, end });
        if tokens.len() > MAX_NODES * 4 {
            return Err(invalid(start, "filter is too complex"));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    length: usize,
    nodes: usize,
}
impl Parser {
    fn position(&self) -> usize {
        self.tokens
            .get(self.cursor)
            .map_or(self.length, |t| t.start)
    }
    fn peek(&self) -> Option<&Kind> {
        self.tokens.get(self.cursor).map(|t| &t.kind)
    }
    fn take(&mut self, kind: &Kind) -> bool {
        if self.peek() == Some(kind) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }
    fn node(&mut self, expression: Expr) -> Result<Expr> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(invalid(self.position(), "filter is too complex"));
        }
        Ok(expression)
    }
    fn or(&mut self, depth: usize) -> Result<Expr> {
        let first = self.and(depth)?;
        if !self.take(&Kind::Or) {
            return Ok(first);
        }
        let mut terms = vec![first, self.and(depth)?];
        while self.take(&Kind::Or) {
            terms.push(self.and(depth)?);
        }
        self.node(Expr::Or(terms))
    }
    fn and(&mut self, depth: usize) -> Result<Expr> {
        let first = self.unary(depth)?;
        let mut terms = vec![first];
        loop {
            let explicit = self.take(&Kind::And);
            let adjacent = matches!(
                self.peek(),
                Some(Kind::Word(_) | Kind::Quoted(_) | Kind::Not | Kind::Open)
            ) && self.tokens[self.cursor - 1].end < self.position();
            if !explicit && !adjacent {
                break;
            }
            terms.push(self.unary(depth)?);
        }
        if terms.len() == 1 {
            Ok(terms.pop().unwrap())
        } else {
            self.node(Expr::And(terms))
        }
    }
    fn unary(&mut self, depth: usize) -> Result<Expr> {
        if depth > MAX_DEPTH {
            return Err(invalid(self.position(), "filter nesting exceeds 32 levels"));
        }
        if self.take(&Kind::Not) {
            let term = self.unary(depth + 1)?;
            return self.node(Expr::Not(Box::new(term)));
        }
        if self.take(&Kind::Open) {
            let expression = self.or(depth + 1)?;
            if !self.take(&Kind::Close) {
                return Err(invalid(self.position(), "expected closing parenthesis"));
            }
            return Ok(expression);
        }
        let position = self.position();
        let field = if matches!(self.peek(), Some(Kind::Word(_)))
            && self
                .tokens
                .get(self.cursor + 1)
                .is_some_and(|t| t.kind == Kind::Colon)
        {
            let Kind::Word(name) = &self.tokens[self.cursor].kind else {
                unreachable!()
            };
            let field = match name.as_str() {
                "title" => Field::Title,
                "artist" => Field::Artist,
                "album" => Field::Album,
                _ => {
                    return Err(invalid(
                        position,
                        "unknown field; expected title, artist or album",
                    ));
                }
            };
            self.cursor += 2;
            field
        } else {
            Field::Any
        };
        if !matches!(field, Field::Any) && self.take(&Kind::ListOpen) {
            let mut terms = vec![self.value(field)?];
            while self.take(&Kind::Comma) {
                terms.push(self.value(field)?);
            }
            if !self.take(&Kind::ListClose) {
                return Err(invalid(
                    self.position(),
                    "expected comma or closing list bracket",
                ));
            }
            return self.node(Expr::Or(terms));
        }
        self.value(field)
    }
    fn value(&mut self, field: Field) -> Result<Expr> {
        let position = self.position();
        let value = match self.peek() {
            Some(Kind::Word(value) | Kind::Quoted(value)) if !value.trim().is_empty() => {
                value.to_lowercase()
            }
            _ => return Err(invalid(position, "expected nonempty filter value")),
        };
        self.cursor += 1;
        self.node(Expr::Term(field, value))
    }
}

/// SQL always refers to the candidate track as `t`. Correlated lookups use
/// normalized group columns without adding joins to the unfiltered paging path.
pub(crate) struct Filter {
    pub sql: String,
    pub values: Vec<Value>,
}
impl Filter {
    pub fn parse(input: &str, first_parameter: usize) -> Result<Self> {
        if input.len() > MAX_BYTES {
            return Err(invalid(MAX_BYTES, "filter exceeds 1024 UTF-8 bytes"));
        }
        let mut parser = Parser {
            tokens: tokenize(input)?,
            cursor: 0,
            length: input.len(),
            nodes: 0,
        };
        let mut result = Self {
            sql: String::new(),
            values: Vec::new(),
        };
        if parser.tokens.is_empty() {
            result.sql.push('1');
            return Ok(result);
        }
        let expression = parser.or(0)?;
        if parser.cursor != parser.tokens.len() {
            return Err(invalid(parser.position(), "unexpected token"));
        }
        result.compile(expression, first_parameter);
        Ok(result)
    }
    fn compile(&mut self, expression: Expr, first: usize) {
        match expression {
            Expr::Term(field, value) => {
                let parameter = first + self.values.len();
                self.values.push(Value::Text(value));
                let title = "t.title_sort";
                let artist = "(SELECT fa.search FROM library_artists fa WHERE fa.id=t.artist_id)";
                let album = "(SELECT fl.title_sort FROM library_albums fl WHERE fl.id=t.album_id)";
                let column = match field {
                    Field::Any => "t.search",
                    Field::Title => title,
                    Field::Artist => artist,
                    Field::Album => album,
                };
                write!(self.sql, "instr({column},?{parameter})>0").unwrap();
            }
            Expr::Not(term) => {
                self.sql.push_str("(NOT (");
                self.compile(*term, first);
                self.sql.push_str("))");
            }
            Expr::And(terms) => self.junction(terms, " AND ", first),
            Expr::Or(terms) => self.junction(terms, " OR ", first),
        }
    }
    fn junction(&mut self, terms: Vec<Expr>, operator: &str, first: usize) {
        self.sql.push('(');
        for (index, term) in terms.into_iter().enumerate() {
            if index != 0 {
                self.sql.push_str(operator);
            }
            self.compile(term, first);
        }
        self.sql.push(')');
    }
}
