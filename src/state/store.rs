//! `~/.colony/` store: atomic writes and locked read-modify-write.

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

impl StateDir {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `$HOME/.colony`, created with mode 700 if missing.
    pub fn home() -> Result<Self> {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        let root = PathBuf::from(home).join(".colony");
        create_root(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.root.join(file)
    }

    /// Missing file yields `T::default()`.
    pub fn read_json<T: DeserializeOwned + Default>(&self, file: &str) -> Result<T> {
        let path = self.path(file);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Exclusive lock on `<root>/.lock`, read, apply `f`, write atomically, unlock.
    /// If `f` fails nothing is written.
    pub fn update_json<T, R>(
        &self,
        file: &str,
        mode: u32,
        f: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        create_root(&self.root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(".lock"))
            .context("opening state lock")?;
        lock.lock().context("locking state")?;
        let result = self.update_locked(file, mode, f);
        let _ = lock.unlock();
        result
    }

    fn update_locked<T, R>(
        &self,
        file: &str,
        mode: u32,
        f: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        let mut value: T = self.read_json(file)?;
        let out = f(&mut value)?;
        let bytes = serde_json::to_vec_pretty(&value)?;
        write_atomic(&self.path(file), &bytes, mode)?;
        Ok(out)
    }
}

fn create_root(root: &Path) -> Result<()> {
    if !root.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .with_context(|| format!("creating {}", root.display()))?;
    }
    Ok(())
}

/// Writes `<file>.tmp.<pid>` with `mode`, fsyncs, then renames over the target.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("no file name in {}", path.display()))?
        .to_string_lossy()
        .into_owned();
    let tmp = path.with_file_name(format!("{file_name}.tmp.{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut f: File = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        // `mode` only applies on creation (and is masked by umask): set it explicitly.
        f.set_permissions(Permissions::from_mode(mode))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
            .with_context(|| format!("renaming onto {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type M = BTreeMap<String, u32>;

    fn store() -> (tempfile::TempDir, StateDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = StateDir::new(dir.path().to_path_buf());
        (dir, s)
    }

    #[test]
    fn read_missing_returns_default() {
        let (_d, s) = store();
        let m: M = s.read_json("x.json").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn update_roundtrips() {
        let (_d, s) = store();
        s.update_json("x.json", 0o644, |m: &mut M| {
            m.insert("a".to_string(), 1);
            Ok(())
        })
        .unwrap();
        let m: M = s.read_json("x.json").unwrap();
        assert_eq!(m.get("a"), Some(&1));
    }

    #[test]
    fn no_temp_file_left() {
        let (d, s) = store();
        s.update_json("x.json", 0o644, |m: &mut M| {
            m.insert("a".to_string(), 1);
            Ok(())
        })
        .unwrap();
        let mut names: Vec<String> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec![".lock".to_string(), "x.json".to_string()]);
    }

    #[test]
    fn mode_600_kept_across_writes() {
        let (_d, s) = store();
        for i in 0..2 {
            s.update_json("x.json", 0o600, |m: &mut M| {
                m.insert("a".to_string(), i);
                Ok(())
            })
            .unwrap();
            let mode = fs::metadata(s.path("x.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn concurrent_updates_lose_nothing() {
        let (_d, s) = store();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        s.update_json("x.json", 0o644, |m: &mut M| {
                            *m.entry("n".to_string()).or_insert(0) += 1;
                            Ok(())
                        })
                        .unwrap();
                    }
                });
            }
        });
        let m: M = s.read_json("x.json").unwrap();
        assert_eq!(m.get("n"), Some(&100));
    }
}
