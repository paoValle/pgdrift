//! Splitting a migration file into statements, and finding the table a statement talks about.
//!
//! A migration file is not a list of statements separated by `;`: it has comments, quoted strings,
//! dollar-quoted bodies and semicolons inside all of them. Splitting it with `split(';')` is how a
//! tool starts lying about what a migration does, so this is a small lexer instead — about ninety
//! lines, and it is the part of `pgdrift` with the most tests.

/// A statement, ready to be classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// One-based position in the file.
    pub index: usize,
    /// The statement, comments removed and whitespace collapsed.
    pub sql: String,
}

/// A statement that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Where.
    pub detail: String,
}

/// Splits SQL into statements, ignoring semicolons inside strings, comments and dollar quotes.
pub fn split(input: &str) -> Result<Vec<Statement>, ParseError> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut dollar_tag: Option<String> = None;

    while let Some(c) = chars.next() {
        // inside a dollar-quoted body: copy verbatim until the closing tag
        if let Some(tag) = dollar_tag.clone() {
            if c == '$' && peek_tag(&mut chars, &tag) {
                for _ in 0..tag.len() - 1 {
                    chars.next();
                }
                current.push_str(&tag);
                dollar_tag = None;
            } else {
                current.push(c);
            }
            continue;
        }

        match c {
            '\'' => {
                current.push(c);
                // a doubled quote is an escaped quote, not the end of the string
                while let Some(inner) = chars.next() {
                    current.push(inner);
                    if inner == '\'' {
                        if chars.peek() == Some(&'\'') {
                            current.push(chars.next().unwrap_or('\''));
                            continue;
                        }
                        break;
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        break;
                    }
                }
                current.push(' ');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for inner in chars.by_ref() {
                    if previous == '*' && inner == '/' {
                        break;
                    }
                    previous = inner;
                }
                current.push(' ');
            }
            '$' => {
                current.push(c);
                let mut tag = String::from("$");
                while let Some(&inner) = chars.peek() {
                    if inner == '$' {
                        chars.next();
                        tag.push('$');
                        break;
                    }
                    if inner.is_alphanumeric() || inner == '_' {
                        tag.push(inner);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if tag.len() > 1 {
                    dollar_tag = Some(tag);
                }
            }
            ';' => {
                push(&mut statements, &mut current);
            }
            _ => current.push(c),
        }
    }
    if dollar_tag.is_some() {
        return Err(ParseError {
            detail: "a dollar-quoted string was never closed".to_owned(),
        });
    }
    push(&mut statements, &mut current);
    Ok(statements)
}

fn push(statements: &mut Vec<Statement>, current: &mut String) {
    let sql = collapse(current);
    if !sql.is_empty() {
        statements.push(Statement {
            index: statements.len() + 1,
            sql,
        });
    }
    current.clear();
}

fn peek_tag(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, tag: &str) -> bool {
    // the closing tag is `$tag$`; we are standing on the `$`, so the rest must follow
    let rest: String = chars.clone().take(tag.len() - 1).collect();
    rest == tag[1..]
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The table a statement operates on, when it names exactly one and we can find it.
///
/// It is deliberately conservative: an incorrect guess would make `prove` measure the wrong table,
/// and a wrong measurement is worse than a skipped statement.
#[must_use]
pub fn target_table(sql: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let rest = if upper.starts_with("ALTER TABLE") {
        &sql["ALTER TABLE".len()..]
    } else if upper.starts_with("LOCK TABLE") {
        &sql["LOCK TABLE".len()..]
    } else if upper.starts_with("TRUNCATE") {
        &sql["TRUNCATE".len()..]
    } else if upper.starts_with("DROP TABLE") {
        &sql["DROP TABLE".len()..]
    } else if upper.starts_with("CREATE TABLE") {
        &sql["CREATE TABLE".len()..]
    } else if upper.contains(" INDEX ") {
        // CREATE [UNIQUE] INDEX [CONCURRENTLY] [IF NOT EXISTS] name ON table
        let position = upper.find(" INDEX ")?;
        let on = upper[position..].find(" ON ")?;
        &sql[position + on + 4..]
    } else {
        return None;
    };

    let rest = rest
        .trim_start()
        .trim_start_matches("ONLY")
        .trim_start_matches("IF NOT EXISTS")
        .trim_start()
        .trim_start_matches("CONCURRENTLY")
        .trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '"')
        .collect();
    let name = name.trim_matches('"').to_owned();
    (!name.is_empty()).then_some(name)
}

/// The table a foreign key points at, for the probe to create it first.
#[must_use]
pub fn referenced_table(sql: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let position = upper.find("REFERENCES")?;
    let rest = sql[position + "REFERENCES".len()..].trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '"')
        .collect();
    (!name.is_empty()).then_some(name.trim_matches('"').to_owned())
}

/// Where a statement names a column, which is what decides the type the probe gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnUse {
    /// `ALTER COLUMN <name>`, `SET NOT NULL`, a type change: any type will do.
    Alter,
    /// A column in an index list, or in a foreign key.
    Index,
    /// A column in a `FOREIGN KEY` list: its type has to match what it references.
    ForeignKey,
}

/// A column a statement names, and where it names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedColumn {
    /// The column name, quotes removed.
    pub name: String,
    /// Where the statement named it.
    pub use_: ColumnUse,
}

/// The columns a statement names, in the places this can read without guessing.
///
/// The probe adds a named column to the scratch table when it is missing: a statement that alters,
/// indexes or references a column nobody has invented yet otherwise fails with a database error and
/// is reported as not measurable, which is honest and useless.
///
/// It is deliberately conservative. It returns nothing rather than a guess, because a column
/// invented from a wrong guess would be measured against a shape the statement does not expect —
/// the failure this tool exists to avoid. An expression index (`(lower(email))`), a column list
/// this cannot read as plain names, or a statement that names nothing all give an empty list.
#[must_use]
pub fn named_columns(sql: &str) -> Vec<NamedColumn> {
    let upper = sql.to_uppercase();
    let mut names: Vec<NamedColumn> = Vec::new();

    let mut push = |name: String, use_: ColumnUse| {
        if let Some(existing) = names.iter_mut().find(|named| named.name == name) {
            // a foreign key decides the type even when the column is also indexed or altered
            if use_ == ColumnUse::ForeignKey {
                existing.use_ = use_;
            }
            return;
        }
        names.push(NamedColumn { name, use_ });
    };

    if let Some(position) = upper.find("ALTER COLUMN ") {
        // the grammar fixes this one: `ALTER COLUMN <name> SET | TYPE | DROP | ...`, so the next
        // token is the column and whatever follows it is the action, not a direction
        if let Some(name) = leading_identifier(&sql[position + "ALTER COLUMN ".len()..]) {
            push(name, ColumnUse::Alter);
        }
    }

    if let Some(position) = upper.find("FOREIGN KEY") {
        for name in key_list(&sql[position + "FOREIGN KEY".len()..]) {
            push(name, ColumnUse::ForeignKey);
        }
    }

    if upper.contains(" INDEX ") {
        // the index columns are the first parenthesised list after the table it is built on
        if let Some(position) = upper.find(" ON ") {
            for name in key_list(&sql[position + " ON ".len()..]) {
                push(name, ColumnUse::Index);
            }
        }
    }

    names
}

/// The column a foreign key references, when it spells one out: `REFERENCES orders (id)` -> `id`.
///
/// `REFERENCES orders` names the primary key without saying so, and the probe's primary key is a
/// `bigint`, so the caller defaults to that.
#[must_use]
pub fn referenced_column(sql: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let position = upper.find("REFERENCES")?;
    key_list(&sql[position + "REFERENCES".len()..])
        .into_iter()
        .next()
}

/// The names inside a parenthesised list, when every item is a plain column.
fn key_list(rest: &str) -> Vec<String> {
    let Some(open) = rest.find('(') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find(')') else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for item in rest[open + 1..open + close].split(',') {
        let Some(name) = column_of(item) else {
            return Vec::new();
        };
        names.push(name);
    }
    names
}

/// One item of a key or column list: a name, optionally followed by a direction and a null ordering.
///
/// Anything else — an expression, a function, an operator class — is not a column we can name, and
/// is reported as nothing rather than as the first token of an expression.
fn column_of(item: &str) -> Option<String> {
    let tokens: Vec<&str> = item.split_whitespace().collect();
    let (name, tail) = tokens.split_first()?;
    if !is_identifier(name) {
        return None;
    }
    let tail = tail.join(" ").to_uppercase();
    let allowed = [
        "",
        "ASC",
        "DESC",
        "NULLS FIRST",
        "NULLS LAST",
        "ASC NULLS FIRST",
        "ASC NULLS LAST",
        "DESC NULLS FIRST",
        "DESC NULLS LAST",
    ];
    allowed
        .contains(&tail.as_str())
        .then(|| name.trim_matches('"').to_owned())
}

/// The identifier at the start of `rest`, for the places where the grammar says one must be there.
fn leading_identifier(rest: &str) -> Option<String> {
    let name = rest.split_whitespace().next()?;
    is_identifier(name).then(|| name.trim_matches('"').to_owned())
}

fn is_identifier(text: &str) -> bool {
    // a quoted name is only a name when the whole token is the quoted name: `"My` is half of one
    let quoted = text.len() > 1 && text.starts_with('"') && text.ends_with('"');
    if text.contains('"') && !quoted {
        return false;
    }
    let name = text.trim_matches('"');
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}
