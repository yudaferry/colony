//! `~/.colony/inbox/`: files and images received from the phone. The directory is 700, the
//! files 600, and files older than 7 days are deleted (at bot start and hourly).

use super::api::{FileRef, MAX_DOWNLOAD_BYTES, TelegramApi};
use super::reply_id::short_id;
use crate::ctx::Ctx;
use crate::time::file_stamp;
use anyhow::{Context, Result};
use chrono::Duration;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const INBOX_DIR: &str = "inbox";
pub const INBOX_MAX_AGE: Duration = Duration::days(7);
pub const TOO_BIG: &str = "\u{26a0}\u{fe0f} file too big (max 20 MB)";

/// Why a download did not happen.
#[derive(Debug)]
pub enum FetchError {
    TooBig,
    Failed(String),
}

pub fn inbox_path(ctx: &Ctx) -> PathBuf {
    ctx.state.path(INBOX_DIR)
}

/// The inbox directory, created with mode 700. A symlink in its place is refused.
fn ensure_inbox(ctx: &Ctx) -> Result<PathBuf> {
    let dir = inbox_path(ctx);
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => anyhow::bail!("{} is not a plain directory", dir.display()),
        Err(_) => DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating {}", dir.display()))?,
    }
    Ok(dir)
}

/// Extension from the Telegram file path, else the mime type, else `jpg` for a photo, else `bin`.
fn extension(file_path: &str, mime: Option<&str>, photo: bool) -> String {
    let from_path = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    if let Some(e) = from_path {
        return e;
    }
    let by_mime = match mime.unwrap_or("") {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "video/mp4" => "mp4",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        _ => "",
    };
    if !by_mime.is_empty() {
        by_mime.to_string()
    } else if photo {
        "jpg".to_string()
    } else {
        "bin".to_string()
    }
}

/// Downloads `file` into the inbox as `<YYYY-MM-DD_HHMMSS>_<shortid>.<ext>` and returns its path.
pub fn fetch(ctx: &Ctx, api: &dyn TelegramApi, file: &FileRef, photo: bool) -> Result<PathBuf, FetchError> {
    if file.file_size.is_some_and(|n| n > MAX_DOWNLOAD_BYTES) {
        return Err(FetchError::TooBig);
    }
    let info = api.get_file(&file.file_id).map_err(|e| {
        let m = format!("{e:#}");
        if m.to_lowercase().contains("too big") { FetchError::TooBig } else { FetchError::Failed(m) }
    })?;
    if info.file_size.is_some_and(|n| n > MAX_DOWNLOAD_BYTES) {
        return Err(FetchError::TooBig);
    }
    let Some(remote) = info.file_path else {
        return Err(FetchError::Failed("Telegram gave no file path".to_string()));
    };
    let dir = ensure_inbox(ctx).map_err(|e| FetchError::Failed(format!("{e:#}")))?;
    let ext = extension(&remote, file.mime_type.as_deref(), photo);
    let name = format!("{}_{}.{ext}", file_stamp(ctx.clock.now()), short_id());
    let dest = dir.join(name);
    match api.download_file(&remote, &dest, MAX_DOWNLOAD_BYTES) {
        Ok(()) => {
            // Whatever the client created, the file is private.
            let _ = fs::set_permissions(&dest, std::os::unix::fs::PermissionsExt::from_mode(0o600));
            Ok(dest)
        }
        Err(e) => {
            let _ = fs::remove_file(&dest);
            let m = format!("{e:#}");
            Err(if m.contains("too big") { FetchError::TooBig } else { FetchError::Failed(m) })
        }
    }
}

/// Deletes regular files in the inbox older than 7 days. Never follows symlinks and never
/// touches anything outside the inbox directory. Returns how many files were removed.
pub fn cleanup(ctx: &Ctx) -> Result<usize> {
    let dir = inbox_path(ctx);
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        _ => return Ok(0),
    }
    let now: SystemTime = ctx.clock.now().into();
    let limit = INBOX_MAX_AGE.to_std().unwrap_or_default();
    let mut removed = 0;
    for entry in fs::read_dir(&dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // symlink_metadata: a symlink is neither a file nor a dir here, so it is skipped.
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else { continue };
        if now.duration_since(mtime).is_ok_and(|age| age > limit) && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil::Harness;
    use crate::telegram::api::FakeTelegram;
    use std::os::unix::fs::PermissionsExt;

    fn age(h: &Harness, path: &Path, days: i64) {
        let t: SystemTime = (h.clock.0 - Duration::days(days)).into();
        fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn extension_rules() {
        assert_eq!(extension("photos/file_1.JPG", None, true), "jpg");
        assert_eq!(extension("documents/file_2", Some("application/pdf"), false), "pdf");
        assert_eq!(extension("photos/file_3", None, true), "jpg");
        assert_eq!(extension("x/file", None, false), "bin");
        assert_eq!(extension("x/file.../../../evil", Some("text/plain"), false), "txt");
    }

    #[test]
    fn fetch_saves_with_modes_and_name() {
        let h = Harness::herdr();
        let api = FakeTelegram::new();
        api.add_download("F1", "photos/file_9.jpg", b"jpegdata");
        let f = FileRef { file_id: "F1".into(), ..Default::default() };
        let p = fetch(&h.ctx(), &api, &f, true).unwrap();
        assert!(p.starts_with(h.state.path("inbox")));
        let name = p.file_name().unwrap().to_str().unwrap();
        // 2026-10-29 14:52:03 UTC = 21:52:03 WIB
        assert!(name.starts_with("2026-10-29_215203_") && name.ends_with(".jpg"), "{name}");
        assert_eq!(name.len(), "2026-10-29_215203_".len() + 6 + 4);
        assert_eq!(fs::read(&p).unwrap(), b"jpegdata");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(h.state.path("inbox")).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn too_big_by_message_or_by_telegram() {
        let h = Harness::herdr();
        let api = FakeTelegram::new();
        let big = FileRef { file_id: "B".into(), file_size: Some(21 * 1024 * 1024), ..Default::default() };
        assert!(matches!(fetch(&h.ctx(), &api, &big, false), Err(FetchError::TooBig)));
        assert!(api.file_requests.borrow().is_empty());
        let unknown = FileRef { file_id: "nope".into(), ..Default::default() };
        assert!(matches!(fetch(&h.ctx(), &api, &unknown, false), Err(FetchError::Failed(_))));
    }

    #[test]
    fn cleanup_removes_only_old_files_inside_inbox() {
        let h = Harness::herdr();
        let dir = ensure_inbox(&h.ctx()).unwrap();
        let old = dir.join("old.jpg");
        let fresh = dir.join("fresh.jpg");
        let edge = dir.join("six.jpg");
        for p in [&old, &fresh, &edge] {
            fs::write(p, b"x").unwrap();
        }
        age(&h, &old, 8);
        age(&h, &edge, 6);
        age(&h, &fresh, 0);
        // A file outside the inbox, old, reached through a symlink in the inbox, and an old subdir.
        let outside = h.state.path("outside.txt");
        fs::write(&outside, b"keep").unwrap();
        age(&h, &outside, 30);
        std::os::unix::fs::symlink(&outside, dir.join("link.txt")).unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/old.txt"), b"x").unwrap();
        age(&h, &dir.join("sub/old.txt"), 30);

        assert_eq!(cleanup(&h.ctx()).unwrap(), 1);
        assert!(!old.exists());
        assert!(fresh.exists() && edge.exists());
        assert!(outside.exists(), "file outside the inbox must survive");
        assert!(dir.join("link.txt").symlink_metadata().is_ok());
        assert!(dir.join("sub/old.txt").exists());
    }

    #[test]
    fn cleanup_without_inbox_or_with_symlinked_inbox_does_nothing() {
        let h = Harness::herdr();
        assert_eq!(cleanup(&h.ctx()).unwrap(), 0);
        let elsewhere = tempfile::tempdir().unwrap();
        let f = elsewhere.path().join("old.txt");
        fs::write(&f, b"x").unwrap();
        age(&h, &f, 30);
        std::os::unix::fs::symlink(elsewhere.path(), h.state.path("inbox")).unwrap();
        assert_eq!(cleanup(&h.ctx()).unwrap(), 0);
        assert!(f.exists());
    }
}
