use std::collections::HashMap;
/*
 * Copyright 2024 Oxide Computer Company
 */

use anyhow::{bail, Result};

pub struct Expansion {
    chunks: Vec<Chunk>,
}

enum Chunk {
    Char(char),
    Simple(String),
    IfLiteral(String, String),
}

fn is_variable_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/*
 * Current expansion forms:
 *
 *  ${variable?literal}      expand to "literal" if variable is defined,
 *                           otherwise the empty string
 *  ${variable}              expand to "variable" if set, or error if not
 */
fn expand(expand: &str) -> Result<Chunk> {
    enum State {
        Variable,
        Literal,
    }

    let mut s = State::Variable;
    let mut chars = expand.chars();
    let mut variable = String::new();
    let mut literal = String::new();

    loop {
        match s {
            State::Variable => match chars.next() {
                Some('?') => {
                    if variable.is_empty() {
                        bail!("empty variable unexpected");
                    }
                    s = State::Literal;
                }
                Some(c) if is_variable_char(c) => variable.push(c),
                Some(c) => bail!("unexpected char in variable name: {:?}", c),
                None => {
                    if variable.is_empty() {
                        bail!("empty variable unexpected");
                    }
                    return Ok(Chunk::Simple(variable));
                }
            },
            State::Literal => match chars.next() {
                Some(c) => literal.push(c),
                None => return Ok(Chunk::IfLiteral(variable, literal)),
            },
        }
    }
}

impl Expansion {
    pub fn parse(template: &str) -> Result<Expansion> {
        enum State {
            Rest,
            Dollar,
            Expansion,
        }

        let mut s = State::Rest;
        let mut chars = template.chars();
        let mut chunks = Vec::new();
        let mut exp = String::new();

        loop {
            match s {
                State::Rest => match chars.next() {
                    Some('$') => {
                        s = State::Dollar;
                    }
                    Some(c) => {
                        chunks.push(Chunk::Char(c));
                    }
                    None => {
                        return Ok(Expansion { chunks });
                    }
                },
                State::Dollar => match chars.next() {
                    Some('$') => {
                        chunks.push(Chunk::Char('$'));
                        s = State::Rest;
                    }
                    Some('{') => {
                        s = State::Expansion;
                    }
                    Some(c) => {
                        bail!("expected $ or {{ after $, not {:?}", c);
                    }
                    None => {
                        bail!("unexpected end of string after $");
                    }
                },
                State::Expansion => match chars.next() {
                    Some('}') => {
                        chunks.push(expand(&exp)?);
                        exp.clear();
                        s = State::Rest;
                    }
                    Some('$') => {
                        bail!("no nesting in expansions for now");
                    }
                    Some(c) => {
                        exp.push(c);
                    }
                    None => {
                        bail!("unexpected end of string after ${{");
                    }
                },
            }
        }
    }

    pub fn evaluate(
        &self,
        variables: &HashMap<String, String>,
    ) -> Result<String> {
        let mut out = String::new();

        for ch in self.chunks.iter() {
            match ch {
                Chunk::Char(c) => {
                    out.push(*c);
                }
                Chunk::Simple(f) => {
                    if let Some(v) = variables.get(f) {
                        out.push_str(v);
                    } else {
                        bail!("variable {:?} not defined", f);
                    }
                }
                Chunk::IfLiteral(f, l) => {
                    if variables.contains_key(f) {
                        out.push_str(l);
                    }
                }
            }
        }

        Ok(out)
    }
}
