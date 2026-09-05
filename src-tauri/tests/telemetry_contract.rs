//! Guards the telemetry contract by grepping the source. Two events shipped
//! for three releases with no emit site, and a third came from an unbounded
//! poller — both cheap to catch here, expensive to spot in review.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
const TELEMETRY_RS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/telemetry.rs");

fn rust_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new(SRC), &mut out);
    out
}

/// Variant names declared in the `TelemetryEvent` enum body.
fn declared_variants() -> BTreeSet<String> {
    let src = std::fs::read_to_string(TELEMETRY_RS).expect("read telemetry.rs");
    let start = src
        .find("pub enum TelemetryEvent {")
        .expect("TelemetryEvent enum");
    let body = &src[start..];
    // The enum ends at the first line that is exactly "}" at column 0.
    let end = body.find("\n}").expect("enum terminator");
    let body = &body[..end];

    let mut out = BTreeSet::new();
    for line in body.lines() {
        let t = line.trim();
        // Variants are the only lines starting with an uppercase letter.
        let Some(first) = t.chars().next() else {
            continue;
        };
        if !first.is_ascii_uppercase() {
            continue;
        }
        let name: String = t
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !name.is_empty() {
            out.insert(name);
        }
    }
    out
}

#[test]
fn every_variant_has_an_emit_site() {
    let variants = declared_variants();
    assert!(
        variants.len() > 20,
        "parsed too few variants — parser broke"
    );

    let mut sources = String::new();
    for f in rust_files() {
        // telemetry.rs itself only declares; an emit site must be elsewhere.
        if f.ends_with("telemetry.rs") {
            continue;
        }
        sources.push_str(&std::fs::read_to_string(&f).unwrap_or_default());
    }

    let dead: Vec<&String> = variants
        .iter()
        .filter(|v| !sources.contains(&format!("TelemetryEvent::{v}")))
        .collect();

    assert!(
        dead.is_empty(),
        "these TelemetryEvent variants are declared but never emitted: {dead:?}\n\
         Either wire them up or delete them — a variant with no emit site is a \
         metric everyone assumes exists and nobody is sending."
    );
}

#[test]
fn no_unbounded_poller_emits_telemetry() {
    // A send inside a seconds-long poll loop is the shape that produced 2,863
    // events. Aggregate and flush per window — see `flush_secure_input`.
    let offenders: Vec<String> = rust_files()
        .into_iter()
        .filter_map(|f| {
            let src = std::fs::read_to_string(&f).ok()?;
            let bad = src.contains("from_secs(2)") && src.contains("TelemetryEvent::");
            let aggregated = src.contains("SECURE_INPUT_WINDOW") || src.contains("drain()");
            (bad && !aggregated).then(|| f.display().to_string())
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "2-second poll loop emitting telemetry directly: {offenders:?}"
    );
}

#[test]
fn no_event_field_restates_an_aptabase_column() {
    // Aptabase already sends os/locale/app_version as columns on every row.
    // `AppStarted` used to spend its whole payload restating them.
    let src = std::fs::read_to_string(TELEMETRY_RS).expect("read telemetry.rs");
    let start = src.find("pub enum TelemetryEvent {").unwrap();
    let body = &src[start..src[start..].find("\n}").unwrap() + start];
    for banned in ["locale:", "os:", "app_version:", "os_version:"] {
        assert!(
            !body.contains(banned),
            "TelemetryEvent carries `{banned}`, which Aptabase already sends as a column"
        );
    }
}
