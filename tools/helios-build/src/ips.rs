/*
 * This code should eventually be in a crate that exports things as public
 * methods instead of vendored here, but for now we'll just ignore unused code.
 */
#![allow(unused)]

use anyhow::{bail, Result};
use std::convert::{TryFrom, TryInto};
use std::collections::BTreeSet;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DependType {
    Incorporate,
    Require,
    RequireAny,
    Group,
    GroupAny,
    Optional,
    Conditional,
}

impl TryFrom<String> for DependType {
    type Error = anyhow::Error;

    fn try_from(s: String) -> Result<DependType> {
        s.as_str().try_into()
    }
}

impl TryFrom<&str> for DependType {
    type Error = anyhow::Error;

    fn try_from(s: &str) -> Result<DependType> {
        Ok(match s {
            "incorporate" => DependType::Incorporate,
            "require" => DependType::Require,
            "require-any" => DependType::RequireAny,
            "group" => DependType::Group,
            "group-any" => DependType::GroupAny,
            "optional" => DependType::Optional,
            "conditional" => DependType::Conditional,
            n => bail!("unknown depend type {:?}", n),
        })
    }
}

#[derive(Debug)]
pub struct ActionDepend {
    fmri: Vec<String>,
    type_: DependType,
    predicate: Vec<String>,
    variant_zone: Option<String>,
}

impl ActionDepend {
    pub fn fmris(&self) -> Vec<&str> {
        self.fmri.iter().map(|x| x.as_str()).collect()
    }

    pub fn type_(&self) -> DependType {
        self.type_
    }
}

#[derive(Debug)]
pub enum Action {
    Depend(ActionDepend),
    Unknown(String, Vec<String>, Vals),
}

#[derive(Debug)]
enum ParseState {
    Rest,
    Type,
    Key,
    Value,
    ValueQuoted,
    ValueQuotedSpace,
    ValueUnquoted,
}

#[derive(Debug)]
pub struct Vals {
    vals: Vec<(String, String)>,
    extra: BTreeSet<String>,
}

impl Vals {
    fn new() -> Vals {
        Vals {
            vals: Vec::new(),
            extra: BTreeSet::new(),
        }
    }

    fn insert(&mut self, key: &str, value: &str) {
        /*
         * XXX Ignore "facet.*" properties for now...
         */
        if key.starts_with("facet.") {
            return;
        }

        self.vals.push((key.to_string(), value.to_string()));
        self.extra.insert(key.to_string());
    }

    fn maybe_single(&mut self, name: &str) -> Result<Option<String>> {
        let mut out: Option<String> = None;

        for (k, v) in self.vals.iter() {
            if k == name {
                if out.is_some() {
                    bail!("more than one value for {}, wanted a single value",
                        name);
                }
                out = Some(v.to_string());
            }
        }

        self.extra.remove(name);
        Ok(out)
    }

    fn single(&mut self, name: &str) -> Result<String> {
        let out = self.maybe_single(name)?;

        if let Some(out) = out {
            Ok(out)
        } else {
            bail!("no values for {} found", name);
        }
    }

    fn maybe_list(&mut self, name: &str) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();

        for (k, v) in self.vals.iter() {
            if k == name {
                out.push(v.to_string());
            }
        }

        self.extra.remove(name);
        Ok(out)
    }

    fn list(&mut self, name: &str) -> Result<Vec<String>> {
        let out = self.maybe_list(name)?;
        if out.is_empty() {
            bail!("wanted at least one value for {}, found none", name);
        }
        Ok(out)
    }

    fn check_for_extra(&self) -> Result<()> {
        if !self.extra.is_empty() {
            bail!("some properties present but not consumed: {:?}, {:?}",
                self.extra, self.vals);
        }

        Ok(())
    }
}

pub fn parse_manifest(input: &str) -> Result<Vec<Action>> {
    let mut out = Vec::new();

    for l in input.lines() {
        let mut s = ParseState::Rest;
        let mut a = String::new();
        let mut k = String::new();
        let mut v = String::new();
        let mut vals = Vals::new();
        let mut free: Vec<String> = Vec::new();
        let mut quote = '"';

        for c in l.chars() {
            match s {
                ParseState::Rest => {
                    if c.is_ascii_alphabetic() {
                        a.clear();
                        k.clear();
                        v.clear();

                        a.push(c);
                        s = ParseState::Type;
                    } else {
                        bail!("invalid line ({:?}): {}", s, l);
                    }
                }
                ParseState::Type => {
                    if c.is_ascii_alphabetic() {
                        a.push(c);
                    } else if c == ' ' {
                        s = ParseState::Key;
                    } else {
                        bail!("invalid line ({:?}): {}", s, l);
                    }
                }
                ParseState::Key => {
                    if c.is_ascii_alphanumeric()
                        || c == '.' || c == '-' || c == '_' || c == '/'
                        || c == '@'
                    {
                        k.push(c);
                    } else if c == ' ' {
                        free.push(k.clone());
                        k.clear();
                    } else if c == '=' {
                        s = ParseState::Value;
                    } else {
                        bail!("invalid line ({:?}, {}): {}", s, k, l);
                    }
                }
                ParseState::Value => {
                    /*
                     * This state represents the start of a new value, which
                     * will either be quoted or unquoted.
                     */
                    v.clear();
                    if c == '"' || c == '\'' {
                        /*
                         * Record the type of quote used at the start of the
                         * string so that we can match it with the same type
                         * of quote at the end.
                         */
                        quote = c;
                        s = ParseState::ValueQuoted;
                    } else {
                        s = ParseState::ValueUnquoted;
                        v.push(c);
                    }
                }
                ParseState::ValueQuoted => {
                    if c == '\\' {
                        /*
                         * XXX handle escaped quotes...
                         */
                        bail!("invalid line (backslash...): {}", l);
                    } else if c == quote {
                        s = ParseState::ValueQuotedSpace;
                    } else {
                        v.push(c);
                    }
                }
                ParseState::ValueQuotedSpace => {
                    /*
                     * We expect at least one space after a quoted string before
                     * the next key.
                     */
                    if c == ' ' {
                        vals.insert(&k, &v);
                        s = ParseState::Key;
                        k.clear();
                    } else {
                        bail!("invalid after quote ({:?}, {}): {}", s, k, l);
                    }
                }
                ParseState::ValueUnquoted => {
                    if c == '"' || c == '\'' {
                        bail!("invalid line (errant quote...): {}", l);
                    } else if c == ' ' {
                        vals.insert(&k, &v);
                        s = ParseState::Key;
                        k.clear();
                    } else {
                        v.push(c);
                    }
                }
            }
        }

        match s {
            ParseState::ValueQuotedSpace | ParseState::ValueUnquoted => {
                vals.insert(&k, &v);
            }
            ParseState::Type => {},
            _ => bail!("invalid line (terminal state {:?}: {}", s, l),
        }

        match a.as_str() {
            "depend" => {
                let fmri = vals.list("fmri")?;
                let type_ = vals.single("type")?.try_into()?;
                let predicate = vals.maybe_list("predicate")?;
                let variant_zone = vals.maybe_single(
                    "variant.opensolaris.zone")?;

                vals.check_for_extra()?;

                out.push(Action::Depend(ActionDepend {
                    fmri,
                    type_,
                    predicate,
                    variant_zone,
                }))
            }
            _ => out.push(Action::Unknown(a.to_string(), free, vals)),
        }
    }

    Ok(out)
}
