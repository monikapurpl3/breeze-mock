//! Every example must load; every fixture in tests/fixtures/bad must fail,
//! with the message its second line promises and a real line number.
//!
//! When `xmllint` is installed, the same files also go through the XSD: the
//! examples must validate, and each bad fixture must be rejected or accepted
//! exactly as its `xsd:` marker says. "accepts" marks a rule XSD 1.0 cannot
//! express; if the schema ever starts catching one, the marker has to change,
//! so the documented gap can never silently drift from the real one.

use std::path::{Path, PathBuf};
use std::process::Command;

use breeze_mock::blueprint;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn xml_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "xml"))
        .collect();
    v.sort();
    v
}

/// `(expected message, xsd rejects?)` from `<!-- expect: ... | xsd: ... -->`.
fn marker(path: &Path) -> (String, bool) {
    let text = std::fs::read_to_string(path).unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with("<!-- expect: "))
        .unwrap_or_else(|| panic!("{} has no expect: marker", path.display()));
    let body = line
        .trim_start_matches("<!-- expect: ")
        .trim_end_matches(" -->");
    let (expect, xsd) = body
        .rsplit_once(" | xsd: ")
        .unwrap_or_else(|| panic!("{} has no xsd: marker", path.display()));
    let rejects = match xsd {
        "rejects" => true,
        "accepts" => false,
        other => panic!("{}: xsd marker {other:?}", path.display()),
    };
    (expect.replace("- -", "--"), rejects)
}

/// A library of models is only meaningful through a blueprint that includes it.
fn is_library(path: &Path) -> bool {
    std::fs::read_to_string(path).unwrap().contains("<library")
}

#[test]
fn every_example_loads() {
    let mut n = 0;
    for f in xml_files(&root().join("examples")) {
        if is_library(&f) {
            continue;
        }
        if let Err(e) = blueprint::load(&f) {
            panic!("example {} does not load: {e}", f.display());
        }
        n += 1;
    }
    assert!(n >= 2, "expected the examples to be found");
}

#[test]
fn every_bad_fixture_fails_with_its_message() {
    let files = xml_files(&root().join("tests/fixtures/bad"));
    assert!(files.len() >= 40, "only {} fixtures", files.len());
    let mut failures = Vec::new();
    for f in &files {
        let (expect, _) = marker(f);
        match blueprint::load(f) {
            Ok(_) => failures.push(format!(
                "{}: loaded, but must fail with {expect:?}",
                f.display()
            )),
            Err(e) => {
                if !e.to_string().contains(&expect) {
                    failures.push(format!(
                        "{}:\n    got:  {e}\n    want: {expect}",
                        f.display()
                    ));
                }
                if e.line == 0 {
                    failures.push(format!("{}: error has no line number: {e}", f.display()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn xmllint() -> Option<String> {
    let found = Command::new("xmllint").arg("--version").output().is_ok();
    found.then(|| "xmllint".to_string())
}

fn schema_accepts(lint: &str, file: &Path) -> bool {
    Command::new(lint)
        .args(["--noout", "--nonet", "--schema"])
        .arg(root().join("schema/blueprint.xsd"))
        .arg(file)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn the_schema_agrees_with_the_tool() {
    let Some(lint) = xmllint() else {
        eprintln!("xmllint is not installed; skipping the schema half (CI runs it)");
        return;
    };
    let mut failures = Vec::new();
    for f in xml_files(&root().join("examples")) {
        if !schema_accepts(&lint, &f) {
            failures.push(format!("{}: the schema rejects an example", f.display()));
        }
    }
    for f in xml_files(&root().join("tests/fixtures/bad")) {
        let (_, rejects) = marker(&f);
        let accepted = schema_accepts(&lint, &f);
        if rejects && accepted {
            failures.push(format!(
                "{}: marked xsd: rejects, but the schema accepts it",
                f.display()
            ));
        }
        if !rejects && !accepted {
            failures.push(format!(
                "{}: marked xsd: accepts, but the schema now rejects it -- update the marker",
                f.display()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
