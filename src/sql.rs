//! Splitting a migration file into statements, and finding the table a statement talks about.
//!
//! A migration file is not a list of statements separated by `;`: it has comments, quoted strings,
//! dollar-quoted bodies and semicolons inside all of them. Splitting it with `split(';')` is how a
//! tool starts lying about what a migration does, so this is a small lexer instead — about sixty
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
