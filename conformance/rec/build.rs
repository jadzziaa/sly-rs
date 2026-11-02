//! Recompiles `conformance.elf` into `$OUT_DIR/rec.rs`.

use std::path::Path;

fn main() {
    let elf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../conformance.elf");
    println!("cargo::rerun-if-changed={}", elf.display());
    let bytes = std::fs::read(&elf).unwrap_or_else(|e| panic!("{}: {e}", elf.display()));
    let image = ps2_analysis::Image::from_elf(&bytes).unwrap_or_else(|e| panic!("{e}"));
    let found = ps2_analysis::discover(&image, &[]).unwrap_or_else(|e| panic!("{e}"));
    let functions: Vec<_> = found.functions.iter().collect();
    let source = ps2_codegen::module(&image, &found, &functions);
    let out = Path::new(&std::env::var("OUT_DIR").unwrap_or_else(|e| panic!("{e}"))).join("rec.rs");
    std::fs::write(&out, source).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
