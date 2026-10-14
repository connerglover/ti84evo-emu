//! Architecture boundary: the GUI reaches the core only through the machine adapter.

use std::path::{Path, PathBuf};

const FORBIDDEN: [&str; 4] = ["evo_core", "evo_hw", "evo_image", "armv8m"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_machine_adapter_touches_the_core() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let adapter = src.join("backend").join("machine.rs");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 10, "found only {files:?}");
    assert!(files.contains(&adapter), "{adapter:?} is missing");
    let mut offenders = Vec::new();
    for f in files.iter().filter(|f| **f != adapter) {
        let text = std::fs::read_to_string(f).unwrap();
        for name in FORBIDDEN {
            if text.contains(name) {
                offenders.push(format!("{} names {name}", f.display()));
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}
