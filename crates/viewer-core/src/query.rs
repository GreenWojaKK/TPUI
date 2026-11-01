//! Text form of a row view query: find, filter and sort, one command per line.
//!
//! ```text
//! find 홍길동
//! filter age >= 30 and status == "활성"
//! sort -created_at
//! ```
//!
//! - `find <value>` finds cells whose whole value equals the value.
//! - `filter <expression>` keeps rows for which the expression holds. A condition is `<column> <op> <value>` with
//!   `==`, `!=`, `<`, `<=`, `>`, `>=` or `contains`, or `<column> is null` and `<column> is not null`; `=` is read
//!   as `==`. Conditions are joined with `and`, and conditions on one column also with `or`, as in
//!   `(route == a or route == b) and id >= 5` (see [`crate::Filter`]). One level joins with one word, so mixing
//!   `and` and `or` takes parentheses. Several `filter` lines combine with `and`.
//! - `sort <column>` sorts ascending and `sort -<column>` descending, by one column.
//! - Values without spaces may be written as is; otherwise they are quoted as `"…"` with `\"`, `\\`, `\n`, `\t`
//!   and `\r` escapes. Column names with spaces or symbols are quoted as `` `…` ``, doubling a backtick inside.
//! - Keywords are case-insensitive. Blank lines are ignored. A query states the whole view: a missing line means
//!   no find, no filter or no sort, and an empty query clears them all.
//!
//! [`format_query`] writes a query back in this form, so a query parses to the same view it was formatted from.
//! It puts each set of conditions joined by `or` in parentheses, as the filter and sort bar draws them.

use std::fmt;

use arrow_schema::Schema;

use crate::condition::{ValueClass, check_condition, filter_ops, is_sortable, value_class};
use crate::{ColumnId, Filter, FilterCondition, FilterOp, LogicOp, SortKey, ViewSpec};

/// Row view conditions and the find value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub view: ViewSpec,
    pub find: Option<String>,
}

/// Why a query does not parse, at a 1-based line and character column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for QueryError {}

/// Text of an operator in queries.
pub fn op_text(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "==",
        FilterOp::NotEq => "!=",
        FilterOp::Contains => "contains",
        FilterOp::Lt => "<",
        FilterOp::LtEq => "<=",
        FilterOp::Gt => ">",
        FilterOp::GtEq => ">=",
        FilterOp::IsNull => "is null",
        FilterOp::IsNotNull => "is not null",
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// Text up to whitespace or a symbol.
    Word(String),
    /// `"…"` with escapes decoded.
    Quoted(String),
    /// `` `…` `` column name.
    Name(String),
    Op(FilterOp),
    Open,
    Close,
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    /// 1-based character column where the token starts.
    column: usize,
}

/// A filter expression as written, before it is checked against the shape a [`Filter`] takes.
enum Expr {
    /// A condition and the column where it starts.
    Condition(FilterCondition, usize),
    /// Items joined by one word. `at` is where the group starts: its opening parenthesis, or its first item.
    Group {
        op: LogicOp,
        items: Vec<Expr>,
        at: usize,
    },
}

impl Expr {
    fn at(&self) -> usize {
        match self {
            Expr::Condition(_, at) | Expr::Group { at, .. } => *at,
        }
    }
}

const SYMBOLS: [char; 8] = ['=', '!', '<', '>', '"', '`', '(', ')'];

fn tokens(line: &str, line_no: usize) -> Result<Vec<Token>, QueryError> {
    let chars: Vec<char> = line.chars().collect();
    let error = |column: usize, message: String| QueryError {
        line: line_no,
        column,
        message,
    };
    let mut out = Vec::new();
    let mut ix = 0;
    while ix < chars.len() {
        let ch = chars[ix];
        let column = ix + 1;
        if ch.is_whitespace() {
            ix += 1;
            continue;
        }
        let kind = match ch {
            '"' => {
                let mut text = String::new();
                ix += 1;
                loop {
                    match chars.get(ix) {
                        None => return Err(error(column, "the quote is not closed".into())),
                        Some('"') => break,
                        Some('\\') => {
                            match chars.get(ix + 1) {
                                Some('"') => text.push('"'),
                                Some('\\') => text.push('\\'),
                                Some('n') => text.push('\n'),
                                Some('t') => text.push('\t'),
                                Some('r') => text.push('\r'),
                                // Other backslashes stay as written, such as in Windows paths.
                                Some(other) => {
                                    text.push('\\');
                                    text.push(*other);
                                }
                                None => {
                                    return Err(error(column, "the quote is not closed".into()));
                                }
                            }
                            ix += 2;
                            continue;
                        }
                        Some(other) => text.push(*other),
                    }
                    ix += 1;
                }
                ix += 1;
                Kind::Quoted(text)
            }
            '`' => {
                let mut text = String::new();
                ix += 1;
                loop {
                    match chars.get(ix) {
                        None => return Err(error(column, "the column name is not closed".into())),
                        Some('`') if chars.get(ix + 1) == Some(&'`') => {
                            text.push('`');
                            ix += 2;
                            continue;
                        }
                        Some('`') => break,
                        Some(other) => text.push(*other),
                    }
                    ix += 1;
                }
                ix += 1;
                Kind::Name(text)
            }
            '(' | ')' => {
                ix += 1;
                if ch == '(' { Kind::Open } else { Kind::Close }
            }
            '=' | '!' | '<' | '>' => {
                let next = chars.get(ix + 1).copied();
                let (op, len) = match (ch, next) {
                    ('=', Some('=')) => (FilterOp::Eq, 2),
                    ('=', _) => (FilterOp::Eq, 1),
                    ('!', Some('=')) => (FilterOp::NotEq, 2),
                    ('<', Some('=')) => (FilterOp::LtEq, 2),
                    ('<', _) => (FilterOp::Lt, 1),
                    ('>', Some('=')) => (FilterOp::GtEq, 2),
                    ('>', _) => (FilterOp::Gt, 1),
                    _ => return Err(error(column, "`!` must be followed by `=`".into())),
                };
                ix += len;
                Kind::Op(op)
            }
            _ => {
                let start = ix;
                while ix < chars.len()
                    && !chars[ix].is_whitespace()
                    && !SYMBOLS.contains(&chars[ix])
                {
                    ix += 1;
                }
                Kind::Word(chars[start..ix].iter().collect())
            }
        };
        out.push(Token { kind, column });
    }
    Ok(out)
}

fn is_keyword(token: Option<&Token>, keyword: &str) -> bool {
    matches!(token, Some(Token { kind: Kind::Word(word), .. }) if word.eq_ignore_ascii_case(keyword))
}

struct LineParser<'a> {
    schema: &'a Schema,
    tokens: Vec<Token>,
    pos: usize,
    line: usize,
    /// Column just past the end of the line, for errors about something missing.
    end: usize,
}

impl LineParser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        token
    }

    fn error(&self, column: usize, message: impl Into<String>) -> QueryError {
        QueryError {
            line: self.line,
            column,
            message: message.into(),
        }
    }

    fn here(&self) -> usize {
        self.peek().map_or(self.end, |token| token.column)
    }

    fn column(&self, name: &str, at: usize) -> Result<ColumnId, QueryError> {
        let fields = self.schema.fields();
        if let Some(ix) = fields.iter().position(|field| field.name() == name) {
            return Ok(ColumnId(ix));
        }
        let similar: Vec<&str> = fields
            .iter()
            .map(|field| field.name().as_str())
            .filter(|field| field.to_lowercase() == name.to_lowercase())
            .collect();
        Err(match similar.as_slice() {
            [one] => self.error(
                at,
                format!(
                    "no column named {}; did you mean {}?",
                    quote_name(name),
                    quote_name(one)
                ),
            ),
            _ => self.error(at, format!("no column named {}", quote_name(name))),
        })
    }

    /// A value: a word or a quoted text.
    fn value(&mut self, what: &str) -> Result<(String, usize), QueryError> {
        let at = self.here();
        match self.next() {
            Some(Token {
                kind: Kind::Word(text) | Kind::Quoted(text),
                column,
            }) => Ok((text, column)),
            Some(token) => Err(self.error(token.column, format!("expected {what}"))),
            None => Err(self.error(at, format!("expected {what}"))),
        }
    }

    fn finish(&self, hint: &str) -> Result<(), QueryError> {
        match self.peek() {
            None => Ok(()),
            Some(token) => Err(self.error(token.column, hint.to_string())),
        }
    }

    fn find(&mut self) -> Result<String, QueryError> {
        let (value, _) = self.value("a value to find")?;
        self.finish("quote a value that has spaces, as in find \"a b\"")?;
        Ok(value)
    }

    fn sort(&mut self) -> Result<SortKey, QueryError> {
        let at = self.here();
        let (name, descending) = match self.next() {
            Some(Token {
                kind: Kind::Word(word),
                ..
            }) if word == "-" => match self.next() {
                Some(Token {
                    kind: Kind::Name(name),
                    ..
                }) => (name, true),
                _ => return Err(self.error(at, "expected a column after `-`")),
            },
            Some(Token {
                kind: Kind::Word(word),
                ..
            }) => match word.strip_prefix('-') {
                Some(name) => (name.to_string(), true),
                None => (word, false),
            },
            Some(Token {
                kind: Kind::Name(name),
                ..
            }) => (name, false),
            _ => return Err(self.error(at, "expected a column to sort by")),
        };
        let column = self.column(&name, at)?;
        self.finish("sort takes one column")?;
        let field = &self.schema.fields()[column.0];
        if !is_sortable(field.data_type()) {
            return Err(self.error(
                at,
                format!(
                    "{} ({}) cannot be sorted",
                    quote_name(field.name()),
                    field.data_type()
                ),
            ));
        }
        Ok(SortKey { column, descending })
    }

    fn condition(&mut self) -> Result<FilterCondition, QueryError> {
        let at = self.here();
        let name = match self.next() {
            Some(Token {
                kind: Kind::Word(name) | Kind::Name(name),
                ..
            }) => name,
            Some(token) => return Err(self.error(token.column, "expected a column")),
            None => return Err(self.error(at, "expected a column")),
        };
        let column = self.column(&name, at)?;
        let op_at = self.here();
        let op = match self.next() {
            Some(Token {
                kind: Kind::Op(op), ..
            }) => op,
            Some(Token {
                kind: Kind::Word(word),
                ..
            }) if word.eq_ignore_ascii_case("contains") => FilterOp::Contains,
            Some(Token {
                kind: Kind::Word(word),
                ..
            }) if word.eq_ignore_ascii_case("is") => {
                let not = is_keyword(self.peek(), "not");
                if not {
                    self.pos += 1;
                }
                if !is_keyword(self.peek(), "null") {
                    return Err(self.error(self.here(), "expected `null`"));
                }
                self.pos += 1;
                if not {
                    FilterOp::IsNotNull
                } else {
                    FilterOp::IsNull
                }
            }
            _ => {
                return Err(self.error(
                    op_at,
                    "expected an operator: == != < <= > >= contains, is null, is not null",
                ));
            }
        };
        let (value, value_at) = if op.takes_value() {
            self.value("a value")?
        } else {
            (String::new(), op_at)
        };
        let data_type = self.schema.fields()[column.0].data_type();
        if !filter_ops(data_type).contains(&op) {
            return Err(self.error(
                op_at,
                format!("`{}` cannot be used on {data_type} columns", op_text(op)),
            ));
        }
        let condition = FilterCondition { column, op, value };
        check_condition(self.schema, &condition)
            .map_err(|message| self.error(value_at, message.trim_end_matches('.').to_string()))?;
        Ok(condition)
    }

    /// A whole `filter` line, an expression with nothing after it, added to `clauses`.
    fn filter(&mut self, clauses: &mut Vec<Vec<FilterCondition>>) -> Result<(), QueryError> {
        let expr = self.expression()?;
        match self.peek() {
            None => {}
            Some(Token {
                kind: Kind::Close,
                column,
            }) => return Err(self.error(*column, "this `)` has no matching `(`")),
            Some(token) => {
                return Err(self.error(
                    token.column,
                    "expected `and` or `or` before another condition, or a quoted value",
                ));
            }
        }
        self.all_of(expr, clauses)
    }

    /// Items joined by one word, `and` or `or`.
    fn expression(&mut self) -> Result<Expr, QueryError> {
        let first = self.atom()?;
        let at = first.at();
        let mut items = vec![first];
        let mut op = None;
        loop {
            let next = if is_keyword(self.peek(), "and") {
                LogicOp::And
            } else if is_keyword(self.peek(), "or") {
                LogicOp::Or
            } else {
                break;
            };
            if op.is_some_and(|op| op != next) {
                return Err(self.error(
                    self.here(),
                    "use parentheses to mix `and` and `or`, as in (a == 1 or a == 2) and b == 3",
                ));
            }
            op = Some(next);
            self.pos += 1;
            items.push(self.atom()?);
        }
        Ok(match op {
            None => items.pop().expect("one item"),
            Some(op) => Expr::Group { op, items, at },
        })
    }

    /// A condition or a parenthesized expression.
    fn atom(&mut self) -> Result<Expr, QueryError> {
        let Some(Token {
            kind: Kind::Open,
            column,
        }) = self.peek().cloned()
        else {
            let at = self.here();
            return Ok(Expr::Condition(self.condition()?, at));
        };
        self.pos += 1;
        let expr = match self.expression()? {
            Expr::Group { op, items, .. } => Expr::Group {
                op,
                items,
                at: column,
            },
            condition => condition,
        };
        match self.next() {
            Some(Token {
                kind: Kind::Close, ..
            }) => Ok(expr),
            _ => Err(self.error(column, "this `(` is not closed")),
        }
    }

    /// Adds an expression that must hold to `clauses`.
    fn all_of(
        &self,
        expr: Expr,
        clauses: &mut Vec<Vec<FilterCondition>>,
    ) -> Result<(), QueryError> {
        match expr {
            Expr::Condition(condition, _) => clauses.push(vec![condition]),
            Expr::Group {
                op: LogicOp::And,
                items,
                ..
            } => {
                for item in items {
                    self.all_of(item, clauses)?;
                }
            }
            Expr::Group {
                op: LogicOp::Or,
                items,
                ..
            } => {
                let mut clause = Vec::new();
                for item in items {
                    self.any_of(item, &mut clause)?;
                }
                clauses.push(clause);
            }
        }
        Ok(())
    }

    /// Adds the conditions of an expression joined by `or` to `clause`, all on one column.
    fn any_of(&self, expr: Expr, clause: &mut Vec<FilterCondition>) -> Result<(), QueryError> {
        match expr {
            Expr::Condition(condition, at) => {
                if let Some(first) = clause.first()
                    && first.column != condition.column
                {
                    let name = |column: ColumnId| quote_name(self.schema.fields()[column.0].name());
                    return Err(self.error(
                        at,
                        format!(
                            "`or` joins conditions on one column only; this one is on {}, not {}",
                            name(condition.column),
                            name(first.column)
                        ),
                    ));
                }
                clause.push(condition);
            }
            Expr::Group {
                op: LogicOp::Or,
                items,
                ..
            } => {
                for item in items {
                    self.any_of(item, clause)?;
                }
            }
            Expr::Group {
                op: LogicOp::And,
                at,
                ..
            } => {
                return Err(self.error(at, "conditions joined by `and` cannot be inside `or`"));
            }
        }
        Ok(())
    }
}

/// Parses a query against the schema. Column names must match exactly, and values must parse as their column
/// types.
pub fn parse_query(schema: &Schema, text: &str) -> Result<Query, QueryError> {
    let mut query = Query::default();
    let mut clauses = Vec::new();
    let (mut find_line, mut sort_line) = (None, None);
    for (ix, line) in text.lines().enumerate() {
        let line_no = ix + 1;
        let tokens = tokens(line, line_no)?;
        let Some(first) = tokens.first().cloned() else {
            continue;
        };
        let mut parser = LineParser {
            schema,
            tokens,
            pos: 1,
            line: line_no,
            end: line.chars().count() + 1,
        };
        let command = match &first.kind {
            Kind::Word(word) => word.to_lowercase(),
            _ => String::new(),
        };
        match command.as_str() {
            "find" => {
                if let Some(earlier) = find_line {
                    return Err(parser.error(
                        first.column,
                        format!("only one find line is allowed; line {earlier} already finds"),
                    ));
                }
                find_line = Some(line_no);
                query.find = Some(parser.find()?);
            }
            "filter" => parser.filter(&mut clauses)?,
            "sort" => {
                if let Some(earlier) = sort_line {
                    return Err(parser.error(
                        first.column,
                        format!("only one sort line is allowed; line {earlier} already sorts"),
                    ));
                }
                sort_line = Some(line_no);
                query.view.sort = Some(parser.sort()?);
            }
            _ => {
                return Err(parser.error(first.column, "expected find, filter or sort"));
            }
        }
    }
    query.view.filter = Filter::from_clauses(clauses);
    Ok(query)
}

/// A column name as written in queries: as is when that reads back the same, otherwise in backticks.
pub fn quote_name(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|ch| !ch.is_whitespace() && !ch.is_control() && !SYMBOLS.contains(&ch));
    if plain {
        name.to_string()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// A value as written in queries: quoted when it would not read back as one word.
fn quote_value(value: &str, always: bool) -> String {
    let plain = !always
        && !value.is_empty()
        && value
            .chars()
            .all(|ch| !ch.is_whitespace() && !ch.is_control() && !SYMBOLS.contains(&ch));
    if plain {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A condition's value as written in queries. Values of string columns are always quoted.
pub fn format_value(schema: &Schema, column: ColumnId, value: &str) -> String {
    let text = schema
        .fields()
        .get(column.0)
        .is_some_and(|field| value_class(field.data_type()) == ValueClass::Text);
    quote_value(value, text)
}

/// One condition, such as `status == "활성"` or `age is null`.
pub fn format_condition(schema: &Schema, condition: &FilterCondition) -> String {
    let name = schema.fields().get(condition.column.0).map_or_else(
        || format!("#{}", condition.column.0),
        |field| quote_name(field.name()),
    );
    let op = op_text(condition.op);
    if condition.op.takes_value() {
        let value = format_value(schema, condition.column, &condition.value);
        format!("{name} {op} {value}")
    } else {
        format!("{name} {op}")
    }
}

/// The sort as written after `sort`, such as `-created_at`.
pub fn format_sort(schema: &Schema, sort: SortKey) -> String {
    let name = schema.fields().get(sort.column.0).map_or_else(
        || format!("#{}", sort.column.0),
        |field| quote_name(field.name()),
    );
    if sort.descending {
        format!("-{name}")
    } else {
        name
    }
}

/// A filter as written after `filter`, with each set of conditions joined by `or` in parentheses.
pub fn format_filter(schema: &Schema, filter: &Filter) -> String {
    let clauses: Vec<String> = filter
        .clauses()
        .iter()
        .map(|clause| match clause.conditions() {
            [one] => format_condition(schema, one),
            several => {
                let conditions: Vec<String> = several
                    .iter()
                    .map(|condition| format_condition(schema, condition))
                    .collect();
                format!("({})", conditions.join(" or "))
            }
        })
        .collect();
    clauses.join(" and ")
}

/// The whole query: a `find` line, a `filter` line and a `sort` line, each only when set.
pub fn format_query(schema: &Schema, query: &Query) -> String {
    let mut lines = Vec::new();
    if let Some(find) = &query.find {
        lines.push(format!("find {}", quote_value(find, false)));
    }
    if !query.view.filter.is_empty() {
        lines.push(format!(
            "filter {}",
            format_filter(schema, &query.view.filter)
        ));
    }
    if let Some(sort) = query.view.sort {
        lines.push(format!("sort {}", format_sort(schema, sort)));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use arrow_schema::{DataType, Field, TimeUnit};

    use super::*;

    fn schema() -> Schema {
        Schema::new(vec![
            Field::new("age", DataType::Int32, true),
            Field::new("status", DataType::Utf8, true),
            Field::new(
                "created_at",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                true,
            ),
            Field::new("운행 일자", DataType::LargeUtf8, true),
            Field::new("price", DataType::Float64, true),
            Field::new("blob", DataType::Binary, true),
            Field::new("a`b", DataType::Utf8, true),
            Field::new(
                "tags",
                DataType::List(Field::new("item", DataType::Utf8, true).into()),
                true,
            ),
        ])
    }

    fn condition(column: usize, op: FilterOp, value: &str) -> FilterCondition {
        FilterCondition {
            column: ColumnId(column),
            op,
            value: value.into(),
        }
    }

    fn error_at(text: &str) -> (usize, usize, String) {
        let error = parse_query(&schema(), text).unwrap_err();
        (error.line, error.column, error.message)
    }

    #[test]
    fn reads_the_example_query() {
        let query = parse_query(
            &schema(),
            "find 홍길동\nfilter age >= 30 and status == \"활성\"\nsort -created_at",
        )
        .unwrap();
        assert_eq!(query.find.as_deref(), Some("홍길동"));
        assert_eq!(
            query.view.filter,
            Filter::all(vec![
                condition(0, FilterOp::GtEq, "30"),
                condition(1, FilterOp::Eq, "활성")
            ])
        );
        assert_eq!(
            query.view.sort,
            Some(SortKey {
                column: ColumnId(2),
                descending: true
            })
        );
    }

    #[test]
    fn reads_every_operator_and_quoting() {
        let query = parse_query(
            &schema(),
            "FILTER `운행 일자` contains \"2025 05\" AND status != x and age < 1 and age <= 2\n\
             \n\
             filter age > -3 and price = 1.5 and status is null and blob is not NULL and `a``b` == \"q\\\"\\n\\\\\"\n\
             Sort `운행 일자`",
        )
        .unwrap();
        assert_eq!(
            query.view.filter,
            Filter::all(vec![
                condition(3, FilterOp::Contains, "2025 05"),
                condition(1, FilterOp::NotEq, "x"),
                condition(0, FilterOp::Lt, "1"),
                condition(0, FilterOp::LtEq, "2"),
                condition(0, FilterOp::Gt, "-3"),
                condition(4, FilterOp::Eq, "1.5"),
                condition(1, FilterOp::IsNull, ""),
                condition(5, FilterOp::IsNotNull, ""),
                condition(6, FilterOp::Eq, "q\"\n\\"),
            ])
        );
        assert_eq!(query.view.sort.unwrap().column, ColumnId(3));
        assert!(!query.view.sort.unwrap().descending);
        assert_eq!(query.find, None);
        let descending = parse_query(&schema(), "sort -`운행 일자`").unwrap();
        assert!(descending.view.sort.unwrap().descending);
        assert_eq!(
            parse_query(&schema(), "filter status == \"C:\\path\"")
                .unwrap()
                .view
                .filter
                .conditions()[0]
                .value,
            "C:\\path"
        );
    }

    #[test]
    fn writes_queries_that_read_back_the_same() {
        let query = Query {
            find: Some("a \"b\"".into()),
            view: ViewSpec {
                filter: Filter::all(vec![
                    condition(1, FilterOp::Eq, "중구01"),
                    condition(0, FilterOp::GtEq, "30"),
                    condition(0, FilterOp::Lt, "40"),
                    condition(3, FilterOp::Contains, "줄\n바꿈\t"),
                    condition(6, FilterOp::NotEq, ""),
                    condition(5, FilterOp::Eq, "00ff"),
                    condition(4, FilterOp::IsNotNull, ""),
                    condition(2, FilterOp::GtEq, "2025-05-06T08:00:00"),
                ]),
                sort: Some(SortKey {
                    column: ColumnId(3),
                    descending: true,
                }),
            },
        };
        let text = format_query(&schema(), &query);
        assert_eq!(
            text,
            "find \"a \\\"b\\\"\"\n\
             filter status == \"중구01\" and age >= 30 and age < 40 and `운행 일자` contains \"줄\\n바꿈\\t\" \
             and `a``b` != \"\" and blob == 00ff and price is not null and created_at >= 2025-05-06T08:00:00\n\
             sort -`운행 일자`"
        );
        assert_eq!(parse_query(&schema(), &text).unwrap(), query);
        assert_eq!(format_query(&schema(), &Query::default()), "");
        assert_eq!(parse_query(&schema(), "  \n").unwrap(), Query::default());
    }

    #[test]
    fn reads_or_within_one_column() {
        // Parentheses hold `or`; lines combine with `and`.
        let grouped = parse_query(
            &schema(),
            "filter (age == 1 or age == 2) and status == x
filter price > 1 OR price < -1",
        )
        .unwrap();
        assert_eq!(
            grouped.view.filter,
            Filter::from_clauses(vec![
                vec![
                    condition(0, FilterOp::Eq, "1"),
                    condition(0, FilterOp::Eq, "2")
                ],
                vec![condition(1, FilterOp::Eq, "x")],
                vec![
                    condition(4, FilterOp::Gt, "1"),
                    condition(4, FilterOp::Lt, "-1")
                ],
            ])
        );
        // A line of `or` alone needs no parentheses, and nested `or` flattens.
        assert_eq!(
            parse_query(
                &schema(),
                "filter status == a or (status == b or status is null)"
            )
            .unwrap()
            .view
            .filter,
            Filter::from_clauses(vec![vec![
                condition(1, FilterOp::Eq, "a"),
                condition(1, FilterOp::Eq, "b"),
                condition(1, FilterOp::IsNull, ""),
            ]])
        );
        // Redundant parentheses leave no trace, and each column's conditions come together.
        assert_eq!(
            parse_query(
                &schema(),
                "filter ((age == 1)) and (status == x and age == 2)"
            )
            .unwrap()
            .view
            .filter,
            Filter::all(vec![
                condition(0, FilterOp::Eq, "1"),
                condition(0, FilterOp::Eq, "2"),
                condition(1, FilterOp::Eq, "x"),
            ])
        );
    }

    #[test]
    fn writes_or_in_parentheses() {
        let filter = Filter::from_clauses(vec![
            vec![condition(1, FilterOp::Eq, "중구01")],
            vec![
                condition(0, FilterOp::Lt, "10"),
                condition(0, FilterOp::GtEq, "20"),
            ],
            vec![condition(0, FilterOp::NotEq, "15")],
            vec![condition(4, FilterOp::IsNull, "")],
        ]);
        let query = Query {
            view: ViewSpec { filter, sort: None },
            find: None,
        };
        let text = format_query(&schema(), &query);
        assert_eq!(
            text,
            "filter status == \"중구01\" and (age < 10 or age >= 20) and age != 15 and price is null"
        );
        assert_eq!(parse_query(&schema(), &text).unwrap(), query);
        // Values and names with parentheses are quoted.
        let odd = Query {
            view: ViewSpec {
                filter: Filter::all(vec![condition(1, FilterOp::Eq, "f(x)")]),
                sort: None,
            },
            find: Some("(a)".into()),
        };
        let text = format_query(&schema(), &odd);
        assert_eq!(text, "find \"(a)\"\nfilter status == \"f(x)\"");
        assert_eq!(parse_query(&schema(), &text).unwrap(), odd);
        assert_eq!(quote_name("f(x)"), "`f(x)`");
    }

    #[test]
    fn points_at_what_is_wrong() {
        assert_eq!(
            error_at("filter (age >= 30 or status == \"a\""),
            (1, 8, "this `(` is not closed".into())
        );
        assert_eq!(
            error_at("filter age >= 30) and status == a"),
            (1, 17, "this `)` has no matching `(`".into())
        );
        assert_eq!(error_at("filter age >= 30 nor age < 2").1, 18);
        assert_eq!(error_at("filter age == 1 xor age == 2").1, 17);
        assert_eq!(
            error_at("filter age == 1 or status == a"),
            (
                1,
                20,
                "`or` joins conditions on one column only; this one is on status, not age".into()
            )
        );
        assert_eq!(error_at("filter age == 1 and age == 2 or age == 3").1, 30);
        assert_eq!(
            error_at("filter (age == 1 and status == a) or age == 2"),
            (
                1,
                8,
                "conditions joined by `and` cannot be inside `or`".into()
            )
        );
        assert_eq!(error_at("filter () and age == 1").1, 9);
        assert_eq!(
            error_at("filter Age == 1").2,
            "no column named Age; did you mean age?"
        );
        assert_eq!(error_at("filter nope == 1").2, "no column named nope");
        assert_eq!(error_at("filter age == abc").1, 15);
        assert_eq!(error_at("filter age contains 1").1, 12);
        assert_eq!(error_at("filter status ~ 1").1, 15);
        assert_eq!(error_at("filter status == \"open").1, 18);
        assert_eq!(error_at("filter `운행 일자 == 1").1, 8);
        assert_eq!(error_at("filter status is nul").1, 18);
        assert_eq!(error_at("filter status == a b").1, 20);
        assert_eq!(error_at("find 홍 길동").1, 8);
        assert_eq!(error_at("sort age price").1, 10);
        assert!(error_at("sort tags").2.ends_with("cannot be sorted"));
        assert_eq!(error_at("sort age\nsort -age").0, 2);
        assert_eq!(error_at("find a\nfind b").0, 2);
        assert_eq!(error_at("filters age == 1").1, 1);
        assert_eq!(error_at("\n  filter age ==").1, 16);
    }
}
