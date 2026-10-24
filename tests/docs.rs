//! docs/blueprint.md against schema/blueprint.xsd: every element the schema
//! has is documented, with exactly the attributes the schema gives it, and
//! "required" exactly where the schema says `use="required"`.
//!
//! An element is documented by a heading naming it (`### \`<unit>\``), whose
//! section's attribute table has one row per attribute (`` | `key=` | ``), or
//! by a row of the traits table (`` | `<flaky>` | `rate=` (…) | ``).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const XS: &str = "http://www.w3.org/2001/XMLSchema";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Element name -> attribute name -> required, over every place the element
/// appears (`<identity>` in a model and in a unit, say).
type Schema = BTreeMap<String, BTreeMap<String, bool>>;

fn schema() -> Schema {
    let text = std::fs::read_to_string(root().join("schema/blueprint.xsd")).unwrap();
    let doc = roxmltree::Document::parse(&text).unwrap();
    let xs = |n: roxmltree::Node, name: &str| {
        n.is_element() && n.tag_name().namespace() == Some(XS) && n.tag_name().name() == name
    };
    let named_types: BTreeMap<&str, roxmltree::Node> = doc
        .root_element()
        .children()
        .filter(|n| xs(*n, "complexType"))
        .map(|n| (n.attribute("name").unwrap(), n))
        .collect();
    // The attributes a complexType declares itself, not its nested elements'.
    let attrs = |ty: roxmltree::Node| -> Vec<(String, bool)> {
        ty.children()
            .filter(|n| xs(*n, "attribute"))
            .map(|a| (a.attribute("name").unwrap().to_string(), a.attribute("use") == Some("required")))
            .collect()
    };
    let mut out = Schema::new();
    for el in doc.descendants().filter(|n| xs(*n, "element")) {
        let Some(name) = el.attribute("name") else { continue };
        let ty = match el.attribute("type") {
            Some(t) => t.strip_prefix("bm:").and_then(|t| named_types.get(t).copied()),
            None => el.children().find(|n| xs(*n, "complexType")),
        };
        let entry = out.entry(name.to_string()).or_default();
        for (a, required) in ty.map(attrs).unwrap_or_default() {
            // Required anywhere is documented as required: a unit's
            // <epithets name=> is, a model's has none.
            *entry.entry(a).or_insert(false) |= required;
        }
    }
    out
}

/// The `` `<name>` `` or `` `attr=` `` tokens in a piece of Markdown.
fn tokens(s: &str, open: &str, close: &str) -> Vec<String> {
    s.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|t| t.strip_prefix(open).and_then(|t| t.strip_suffix(close)))
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .map(str::to_string)
        .collect()
}

fn cells(row: &str) -> Vec<&str> {
    let row = row.trim();
    row.trim_start_matches('|').trim_end_matches('|').split('|').map(str::trim).collect()
}

/// A cell that is one code span and nothing else.
fn sole_code(cell: &str) -> bool {
    cell.len() > 2 && cell.starts_with('`') && cell.ends_with('`') && cell.matches('`').count() == 2
}

#[derive(Default, Debug)]
struct Documented {
    /// Attribute -> documented as required.
    attrs: BTreeMap<String, bool>,
}

fn documented(text: &str) -> BTreeMap<String, Documented> {
    let mut out: BTreeMap<String, Documented> = BTreeMap::new();
    // The elements the current section is about: those its heading names.
    let mut current: Vec<String> = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if line.starts_with('#') {
            current = tokens(line, "<", ">");
            for e in &current {
                out.entry(e.clone()).or_default();
            }
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }
        let c = cells(line);
        let (Some(first), Some(second)) = (c.first(), c.get(1)) else { continue };
        if !sole_code(first) {
            continue;
        }
        // A trait: `<flaky>` | `rate=` (`1/30`), `x=` (required) | effect
        if let [t] = tokens(first, "<", ">").as_slice() {
            let d = out.entry(t.clone()).or_default();
            for part in second.split(", ").filter(|p| !p.is_empty()) {
                if let [a] = tokens(part, "", "=").as_slice() {
                    d.attrs.insert(a.clone(), part.contains("(required)"));
                }
            }
            continue;
        }
        // An attribute: `attr=` | value | default | ...
        if let ([a], Some(default)) = (tokens(first, "", "=").as_slice(), c.get(2)) {
            assert!(!current.is_empty(), "attribute row {line:?} is under a heading that names no element");
            for e in &current {
                out.entry(e.clone()).or_default().attrs.insert(a.clone(), default.starts_with("required"));
            }
        }
    }
    out
}

fn reference() -> String {
    std::fs::read_to_string(root().join("docs/blueprint.md")).unwrap()
}

fn disagreements(schema: &Schema, docs: &BTreeMap<String, Documented>) -> Vec<String> {
    let mut problems = Vec::new();
    for (el, attrs) in schema {
        let Some(d) = docs.get(el) else {
            problems.push(format!("<{el}> is in the schema but not documented"));
            continue;
        };
        let want: BTreeSet<&String> = attrs.keys().collect();
        let have: BTreeSet<&String> = d.attrs.keys().collect();
        for a in want.difference(&have) {
            problems.push(format!("<{el}> {a}= is in the schema but not documented"));
        }
        for a in have.difference(&want) {
            problems.push(format!("<{el}> {a}= is documented but not in the schema"));
        }
        for (a, required) in attrs {
            if let Some(doc_required) = d.attrs.get(a) {
                if doc_required != required {
                    let word = |r: bool| if r { "required" } else { "optional" };
                    problems.push(format!(
                        "<{el}> {a}= is {} in the schema but documented as {}",
                        word(*required),
                        word(*doc_required)
                    ));
                }
            }
        }
    }
    for el in docs.keys() {
        if !schema.contains_key(el) {
            problems.push(format!("<{el}> is documented but not in the schema"));
        }
    }
    problems
}

#[test]
fn the_blueprint_reference_is_the_schema() {
    let schema = schema();
    assert!(schema.len() > 40, "only {} elements read from the schema", schema.len());
    let problems = disagreements(&schema, &documented(&reference()));
    assert!(problems.is_empty(), "docs/blueprint.md and the schema disagree:\n  {}", problems.join("\n  "));
}

/// And the check can fail: a copy of the page with a row taken out, a
/// requirement changed and an element made up is caught on all three.
#[test]
fn the_reference_check_notices_what_is_wrong() {
    let text = reference();
    let (gone, changed) = ("| `code-ttl=` |", "| `segment=` | name | required |");
    for row in [gone, changed] {
        assert!(text.contains(row), "{row:?} is gone from the page; pick another row for this test");
    }
    let broken = text
        .lines()
        .filter(|l| !l.starts_with(gone))
        .map(|l| l.replace(changed, "| `segment=` | name | none |"))
        .chain(["### `<weather>`".to_string()])
        .collect::<Vec<_>>()
        .join("\n");
    let problems = disagreements(&schema(), &documented(&broken));
    for want in [
        "<pairing> code-ttl= is in the schema but not documented",
        "<unit> segment= is required in the schema but documented as optional",
        "<weather> is documented but not in the schema",
    ] {
        assert!(problems.iter().any(|p| p == want), "not caught: {want}\n  {}", problems.join("\n  "));
    }
}
