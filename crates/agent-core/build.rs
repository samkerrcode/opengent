//! Reads the repository-level `product.json` at build time and exposes its fields as
//! compile-time environment variables (see `config::product`).

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let product_path = manifest_dir.join("../../product.json");
    println!("cargo:rerun-if-changed={}", product_path.display());

    let raw = fs::read_to_string(&product_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", product_path.display()));

    for key in [
        "displayName",
        "slug",
        "dataDirName",
        "identifier",
        "version",
        "description",
    ] {
        let value = json_string_field(&raw, key)
            .unwrap_or_else(|| panic!("product.json is missing string field `{key}`"));
        println!(
            "cargo:rustc-env=PRODUCT_{}={value}",
            to_screaming_snake(key)
        );
    }
}

/// Minimal extraction of a top-level string field so the build script needs no JSON dependency.
fn json_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = raw.find(&needle)? + needle.len();
    let rest = raw[start..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

fn to_screaming_snake(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}
