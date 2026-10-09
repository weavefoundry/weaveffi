//! Godoc comments: a declaration's doc starts with its name ("Path returns
//! the path the store was opened with."), followed by any further
//! paragraphs and a final `Deprecated:` paragraph.

use crate::codegen::CodeWriter;
use weaveffi_model::model::ParamBinding;

/// The width generated comment text wraps at (IDL docs keep their own line
/// breaks).
const WRAP: usize = 76;

/// What a doc comment describes, which decides how a summary that doesn't
/// start with the name is joined to it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A function or method with a result: "The path ..." reads "Path
    /// returns the path ...".
    Func,
    /// A function or method without one: only a third-person verb or
    /// "Whether" joins the name directly.
    Proc,
    /// A type or field: "The store's clock" reads "Clock is the store's
    /// clock".
    Value,
    /// A variant, constant, or error code, whose IDL doc is often a clause
    /// ("An entry was stored."): only a third-person verb joins the name
    /// directly.
    Clause,
}

impl Kind {
    /// [`Kind::Func`] for a callable with a result, else [`Kind::Proc`].
    pub(crate) fn callable(returns: bool) -> Self {
        if returns {
            Kind::Func
        } else {
            Kind::Proc
        }
    }
}

/// A godoc comment under construction: the summary paragraph naming the
/// declaration, then further paragraphs, then `Deprecated:`.
pub(crate) struct GoDoc {
    name: String,
    paragraphs: Vec<Vec<String>>,
    deprecated: Option<String>,
}

impl GoDoc {
    /// A doc for `name` whose summary is the IDL doc `doc`, joined to the
    /// name per `kind`, or `fallback` (which must start with the name)
    /// when there's no IDL doc.
    pub(crate) fn new(name: &str, doc: Option<&str>, kind: Kind, fallback: Option<String>) -> Self {
        let mut paragraphs = Vec::new();
        match doc.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => {
                // Blank lines separate paragraphs.
                let mut current = Vec::new();
                for (i, line) in d.lines().enumerate() {
                    let line = line.trim_end();
                    if line.is_empty() {
                        if !current.is_empty() {
                            paragraphs.push(std::mem::take(&mut current));
                        }
                    } else if i == 0 {
                        current.push(lead(name, line, kind));
                    } else {
                        current.push(line.to_string());
                    }
                }
                if !current.is_empty() {
                    paragraphs.push(current);
                }
            }
            None => paragraphs.extend(fallback.map(|f| wrap(&f))),
        }
        Self {
            name: name.to_string(),
            paragraphs,
            deprecated: None,
        }
    }

    /// Append a paragraph of generated text, rewrapped. When it's the
    /// first, it's joined to the name ("Wait blocks until ...").
    pub(crate) fn para(mut self, text: &str) -> Self {
        let text = if self.paragraphs.is_empty() {
            format!("{} {}", self.name, lower_first(text))
        } else {
            text.to_string()
        };
        self.paragraphs.push(wrap(&text));
        self
    }

    /// Append a "Parameters:" list for the documented parameters, labelled
    /// with their Go spellings (`name`).
    pub(crate) fn params(mut self, params: &[ParamBinding], name: impl Fn(&str) -> String) -> Self {
        let mut list = Vec::new();
        for p in params {
            let Some(doc) = p.doc.as_deref().map(str::trim).filter(|d| !d.is_empty()) else {
                continue;
            };
            let mut lines = doc.lines();
            let first = lines.next().unwrap_or_default();
            list.push(format!("  - {}: {first}", name(&p.name)));
            list.extend(lines.map(|l| {
                if l.is_empty() {
                    String::new()
                } else {
                    format!("    {l}")
                }
            }));
        }
        if !list.is_empty() {
            if self.paragraphs.is_empty() {
                self.paragraphs
                    .push(vec![format!("{} takes these parameters:", self.name)]);
            } else {
                self.paragraphs.push(vec!["Parameters:".into()]);
            }
            self.paragraphs.push(list);
        }
        self
    }

    /// Set the `Deprecated:` paragraph.
    pub(crate) fn deprecated(mut self, msg: Option<&str>) -> Self {
        self.deprecated = msg.map(str::to_string);
        self
    }

    /// `true` when the comment has no paragraph yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.paragraphs.is_empty()
    }

    /// Write the comment at the writer's indentation.
    pub(crate) fn emit(self, w: &mut CodeWriter) {
        let mut paragraphs = self.paragraphs;
        if let Some(msg) = self.deprecated {
            paragraphs.push(vec![format!("Deprecated: {msg}")]);
        }
        for (i, p) in paragraphs.iter().enumerate() {
            if i > 0 {
                w.line("//");
            }
            for line in p {
                if line.is_empty() {
                    w.line("//");
                } else {
                    w.line(format!("// {line}"));
                }
            }
        }
    }
}

/// The first line of an IDL summary, made to start with `name`: kept when
/// it already does; a third-person verb ("Returns ...") follows the name in
/// lowercase; "Whether ..." becomes "reports whether ..." for a callable or
/// field; "The ...", "A ...", or a wh-word becomes "returns ..." for a
/// callable with a result and "is ..." for a type or field; anything else
/// follows the name after a colon.
fn lead(name: &str, first: &str, kind: Kind) -> String {
    let word = first.split_whitespace().next().unwrap_or_default();
    if word.trim_end_matches(['.', ',', ':', '\'']) == name {
        return first.to_string();
    }
    let lowered = lower_first(first);
    let noun = matches!(
        word,
        "The" | "A" | "An" | "How" | "What" | "When" | "Where" | "Which" | "Who" | "Why"
    );
    match kind {
        _ if third_person(word) => format!("{name} {lowered}"),
        Kind::Func | Kind::Proc | Kind::Value if word == "Whether" => {
            format!("{name} reports {lowered}")
        }
        Kind::Func if noun => format!("{name} returns {lowered}"),
        Kind::Value if noun => format!("{name} is {lowered}"),
        _ => format!("{name}: {first}"),
    }
}

/// `true` for a capitalized third-person verb that opens doc summaries
/// ("Returns", "Creates", "Reports"). A word that can also be a plural
/// noun ("Stores", "Lists", "Statistics") isn't one.
fn third_person(word: &str) -> bool {
    const VERBS: &[&str] = &[
        "Accepts",
        "Adds",
        "Advances",
        "Allocates",
        "Applies",
        "Builds",
        "Checks",
        "Clears",
        "Closes",
        "Computes",
        "Converts",
        "Copies",
        "Creates",
        "Decodes",
        "Deletes",
        "Describes",
        "Determines",
        "Divides",
        "Echoes",
        "Emits",
        "Encodes",
        "Fetches",
        "Finds",
        "Fires",
        "Formats",
        "Frees",
        "Gets",
        "Gives",
        "Greets",
        "Identifies",
        "Indicates",
        "Installs",
        "Loads",
        "Makes",
        "Merges",
        "Notifies",
        "Opens",
        "Parses",
        "Produces",
        "Raises",
        "Reads",
        "Receives",
        "Registers",
        "Releases",
        "Removes",
        "Renders",
        "Replaces",
        "Reports",
        "Represents",
        "Resets",
        "Resolves",
        "Returns",
        "Runs",
        "Sends",
        "Sets",
        "Starts",
        "Stops",
        "Subscribes",
        "Takes",
        "Tells",
        "Unsubscribes",
        "Updates",
        "Validates",
        "Waits",
        "Wraps",
        "Writes",
        "Yields",
    ];
    VERBS.contains(&word)
}

/// `s` with its first character lowercased, unless the first word is an
/// acronym or initialism (all capitals).
fn lower_first(s: &str) -> String {
    let word = s.split_whitespace().next().unwrap_or_default();
    if word.len() > 1 && word.chars().all(|c| !c.is_lowercase()) {
        return s.to_string();
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Generated text wrapped at [`WRAP`] columns.
fn wrap(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > WRAP {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(doc: GoDoc) -> String {
        let mut w = CodeWriter::tabs();
        doc.emit(&mut w);
        w.finish()
    }

    #[test]
    fn summaries_start_with_the_name() {
        assert_eq!(
            lead("Add", "Add two numbers.", Kind::Func),
            "Add two numbers."
        );
        assert_eq!(
            lead("Sum", "Returns the sum.", Kind::Func),
            "Sum returns the sum."
        );
        assert_eq!(
            lead("Path", "The path it was opened with.", Kind::Func),
            "Path returns the path it was opened with."
        );
        assert_eq!(
            lead("Entry", "A stored value.", Kind::Value),
            "Entry is a stored value."
        );
        assert_eq!(
            lead("Reason", "Why it failed.", Kind::Value),
            "Reason is why it failed."
        );
        assert_eq!(
            lead("Accepts", "Whether to accept.", Kind::Func),
            "Accepts reports whether to accept."
        );
        assert_eq!(
            lead("OnChange", "A change it accepted.", Kind::Proc),
            "OnChange: A change it accepted."
        );
        assert_eq!(
            lead("ChangePut", "An entry was stored.", Kind::Clause),
            "ChangePut: An entry was stored."
        );
        assert_eq!(lead("Open", "Open a store.", Kind::Func), "Open a store.");
        assert_eq!(
            lead("Connect", "Open a store.", Kind::Func),
            "Connect: Open a store."
        );
    }

    #[test]
    fn paragraphs_wrap_and_deprecation_comes_last() {
        let doc = GoDoc::new("Old", Some("Returns it.\n\nMore."), Kind::Func, None)
            .para("Extra.")
            .deprecated(Some("use New"));
        assert_eq!(
            render(doc),
            "// Old returns it.\n//\n// More.\n//\n// Extra.\n//\n// Deprecated: use New\n"
        );
        let bare = GoDoc::new("Wait", None, Kind::Func, None).para("Blocks until done.");
        assert_eq!(render(bare), "// Wait blocks until done.\n");
        let long = "word ".repeat(30);
        assert!(wrap(&long).iter().all(|l| l.len() <= WRAP));
    }
}
