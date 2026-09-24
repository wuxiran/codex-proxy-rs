//! 文件存储的公共件：路径分量净化、原子写、跨进程锁、权限。

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const LOCK_FILE: &str = "turn_state.lock";
const MAX_COMPONENT_LEN: usize = 128;

/// 把账号/模型 id 变成安全的目录或文件名：拒绝空值、`.`/`..`、控制字符，其余
/// 非 `[A-Za-z0-9._-]` 百分号编码，因此路径分隔符不可能出现，两个不同 id 也不会撞名。
pub fn sanitize_component(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "." || raw == ".." || raw.len() > MAX_COMPONENT_LEN {
        return None;
    }
    if raw.bytes().any(|b| b == 0 || b.is_ascii_control()) {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    Some(out)
}

pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    restrict_dir(dir);
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) {}

#[cfg(unix)]
fn restrict_file(file: &fs::File) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file(_file: &fs::File) {}

/// 临时文件名各不相同后重命名，读者永远看不到半写的文件。
pub fn atomic_write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    ensure_dir(dir)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let tmp = dir.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
    let written = (|| {
        let mut file = fs::File::create(&tmp)?;
        restrict_file(&file);
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join(name))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// 跨进程独占锁，随句柄释放。多实例共享同一目录时，读-改-写要在锁内完成。
pub struct LockGuard {
    _file: fs::File,
}

pub fn lock(dir: &Path) -> io::Result<LockGuard> {
    ensure_dir(dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    file.lock()?;
    Ok(LockGuard { _file: file })
}

pub fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        },
    )
}

pub fn json_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
                })
                .collect()
        },
    )
}
