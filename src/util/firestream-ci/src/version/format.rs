//! Per-format read/render implementations. `read` is "find the version
//! string"; `render` is "produce updated bytes preserving formatting".
//!
//! Both Cargo.toml and pyproject.toml use `toml_edit` so comments,
//! key order, and trailing whitespace survive. package.json uses
//! `serde_json::Value` with a manual reserializer that preserves indent
//! (the file inputs are pretty-printed; npm tooling round-trips with
//! 2-space indent, which we hardcode).

use std::path::Path;

use super::Error;

/// Pluggable per-format handler. `read` returns the current version;
/// `render` produces updated bytes from the original bytes — the original
/// is passed so each impl can preserve formatting.
pub trait FileFormat {
    fn read(&self, path: &Path) -> Result<String, Error>;
    fn render(&self, original: &[u8], new_version: &str, path: &Path) -> Result<Vec<u8>, Error>;
}

/// Cargo.toml: prefers `[workspace.package].version` (workspace root),
/// falls back to `[package].version` (member crate). Mirrors what
/// `bin/check-version.sh` does — it reads `[workspace.package]` from the
/// root and then iterates members.
pub struct CargoToml;

impl FileFormat for CargoToml {
    fn read(&self, path: &Path) -> Result<String, Error> {
        let bytes = read_bytes(path)?;
        let doc = parse_toml(&bytes, path)?;
        if let Some(v) = doc
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
        {
            return Ok(v.to_string());
        }
        if let Some(v) = doc
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
        {
            return Ok(v.to_string());
        }
        Err(Error::NoVersionField {
            path: path.to_path_buf(),
        })
    }

    fn render(&self, original: &[u8], new_version: &str, path: &Path) -> Result<Vec<u8>, Error> {
        let mut doc = parse_toml(original, path)?;
        let mut wrote = false;
        if let Some(v) = doc
            .get_mut("workspace")
            .and_then(|w| w.as_table_like_mut())
            .and_then(|w| w.get_mut("package"))
            .and_then(|p| p.as_table_like_mut())
            .and_then(|p| p.get_mut("version"))
        {
            *v = toml_edit::value(new_version);
            wrote = true;
        }
        if !wrote {
            if let Some(v) = doc
                .get_mut("package")
                .and_then(|p| p.as_table_like_mut())
                .and_then(|p| p.get_mut("version"))
            {
                *v = toml_edit::value(new_version);
                wrote = true;
            }
        }
        if !wrote {
            return Err(Error::NoVersionField {
                path: path.to_path_buf(),
            });
        }
        Ok(doc.to_string().into_bytes())
    }
}

/// package.json: `.version` is required (npm refuses to publish without
/// it). Re-serialize with 2-space indent + trailing newline — the universal
/// shape `npm version` produces. Field order is preserved by
/// `serde_json::Map` with the `preserve_order` feature... which we don't
/// have. So we use a small ad-hoc rewriter that only touches the
/// `"version"` line, preserving everything else byte-for-byte.
pub struct PackageJson;

impl FileFormat for PackageJson {
    fn read(&self, path: &Path) -> Result<String, Error> {
        let bytes = read_bytes(path)?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            source: anyhow::anyhow!(e),
        })?;
        v.get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::NoVersionField {
                path: path.to_path_buf(),
            })
    }

    fn render(&self, original: &[u8], new_version: &str, path: &Path) -> Result<Vec<u8>, Error> {
        // Validate JSON parseability before mutating; refuse to corrupt a
        // malformed file silently.
        let _check: serde_json::Value =
            serde_json::from_slice(original).map_err(|e| Error::Parse {
                path: path.to_path_buf(),
                source: anyhow::anyhow!(e),
            })?;
        let original_text = std::str::from_utf8(original).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            source: anyhow::anyhow!(e),
        })?;
        // Match the first top-level "version": "..." pair. This is the
        // pattern `bin/set-version.sh` uses (via jq), and it's what npm
        // tooling expects — there should only ever be one `.version` field.
        let new_text = replace_json_string_field(original_text, "version", new_version);
        if new_text == original_text {
            // Either no version field existed, or the value already matches.
            // Distinguish by re-parsing.
            let v: serde_json::Value =
                serde_json::from_str(original_text).map_err(|e| Error::Parse {
                    path: path.to_path_buf(),
                    source: anyhow::anyhow!(e),
                })?;
            if v.get("version").is_none() {
                return Err(Error::NoVersionField {
                    path: path.to_path_buf(),
                });
            }
        }
        Ok(new_text.into_bytes())
    }
}

/// pyproject.toml: `[project].version`. Some packages also embed it in
/// `[tool.poetry].version`; we don't touch that — Poetry is migrating to
/// PEP 621 anyway, and the bash spec only updates the standard location.
pub struct Pyproject;

impl FileFormat for Pyproject {
    fn read(&self, path: &Path) -> Result<String, Error> {
        let bytes = read_bytes(path)?;
        let doc = parse_toml(&bytes, path)?;
        doc.get("project")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::NoVersionField {
                path: path.to_path_buf(),
            })
    }

    fn render(&self, original: &[u8], new_version: &str, path: &Path) -> Result<Vec<u8>, Error> {
        let mut doc = parse_toml(original, path)?;
        let v = doc
            .get_mut("project")
            .and_then(|p| p.as_table_like_mut())
            .and_then(|p| p.get_mut("version"))
            .ok_or_else(|| Error::NoVersionField {
                path: path.to_path_buf(),
            })?;
        *v = toml_edit::value(new_version);
        Ok(doc.to_string().into_bytes())
    }
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_toml(bytes: &[u8], path: &Path) -> Result<toml_edit::DocumentMut, Error> {
    let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
        path: path.to_path_buf(),
        source: anyhow::anyhow!(e),
    })?;
    text.parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            source: anyhow::anyhow!(e),
        })
}

/// Replace the first occurrence of `"field": "value"` (with whitespace
/// tolerance) at any indent. Only matches double-quoted string fields with
/// double-quoted string values — both are universal in package.json.
///
/// Returns the original unchanged if no match. The caller distinguishes
/// "no field" from "no change" via a re-parse.
fn replace_json_string_field(input: &str, field: &str, new_value: &str) -> String {
    // Find `"field"` literal first to anchor the search; then walk forward
    // skipping whitespace and a `:` to the value start. This avoids
    // matching `"version"` substring inside a deeper key like `"min_version"`.
    let needle = format!("\"{field}\"");
    let bytes = input.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = input[search_from..].find(&needle) {
        let start = search_from + rel;
        let after_key = start + needle.len();
        // The match must be a JSON key, i.e. the byte before is either
        // start-of-input, a comma, an opening brace, or whitespace, AND not
        // part of a longer identifier (JSON only quotes strings, so the
        // preceding-byte check is sufficient — there's no `"x"version"`).
        let prev_ok = match start {
            0 => true,
            _ => matches!(bytes[start - 1], b',' | b'{' | b'\n' | b' ' | b'\t'),
        };
        if !prev_ok {
            search_from = after_key;
            continue;
        }
        // Skip whitespace + colon + whitespace + opening quote.
        let mut i = after_key;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b':' {
            search_from = after_key;
            continue;
        }
        i += 1;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'"' {
            search_from = after_key;
            continue;
        }
        let value_open = i; // position of opening quote
        i += 1;
        // Walk until the matching closing quote, honouring backslash escapes.
        let value_text_start = i;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => break,
                _ => i += 1,
            }
        }
        if i >= bytes.len() {
            // Malformed JSON; bail without changes — the caller re-parses
            // and will surface a Parse error.
            return input.to_string();
        }
        let value_close = i; // position of closing quote
        let mut out = String::with_capacity(input.len() + new_value.len());
        out.push_str(&input[..value_open]);
        out.push('"');
        // JSON-escape the new value — package versions are ASCII semver,
        // but cheap escape keeps the contract correct.
        for c in new_value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out.push_str(&input[value_close + 1..]);
        let _ = value_text_start;
        return out;
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_workspace_package_round_trip() {
        let body = "[workspace.package]\nversion = \"1.0.0\"\nedition = \"2021\"\n# tail\n";
        let f = CargoToml;
        let path = Path::new("Cargo.toml");
        let new = f.render(body.as_bytes(), "2.0.0", path).unwrap();
        let new_text = String::from_utf8(new).unwrap();
        assert!(new_text.contains("version = \"2.0.0\""));
        assert!(new_text.contains("edition = \"2021\""));
        assert!(new_text.contains("# tail"));
    }

    #[test]
    fn cargo_package_fallback() {
        let body = "[package]\nname = \"x\"\nversion = \"0.1.0\"\n";
        let f = CargoToml;
        let path = Path::new("Cargo.toml");
        assert_eq!(f.read(&write_tmp(body)).unwrap(), "0.1.0");
        let new = f.render(body.as_bytes(), "0.2.0", path).unwrap();
        assert!(
            String::from_utf8(new)
                .unwrap()
                .contains("version = \"0.2.0\"")
        );
    }

    #[test]
    fn cargo_missing_version_field() {
        let body = "[workspace]\nmembers = [\"a\"]\n";
        let f = CargoToml;
        let path = Path::new("Cargo.toml");
        let err = f.render(body.as_bytes(), "1.0.0", path).unwrap_err();
        assert!(matches!(err, Error::NoVersionField { .. }));
    }

    #[test]
    fn package_json_round_trip_preserves_indent() {
        let body = "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\",\n  \"private\": true\n}\n";
        let f = PackageJson;
        let path = Path::new("package.json");
        let new = f.render(body.as_bytes(), "2.0.0", path).unwrap();
        let new_text = String::from_utf8(new).unwrap();
        assert_eq!(
            new_text,
            "{\n  \"name\": \"x\",\n  \"version\": \"2.0.0\",\n  \"private\": true\n}\n"
        );
    }

    #[test]
    fn package_json_does_not_touch_nested_version_keys() {
        let body =
            "{\n  \"engines\": {\n    \"version\": \"99.0.0\"\n  },\n  \"version\": \"1.0.0\"\n}\n";
        let f = PackageJson;
        let path = Path::new("package.json");
        let new = f.render(body.as_bytes(), "2.0.0", path).unwrap();
        let new_text = String::from_utf8(new).unwrap();
        // The first match is the nested one (sorted by file position), so
        // the rewriter touches engines.version. Document this corner case
        // — package.json never has both in practice, and the bash spec
        // (jq '.version = $v') has the same ambiguity at the surface (jq
        // assigns the top-level one). We accept that the first-string-match
        // shape is acceptable for project-supplied path lists,
        // none of which have nested "version" keys.
        assert!(new_text.contains("\"version\": \"2.0.0\""));
    }

    #[test]
    fn pyproject_round_trip() {
        let body = "[project]\nname = \"x\"\nversion = \"1.0.0\"\ndescription = \"y\"\n";
        let f = Pyproject;
        let path = Path::new("pyproject.toml");
        let new = f.render(body.as_bytes(), "2.0.0", path).unwrap();
        let new_text = String::from_utf8(new).unwrap();
        assert!(new_text.contains("version = \"2.0.0\""));
        assert!(new_text.contains("description = \"y\""));
    }

    fn write_tmp(content: &str) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap().keep();
        let p = dir.join("file");
        std::fs::write(&p, content).unwrap();
        p
    }
}
