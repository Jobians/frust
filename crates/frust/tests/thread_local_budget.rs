//! Thread-local budget: every `thread_local!` static in `crates/*/src` and
//! `plugins/*/src` is listed in a checked-in file, and each crate's count of
//! Android-linked statics stays under a budget.
//!
//! # Why
//!
//! On Android every Rust `thread_local!` static takes one of the 128
//! process-wide pthread keys (std has no native thread-local storage there on
//! stable), and running out aborts the app. Nothing else stops a contributor
//! adding a static, so this scan does.
//!
//! # Lists
//!
//! `tests/thread_local_budget/<crate>.txt` for the crates named in
//! [`LISTED_CRATES`], and `other.txt` for every other crate and plugin. A list
//! starts with `budget android <N>`; every other non-blank, non-`#` line is
//! `<repo-relative path> <STATIC_NAME> <class>` with class one of:
//!
//! - `android`: compiled into a default Android app, or under a feature the
//!   CLI turns on such as `perf-trace`
//! - `test`: inside `cfg(test)`, a test-support feature, or a host-only
//!   stand-in
//! - `apple`, `web`: compiled only for that target family
//! - `feature:<name>`: off by default
//! - `plugin`: a plugin outside the core graph; counts only in apps using it
//!
//! # Scan
//!
//! Comments and string literals are blanked first, so a `thread_local!` quoted
//! in a source-text constant or a doc line is not a static. Every
//! `thread_local!` block then yields one entry per top-level `static NAME`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const LISTED_CRATES: &[&str] = &[
    "frust",
    "frust-core",
    "frust-native-widgets",
    "frust-reactive",
    "frust-shell-common",
    "frust-widgets",
];

const OTHER: &str = "other";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

fn list_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/thread_local_budget")
}

/// Replaces comments and string/char literal contents with spaces, keeping
/// newlines and byte offsets.
fn blank_non_code(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut [u8], from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                let mut j = i + 2;
                while j < b.len() && depth > 0 {
                    if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                        depth += 1;
                        j += 2;
                    } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            b'r' | b'b' if is_raw_string_start(b, i) => {
                let mut j = i + 1;
                if b[j] == b'r' {
                    j += 1;
                }
                let hashes = b[j..].iter().take_while(|&&c| c == b'#').count();
                let body = j + hashes + 1;
                let mut close = vec![b'"'];
                close.extend(std::iter::repeat_n(b'#', hashes));
                let end = find(b, body, &close).map_or(b.len(), |p| p + close.len());
                blank(&mut out, body, end);
                i = end;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(b.len());
                blank(&mut out, i + 1, end);
                i = end;
            }
            b'\'' => {
                let end = char_literal_end(b, i);
                if let Some(end) = end {
                    blank(&mut out, i + 1, end - 1);
                    i = end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// `r"`, `r#"`, `br"`, `br#"` at `i`, not preceded by an identifier byte.
fn is_raw_string_start(b: &[u8], i: usize) -> bool {
    if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
        return false;
    }
    let mut j = i;
    if b[j] == b'b' {
        j += 1;
    }
    if b.get(j) != Some(&b'r') {
        return false;
    }
    j += 1;
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    b.get(j) == Some(&b'"')
}

/// The index just past a char literal opening at `i`, or `None` for a lifetime.
fn char_literal_end(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i + 1)? {
        b'\\' => {
            let mut j = i + 2;
            while j < b.len() && b[j] != b'\'' {
                j += 1;
            }
            Some(j + 1)
        }
        _ => {
            let width = std::str::from_utf8(&b[i + 1..])
                .ok()
                .and_then(|s| s.chars().next())
                .map_or(1, char::len_utf8);
            (b.get(i + 1 + width) == Some(&b'\'')).then_some(i + 2 + width)
        }
    }
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Names of every top-level `static` in every `thread_local!` block of `src`.
fn thread_local_statics(src: &str) -> Vec<String> {
    let code = blank_non_code(src);
    let b = code.as_bytes();
    let mut names = Vec::new();
    let mut at = 0;
    while let Some(p) = find(b, at, b"thread_local!") {
        at = p + "thread_local!".len();
        if p > 0 && is_ident_byte(b[p - 1]) {
            continue;
        }
        let mut j = at;
        while b.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let Some(&open) = b.get(j) else { break };
        let close = match open {
            b'{' => b'}',
            b'(' => b')',
            b'[' => b']',
            _ => continue,
        };
        let mut depth = 0usize;
        let mut k = j;
        let mut top = Vec::new();
        while k < b.len() {
            let c = b[k];
            if c == open || c == b'{' || c == b'(' || c == b'[' {
                depth += 1;
            } else if c == close || c == b'}' || c == b')' || c == b']' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if depth == 1
                && b[k..].starts_with(b"static")
                && !is_ident_byte(b[k - 1])
                && b[k - 1] != b'\''
                && b.get(k + 6).is_some_and(u8::is_ascii_whitespace)
            {
                let mut n = k + 6;
                while b.get(n).is_some_and(u8::is_ascii_whitespace) {
                    n += 1;
                }
                let start = n;
                while b.get(n).copied().is_some_and(is_ident_byte) {
                    n += 1;
                }
                top.push(code[start..n].to_string());
                k = n;
                continue;
            }
            k += 1;
        }
        names.extend(top);
        at = k.max(at);
    }
    names
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The list a source file belongs to, from its repo-relative path.
fn list_for(rel: &str) -> &'static str {
    let mut parts = rel.split('/');
    let crate_name = match (parts.next(), parts.next()) {
        (Some("crates"), Some(dir)) => dir.to_string(),
        (Some("plugins"), Some("native-widgets")) => "frust-native-widgets".to_string(),
        (Some("plugins"), Some(dir)) => format!("plugins/{dir}"),
        _ => String::new(),
    };
    LISTED_CRATES
        .iter()
        .find(|c| **c == crate_name)
        .copied()
        .unwrap_or(OTHER)
}

type Found = BTreeMap<(String, String), usize>;

fn scan_sources(root: &Path) -> BTreeMap<&'static str, Found> {
    let mut by_list: BTreeMap<&'static str, Found> = BTreeMap::new();
    for group in ["crates", "plugins"] {
        let Ok(members) = fs::read_dir(root.join(group)) else {
            continue;
        };
        for member in members.flatten() {
            let mut files = Vec::new();
            collect_rs(&member.path().join("src"), &mut files);
            for file in files {
                let rel = file
                    .strip_prefix(root)
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let src = fs::read_to_string(&file).expect("source reads");
                for name in thread_local_statics(&src) {
                    *by_list
                        .entry(list_for(&rel))
                        .or_default()
                        .entry((rel.clone(), name))
                        .or_default() += 1;
                }
            }
        }
    }
    by_list
}

struct Listed {
    budget: usize,
    android: usize,
    entries: Found,
}

fn parse_list(name: &str, text: &str, problems: &mut Vec<String>) -> Listed {
    let mut listed = Listed {
        budget: 0,
        android: 0,
        entries: Found::new(),
    };
    let mut saw_budget = false;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let at = format!("{name}.txt:{}", n + 1);
        match fields.as_slice() {
            ["budget", "android", count] if !saw_budget => match count.parse() {
                Ok(v) => {
                    listed.budget = v;
                    saw_budget = true;
                }
                Err(_) => problems.push(format!("{at}: budget `{count}` is not a number")),
            },
            [path, static_name, class] => {
                let class_ok = matches!(*class, "android" | "test" | "apple" | "web" | "plugin")
                    || class
                        .strip_prefix("feature:")
                        .is_some_and(|f| !f.is_empty());
                if !class_ok {
                    problems.push(format!("{at}: unknown class `{class}` for {static_name}"));
                }
                if *class == "android" {
                    listed.android += 1;
                }
                *listed
                    .entries
                    .entry((path.to_string(), static_name.to_string()))
                    .or_default() += 1;
            }
            _ => problems.push(format!("{at}: cannot parse `{line}`")),
        }
    }
    if !saw_budget {
        problems.push(format!("{name}.txt: missing the `budget android <N>` line"));
    }
    listed
}

const FIX: &str = "Put the state in the crate's per-thread struct instead of a new \
    thread_local! static (each one costs a process-wide pthread key on Android), or add the \
    line to the list and justify it in review.";

fn check(found: &BTreeMap<&'static str, Found>, lists: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    let empty = Found::new();
    let names = LISTED_CRATES.iter().copied().chain([OTHER]);
    for name in names {
        let Some(text) = lists.get(name) else {
            problems.push(format!("{name}.txt: list file is missing"));
            continue;
        };
        let listed = parse_list(name, text, &mut problems);
        let seen = found.get(name).unwrap_or(&empty);
        for ((path, stat), count) in seen {
            let have = listed
                .entries
                .get(&(path.clone(), stat.clone()))
                .copied()
                .unwrap_or(0);
            if have < *count {
                problems.push(format!(
                    "{path}: thread_local! static `{stat}` is not in {name}.txt. {FIX}"
                ));
            }
        }
        for ((path, stat), count) in &listed.entries {
            let have = seen
                .get(&(path.clone(), stat.clone()))
                .copied()
                .unwrap_or(0);
            if have < *count {
                problems.push(format!(
                    "{name}.txt lists `{stat}` in {path} but no such thread_local! static exists. \
                     Remove the line (and lower the budget if it was an android one)."
                ));
            }
        }
        if listed.android > listed.budget {
            problems.push(format!(
                "{name}.txt: {} android statics exceed the budget of {}. {FIX}",
                listed.android, listed.budget
            ));
        }
    }
    problems
}

fn read_lists() -> BTreeMap<String, String> {
    LISTED_CRATES
        .iter()
        .copied()
        .chain([OTHER])
        .filter_map(|name| {
            fs::read_to_string(list_dir().join(format!("{name}.txt")))
                .ok()
                .map(|text| (name.to_string(), text))
        })
        .collect()
}

#[test]
fn thread_local_statics_match_the_checked_in_lists() {
    let found = scan_sources(&repo_root());
    let problems = check(&found, &read_lists());
    assert!(
        problems.is_empty(),
        "thread-local budget violations:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn scanner_reads_every_static_in_a_block() {
    let src = "thread_local! {\n    /// doc\n    static A: Cell<u8> = const { Cell::new(0) };\n    \
               static B: RefCell<Vec<u8>> = RefCell::new(Vec::new());\n}\n";
    assert_eq!(thread_local_statics(src), ["A", "B"]);
}

#[test]
fn scanner_ignores_strings_comments_and_lookalikes() {
    let src = concat!(
        "const SRC: &str = \"thread_local! { static X: u8 = 0; }\";\n",
        "const RAW: &str = r#\"thread_local! { static Y: u8 = 0; }\"#;\n",
        "// thread_local! { static Z: u8 = 0; }\n",
        "/* thread_local! { static W: u8 = 0; } */\n",
        "my_thread_local! { static V: u8 = 0; }\n",
        "fn f<'a>(c: char) -> bool { c == '{' }\n",
        "thread_local! { static REAL: Cell<&'static str> = const { Cell::new(\"\") }; }\n",
    );
    assert_eq!(thread_local_statics(src), ["REAL"]);
}

#[test]
fn scanner_handles_function_local_and_parenthesised_blocks() {
    let src = "fn f() {\n    thread_local! {\n        static A: u8 = 0;\n    }\n}\n\
               thread_local!(static B: u8 = 0);\n";
    assert_eq!(thread_local_statics(src), ["A", "B"]);
}

#[test]
fn a_new_static_and_a_stale_line_are_reported() {
    let mut found: BTreeMap<&'static str, Found> = BTreeMap::new();
    found
        .entry("frust")
        .or_default()
        .insert(("crates/frust/src/x.rs".into(), "X".into()), 1);
    let mut lists: BTreeMap<String, String> = LISTED_CRATES
        .iter()
        .copied()
        .chain([OTHER])
        .map(|n| (n.to_string(), "budget android 9\n".to_string()))
        .collect();
    lists.insert(
        "frust-core".into(),
        "budget android 0\ncrates/frust-core/src/gone.rs GONE android\n".into(),
    );
    let problems = check(&found, &lists);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("`X` is not in frust.txt"))
    );
    assert!(problems.iter().any(|p| p.contains("lists `GONE`")));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("exceed the budget of 0"))
    );
}
