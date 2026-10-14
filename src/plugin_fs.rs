//! Filesystem access for plugins, served on each plugin's worker thread.
use crate::plugin_api::{DirEntry, FileKind, FileStat, FsOperation};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, time::UNIX_EPOCH};

const MAX_ENTRIES: usize = 16_384;
const MAX_FILE: u64 = 16 * 1024 * 1024;

pub fn call(operation: FsOperation) -> Result<Value> {
    Ok(match operation {
        FsOperation::ReadDir { path } => json!(read_dir(absolute(&path)?)?),
        FsOperation::Stat { path } => json!(stat(absolute(&path)?)?),
        FsOperation::ReadFile { path } => {
            let path = absolute(&path)?;
            ensure!(
                fs::metadata(path)?.len() <= MAX_FILE,
                "file exceeds 16 MiB"
            );
            json!(fs::read_to_string(path).context("Cannot read file as UTF-8 text")?)
        }
        FsOperation::WriteFile { path, text } => {
            fs::write(absolute(&path)?, text)?;
            Value::Null
        }
        FsOperation::CreateDir { path } => {
            fs::create_dir_all(absolute(&path)?)?;
            Value::Null
        }
        FsOperation::Remove { path } => {
            let path = absolute(&path)?;
            // Never follow a symlink into the directory it points at.
            if fs::symlink_metadata(path)?.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
            Value::Null
        }
        FsOperation::Rename { from, to } => {
            fs::rename(absolute(&from)?, absolute(&to)?)?;
            Value::Null
        }
    })
}

// The worker's current directory is Slerm's, which means nothing to a plugin.
fn absolute(path: &str) -> Result<&Path> {
    let path = Path::new(path);
    ensure!(path.is_absolute(), "path must be absolute: {}", path.display());
    Ok(path)
}

fn kind(metadata: &fs::Metadata) -> FileKind {
    if metadata.is_dir() {
        FileKind::Directory
    } else if metadata.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn read_dir(path: &Path) -> Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("Cannot read {}", path.display()))? {
        ensure!(
            entries.len() < MAX_ENTRIES,
            "directory exceeds {MAX_ENTRIES} entries"
        );
        let entry = entry?;
        // JS strings cannot round-trip arbitrary bytes, so skip such names.
        let (Ok(name), Some(full)) = (
            entry.file_name().into_string(),
            entry.path().to_str().map(str::to_owned),
        ) else {
            continue;
        };
        let symlink = entry.file_type()?.is_symlink();
        // A dangling symlink has no target to describe.
        let kind = fs::metadata(entry.path()).map_or(FileKind::Other, |m| kind(&m));
        entries.push(DirEntry {
            name,
            path: full,
            kind,
            symlink,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn stat(path: &Path) -> Result<FileStat> {
    let symlink = fs::symlink_metadata(path)?.is_symlink();
    let metadata = fs::metadata(path)?;
    Ok(FileStat {
        kind: kind(&metadata),
        symlink,
        size: metadata.len(),
        modified: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_millis() as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(operation: Value) -> Result<Value> {
        call(serde_json::from_value(operation)?)
    }

    #[test]
    fn reads_writes_and_lists_absolute_paths() {
        let root = std::env::temp_dir().join(format!("slerm-plugin-fs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("sub").to_str().unwrap().to_owned();
        let file = root.join("note.txt").to_str().unwrap().to_owned();
        run(json!({"method": "createDir", "path": dir})).unwrap();
        run(json!({"method": "writeFile", "path": file, "text": "hello"})).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("sub"), root.join("link")).unwrap();
        assert_eq!(
            run(json!({"method": "readFile", "path": file})).unwrap(),
            "hello"
        );
        let stat = run(json!({"method": "stat", "path": file})).unwrap();
        assert_eq!(stat["kind"], "file");
        assert_eq!(stat["size"], 5);
        let entries = run(json!({"method": "readDir", "path": root})).unwrap();
        let names: Vec<_> = entries
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["name"].as_str().unwrap(), e["kind"].as_str().unwrap()))
            .collect();
        #[cfg(unix)]
        assert_eq!(
            names,
            [("link", "directory"), ("note.txt", "file"), ("sub", "directory")]
        );
        // Removing a symlink to a directory removes only the link.
        #[cfg(unix)]
        {
            run(json!({"method": "remove", "path": root.join("link")})).unwrap();
            assert!(root.join("sub").is_dir());
        }
        assert!(run(json!({"method": "readDir", "path": "relative"})).is_err());
        assert!(run(json!({"method": "readDir", "path": root.join("missing")})).is_err());
        run(json!({"method": "remove", "path": root})).unwrap();
        assert!(!root.exists());
    }
}
