//! Resolves `!include <path>` tags in a manifest into one assembled document.
//!
//! An `!include` tag at any node is replaced by the parsed content of the
//! referenced file: relative paths resolve against the directory of the file
//! doing the including, fragments recurse, and the assembled document is
//! re-serialized so everything downstream (env expansion, parsing, hashing,
//! drift detection) sees a single plain manifest. Fragments are schema-less
//! replacement values; only the assembled root needs a `version:` header.
//! A manifest with no real include tags is returned byte-for-byte untouched.
//!
//! Failure is always a hard error carrying the include chain — a manifest
//! whose fragment is missing or broken must never load as a partial or empty
//! project fan-out.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde_yaml::Value;

use crate::{
    diag::{Diagnostic, SgCode},
    error::ProcessManagerError,
    runtime,
};

/// Maximum include nesting before resolution is refused.
const MAX_DEPTH: usize = 10;

/// Maximum cumulative bytes of included fragments before resolution is refused.
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// One file on the active include chain, identified by device/inode when the
/// platform provides it so hardlinked cycles are caught and a rename between
/// check and use cannot fake a fresh file.
struct Frame {
    path: PathBuf,
    id: Option<(u64, u64)>,
}

/// Tracks the active include chain, the cumulative size budget, and whether
/// any tag was actually spliced.
struct Resolver {
    stack: Vec<Frame>,
    bytes: usize,
    spliced: bool,
}

/// Replaces every `!include` tag in `content` with the referenced file's
/// parsed value and returns the assembled YAML. Content without any include
/// tags is returned untouched.
pub fn resolve_includes(
    content: &str,
    manifest: &Path,
) -> Result<String, ProcessManagerError> {
    if !content.contains("!include") {
        return Ok(content.to_string());
    }

    let root: Value =
        serde_yaml::from_str(content).map_err(ProcessManagerError::ConfigParseError)?;
    let mut resolver = Resolver {
        stack: vec![Frame {
            path: manifest.to_path_buf(),
            id: fs::metadata(manifest).ok().and_then(|meta| file_id(&meta)),
        }],
        bytes: content.len(),
        spliced: false,
    };
    let dir = manifest.parent().unwrap_or_else(|| Path::new("."));
    let resolved = resolver.resolve(root, dir)?;
    if !resolver.spliced {
        return Ok(content.to_string());
    }
    serde_yaml::to_string(&resolved).map_err(ProcessManagerError::ConfigParseError)
}

/// Stable identity for cycle detection, when the platform provides one.
#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

/// Stable identity for cycle detection, when the platform provides one.
#[cfg(not(unix))]
fn file_id(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

impl Resolver {
    fn resolve(
        &mut self,
        value: Value,
        dir: &Path,
    ) -> Result<Value, ProcessManagerError> {
        match value {
            Value::Tagged(tagged) if tagged.tag == "!include" => {
                let Value::String(raw) = &tagged.value else {
                    return Err(self.fail(
                        SgCode::IncludeUnresolved,
                        "an !include tag must be followed by a file path".to_string(),
                    ));
                };
                self.spliced = true;
                self.splice(&dir.join(raw))
            }
            Value::Tagged(mut tagged) => {
                tagged.value = self.resolve(tagged.value, dir)?;
                Ok(Value::Tagged(tagged))
            }
            Value::Mapping(map) => {
                let mut out = serde_yaml::Mapping::with_capacity(map.len());
                for (key, val) in map {
                    out.insert(key, self.resolve(val, dir)?);
                }
                Ok(Value::Mapping(out))
            }
            Value::Sequence(seq) => seq
                .into_iter()
                .map(|val| self.resolve(val, dir))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Sequence),
            other => Ok(other),
        }
    }

    fn splice(&mut self, path: &Path) -> Result<Value, ProcessManagerError> {
        if self.stack.len() > MAX_DEPTH {
            return Err(self.fail(
                SgCode::IncludeLimit,
                format!(
                    "{} nests includes deeper than {MAX_DEPTH} levels",
                    path.display()
                ),
            ));
        }

        self.stack.push(Frame {
            path: path.to_path_buf(),
            id: None,
        });
        let file = runtime::open_trusted_config(path).map_err(|err| {
            self.fail(
                SgCode::IncludeUnresolved,
                format!("cannot open included file {}: {err}", path.display()),
            )
        })?;
        let id = file.metadata().ok().and_then(|meta| file_id(&meta));
        let cycle =
            self.stack[..self.stack.len() - 1]
                .iter()
                .any(|frame| match (frame.id, id) {
                    (Some(seen), Some(current)) => seen == current,
                    _ => canon(&frame.path) == canon(path),
                });
        if cycle {
            return Err(self.fail(
                SgCode::IncludeCycle,
                format!("{} includes itself", path.display()),
            ));
        }
        if let Some(frame) = self.stack.last_mut() {
            frame.id = id;
        }

        let mut content = String::new();
        let budget = (MAX_BYTES.saturating_sub(self.bytes) + 1) as u64;
        file.take(budget)
            .read_to_string(&mut content)
            .map_err(|err| {
                self.fail(
                    SgCode::IncludeUnresolved,
                    format!("cannot read included file {}: {err}", path.display()),
                )
            })?;
        self.bytes += content.len();
        if self.bytes > MAX_BYTES {
            return Err(self.fail(
                SgCode::IncludeLimit,
                format!("included fragments exceed {MAX_BYTES} bytes"),
            ));
        }

        let value: Value = serde_yaml::from_str(&content).map_err(|err| {
            self.fail(
                SgCode::IncludeUnresolved,
                format!("included file {} is not valid YAML: {err}", path.display()),
            )
        })?;

        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let resolved = self.resolve(value, dir)?;
        self.stack.pop();
        Ok(resolved)
    }

    fn fail(&self, code: SgCode, title: String) -> ProcessManagerError {
        let chain = self
            .stack
            .iter()
            .map(|frame| frame.path.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        ProcessManagerError::Diag(Box::new(
            Diagnostic::error(code, title)
                .note(format!("include chain: {chain}"))
                .help_docs(),
        ))
    }
}

/// Best-effort canonical form for platforms without a file identity.
fn canon(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
