//! The numbers of the interface objects, written from `sdk/bindings.toml` into every language that
//! carries them (S1): the C header, the C# classes and the Rust runtime. The C++ classes use those
//! of the header.

use std::path::Path;

use crate::Result;
use crate::workspace::Workspace;

const SOURCE: &str = "sdk/bindings.toml";
/// The groups of numbers, in the order of the files.
const GROUPS: [&str; 3] = ["kind", "property", "signal"];
/// Lines of prose are wrapped at this width, notes after the code at the second.
const PROSE: usize = 100;
const CODE: usize = 120;

struct Entry {
    number: u32,
    name: String,
    c_name: String,
    note: Option<String>,
}

struct Group {
    key: &'static str,
    description: String,
    rust: String,
    csharp: String,
    c_prefix: String,
    entries: Vec<Entry>,
}

/// Writes the numbers into the files, or with `check`, fails when one differs.
pub fn run(check: bool) -> Result {
    let ws = Workspace::load()?;
    let groups = load(&ws.root.join(SOURCE))?;
    let header = ws.root.join("sdk").join("uniwow.h");
    let csharp = ws.root.join("sdk").join("UniWoW.cs");
    let rust = ws
        .root
        .join("core")
        .join("api")
        .join("src")
        .join("ui")
        .join("numbers.rs");
    let expected = [
        (header.clone(), regions(&read(&header)?, &groups, "/* ", " */", c_enum)?),
        (
            csharp.clone(),
            regions(&read(&csharp)?, &groups, "// ", "", csharp_enum)?,
        ),
        (rust.clone(), rust_file(&groups)),
    ];
    let mut stale = Vec::new();
    for (path, text) in &expected {
        let current = read(path).unwrap_or_default();
        if current.replace("\r\n", "\n") != *text {
            stale.push((path, text, current.contains("\r\n")));
        }
    }
    if check {
        if stale.is_empty() {
            println!("the numbers of {SOURCE} are those of every language");
            return Ok(());
        }
        let names: Vec<String> = stale.iter().map(|(path, _, _)| path.display().to_string()).collect();
        return Err(format!(
            "not written from {SOURCE}: {}; run cargo xtask bindings",
            names.join(", ")
        ));
    }
    for (path, text, crlf) in &stale {
        let text = if *crlf {
            text.replace('\n', "\r\n")
        } else {
            (*text).clone()
        };
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("written: {}", path.display());
    }
    if stale.is_empty() {
        println!("nothing to write");
    }
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn load(path: &Path) -> Result<Vec<Group>> {
    let table: toml::Table = toml::from_str(&read(path)?).map_err(|e| format!("{SOURCE}: {e}"))?;
    let mut groups = Vec::new();
    for key in GROUPS {
        let group = table
            .get(key)
            .and_then(|g| g.as_table())
            .ok_or_else(|| format!("{SOURCE}: no [{key}]"))?;
        let text = |name: &str| {
            group
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .ok_or_else(|| format!("{SOURCE}: [{key}] has no {name}"))
        };
        let mut entries = Vec::new();
        let listed = group
            .get("entries")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("{SOURCE}: [{key}] has no entries"))?;
        for item in listed {
            let fields = item.as_array().map(Vec::as_slice).unwrap_or_default();
            let entry = match fields {
                [number, name, c_name, rest @ ..] if rest.len() <= 1 => Entry {
                    number: number
                        .as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| format!("{SOURCE}: [{key}] {item}: the number"))?,
                    name: name.as_str().unwrap_or_default().to_owned(),
                    c_name: c_name.as_str().unwrap_or_default().to_owned(),
                    note: rest.first().and_then(|n| n.as_str()).map(str::to_owned),
                },
                _ => return Err(format!("{SOURCE}: [{key}] {item}: [number, name, C name, note]")),
            };
            if entry.name.is_empty() || entry.c_name.is_empty() {
                return Err(format!("{SOURCE}: [{key}] {item}: a name is missing"));
            }
            if entries
                .iter()
                .any(|e: &Entry| e.number == entry.number || e.name == entry.name)
            {
                return Err(format!("{SOURCE}: [{key}] {item}: number or name used twice"));
            }
            entries.push(entry);
        }
        groups.push(Group {
            key,
            description: text("description")?,
            rust: text("rust")?,
            csharp: text("csharp")?,
            c_prefix: text("c_prefix")?,
            entries,
        });
    }
    Ok(groups)
}

/// Words of `text` in lines of at most `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.len() + 1 + word.len() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// Lines of code, each followed by its note if it has one, the notes aligned and wrapped. A
/// comment without `close` runs to the end of its line: each line of a note opens it again.
fn with_notes(lines: &[(String, Option<&str>)], open: &str, close: &str) -> String {
    let column = lines.iter().map(|(code, _)| code.len()).max().unwrap_or(0) + 1;
    let mut out = String::new();
    for (code, note) in lines {
        let Some(note) = note else {
            out.push_str(code);
            out.push('\n');
            continue;
        };
        let room = CODE.saturating_sub(column + open.len() + close.len()).max(20);
        let wrapped = wrap(note, room);
        for (i, part) in wrapped.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("{code:column$}{open}{part}"));
            } else if close.is_empty() {
                out.push_str(&format!("{:column$}{open}{part}", ""));
            } else {
                out.push_str(&format!("{:width$}{part}", "", width = column + open.len()));
            }
            out.push_str(if i + 1 == wrapped.len() { close } else { "" });
            out.push('\n');
        }
    }
    out
}

fn c_enum(group: &Group) -> String {
    let mut out = String::new();
    for (i, line) in wrap(&group.description, PROSE - 6).iter().enumerate() {
        out.push_str(if i == 0 { "/* " } else { "\n   " });
        out.push_str(line);
    }
    out.push_str(" */\nenum {\n");
    let last = group.entries.len().saturating_sub(1);
    let lines: Vec<(String, Option<&str>)> = group
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let comma = if i == last { "" } else { "," };
            (
                format!("    {}{} = {}{comma}", group.c_prefix, e.c_name, e.number),
                e.note.as_deref(),
            )
        })
        .collect();
    out.push_str(&with_notes(&lines, "/* ", " */"));
    out.push_str("};\n");
    out
}

fn csharp_enum(group: &Group) -> String {
    let mut out = String::new();
    let summary = wrap(&group.description, PROSE - 4);
    for (i, line) in summary.iter().enumerate() {
        out.push_str("/// ");
        if i == 0 {
            out.push_str("<summary>");
        }
        out.push_str(line);
        if i + 1 == summary.len() {
            out.push_str("</summary>");
        }
        out.push('\n');
    }
    out.push_str(&format!("public enum {} : uint\n{{\n", group.csharp));
    let lines: Vec<(String, Option<&str>)> = group
        .entries
        .iter()
        .map(|e| (format!("    {} = {},", e.name, e.number), e.note.as_deref()))
        .collect();
    out.push_str(&with_notes(&lines, "// ", ""));
    out.push_str("}\n");
    out
}

/// Replaces, in `text`, what lies between the markers of each group by its numbers.
fn regions(text: &str, groups: &[Group], open: &str, close: &str, write: fn(&Group) -> String) -> Result<String> {
    let mut text = text.replace("\r\n", "\n");
    for group in groups {
        let begin = format!("{open}<generated {}>", group.key);
        let end = format!("{open}</generated {}>{close}", group.key);
        let start = text
            .find(&begin)
            .and_then(|at| text[at..].find('\n').map(|n| at + n + 1))
            .ok_or_else(|| format!("no line '{begin}…' to write the numbers after"))?;
        let stop = text[start..]
            .find(&end)
            .map(|at| start + at)
            .ok_or_else(|| format!("no line '{end}' after '{begin}'"))?;
        text.replace_range(start..stop, &write(group));
    }
    Ok(text)
}

fn rust_file(groups: &[Group]) -> String {
    let mut out = String::from(
        "//! The numbers of the interface objects, the same in every language (S1). Written by\n\
         //! `cargo xtask bindings` from `sdk/bindings.toml`: change them there.\n",
    );
    for group in groups {
        out.push_str("\nnumbered!(\n");
        for line in wrap(&group.description, PROSE - 8) {
            out.push_str(&format!("    /// {line}\n"));
        }
        out.push_str(&format!("    {} {{\n", group.rust));
        for entry in &group.entries {
            if let Some(note) = &entry.note {
                for line in wrap(note, PROSE - 12) {
                    out.push_str(&format!("        /// {line}\n"));
                }
            }
            out.push_str(&format!("        {} = {},\n", entry.name, entry.number));
        }
        out.push_str("    }\n);\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Entry, Group, c_enum, regions, wrap};

    fn group() -> Group {
        Group {
            key: "kind",
            description: "Kinds.".to_owned(),
            rust: "Kind".to_owned(),
            csharp: "Kind".to_owned(),
            c_prefix: "UNIWOW_".to_owned(),
            entries: vec![
                Entry {
                    number: 1,
                    name: "Panel".to_owned(),
                    c_name: "PANEL".to_owned(),
                    note: Some("a panel".to_owned()),
                },
                Entry {
                    number: 2,
                    name: "Label".to_owned(),
                    c_name: "LABEL".to_owned(),
                    note: None,
                },
            ],
        }
    }

    #[test]
    fn the_numbers_replace_what_lies_between_their_markers() {
        let text = "a\r\n/* <generated kind> from x */\nold\n/* </generated kind> */\nb\n";
        let written = regions(text, &[group()], "/* ", " */", c_enum).unwrap();
        assert_eq!(
            written,
            "a\n/* <generated kind> from x */\n/* Kinds. */\nenum {\n    UNIWOW_PANEL = 1, /* a panel */\n    \
             UNIWOW_LABEL = 2\n};\n/* </generated kind> */\nb\n"
        );
        assert!(regions("no markers", &[group()], "/* ", " */", c_enum).is_err());
    }

    #[test]
    fn long_notes_are_wrapped() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
    }
}
