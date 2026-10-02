//! Offline administration. The process lock prevents a running server from
//! changing blobs while a consistent backup or restoration is in progress.
use crate::config::Config;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn lock(config: &Config) -> Result<File> {
    fs::create_dir_all(&config.data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.data_dir, fs::Permissions::from_mode(0o700))?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(config.data_dir.join(".process-lock"))?;
    file.try_lock_exclusive().map_err(|_| {
        io::Error::other("data directory is in use; stop Revaro before administration")
    })?;
    Ok(file)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    files: BTreeMap<String, String>,
}
fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn copy(source: &Path, target: &Path) -> Result<()> {
    if !fs::symlink_metadata(source)?.is_file() {
        return Err("backup contains a non-regular file".into());
    }
    private_dir(target.parent().ok_or("missing parent")?)?;
    fs::copy(source, target)?;
    let metadata = fs::metadata(source)?;
    File::options()
        .write(true)
        .open(target)?
        .set_times(std::fs::FileTimes::new().set_modified(metadata.modified()?))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
    }
    File::open(target)?.sync_all()?;
    Ok(())
}
fn walk(root: &Path, current: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &path, files)?;
        } else if kind.is_file() {
            let name = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-UTF8 backup path")?
                .to_owned();
            files.insert(name, hash(&path)?);
        } else {
            return Err("symlinks and special files are not supported in backups".into());
        }
    }
    Ok(())
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
pub fn backup(config: &Config, target: &Path) -> Result<()> {
    if target.exists() {
        return Err("backup destination must not exist".into());
    }
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize()?;
    let destination = parent.join(target.file_name().ok_or("invalid destination")?);
    let objects = config.objects_dir.canonicalize()?;
    let data = config.data_dir.canonicalize()?;
    if destination.starts_with(&objects) || destination.starts_with(&data) {
        return Err("backup destination must be outside the data and objects directories".into());
    }
    let stage = parent.join(format!(".revaro-backup-{}", crate::ids::new_id()));
    private_dir(&stage)?;
    let result = (|| -> Result<()> {
        let database = rusqlite::Connection::open_with_flags(
            config.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        database.execute(
            "VACUUM INTO ?1",
            [stage
                .join("revaro.db")
                .to_str()
                .ok_or("invalid backup path")?],
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(stage.join("revaro.db"), fs::Permissions::from_mode(0o600))?;
        }
        File::open(stage.join("revaro.db"))?.sync_all()?;
        let mut source = BTreeMap::new();
        walk(&objects, &objects, &mut source)?;
        private_dir(&stage.join("objects"))?;
        for name in source.keys() {
            copy(&objects.join(name), &stage.join("objects").join(name))?;
        }
        let mut files = BTreeMap::new();
        walk(&stage, &stage, &mut files)?;
        let bytes = serde_json::to_vec_pretty(&Manifest { format: 1, files })?;
        let manifest_path = stage.join("manifest.json");
        fs::write(&manifest_path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600))?;
        }
        File::open(&manifest_path)?.sync_all()?;
        sync_dir(&stage)?;
        fs::rename(&stage, &destination)?;
        sync_dir(&parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}
pub fn restore(config: &Config, source: &Path) -> Result<()> {
    if config.database_path().exists() {
        return Err("restore requires an empty data directory (no revaro.db)".into());
    }
    if config.objects_dir.exists() && fs::read_dir(&config.objects_dir)?.next().is_some() {
        return Err("restore requires an empty objects directory".into());
    }
    let source = source.canonicalize()?;
    let bytes = fs::read(source.join("manifest.json"))?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.format != 1 || !manifest.files.contains_key("revaro.db") {
        return Err("invalid backup format".into());
    }
    // Validate every relative path and checksum before modifying target directories.
    for (name, expected) in &manifest.files {
        let path = Path::new(name);
        if path
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
            || !(name == "revaro.db" || name.starts_with("objects/"))
        {
            return Err("invalid backup path".into());
        }
        let file = source.join(path);
        let canonical = file.canonicalize()?;
        if !canonical.starts_with(&source) || !fs::symlink_metadata(&file)?.is_file() {
            return Err("invalid backup object".into());
        }
        if hash(&file)? != *expected {
            return Err(format!("backup checksum mismatch: {name}").into());
        }
    }
    let database = rusqlite::Connection::open_with_flags(
        source.join("revaro.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let check: String = database.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err("backup database integrity check failed".into());
    }
    drop(database);
    private_dir(&config.objects_dir)?;
    let result = (|| -> Result<()> {
        for name in manifest.files.keys().filter(|n| n.starts_with("objects/")) {
            copy(&source.join(name), &config.objects_dir.join(&name[8..]))?;
        }
        copy(
            &source.join("revaro.db"),
            &config.data_dir.join(".restore-db"),
        )?;
        fs::rename(config.data_dir.join(".restore-db"), config.database_path())?;
        sync_dir(&config.objects_dir)?;
        sync_dir(&config.data_dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(config.data_dir.join(".restore-db"));
        let _ = fs::remove_file(config.database_path());
        let _ = fs::remove_dir_all(&config.objects_dir);
    }
    result
}

pub async fn command(config: &Config, args: &[String]) -> Result<()> {
    match args {
        [cmd, path] if cmd == "backup" => backup(config, Path::new(path)),
        [cmd, path] if cmd == "restore" => restore(config, Path::new(path)),
        [cmd] if cmd == "reset-admin" => {
            let db = crate::db::Database::open(config.database_path())?;
            let auth = crate::auth::AuthService::new(db);
            let path = config.data_dir.join(format!(
                "admin-recovery-credentials-{}",
                crate::ids::new_id()
            ));
            auth.reset_with_credentials_file(&config.admin_username, &path)
                .await?;
            tracing::info!(path=%path.display(),"administrator reset; all sessions and TOTP cleared; retrieve credentials from the private file");
            Ok(())
        }
        _ => Err(
            "usage: revaro [backup <new-directory> | restore <backup-directory> | reset-admin]"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(root: &Path) -> Config {
        Config::from_lookup(&|name| match name {
            "APP_DATA_DIR" => Some(root.join("data").display().to_string()),
            "APP_OBJECTS_DIR" => Some(root.join("objects").display().to_string()),
            _ => None,
        })
        .unwrap()
    }
    #[test]
    fn backup_restores_database_and_objects_and_rejects_tampering() {
        let root =
            std::env::temp_dir().join(format!("revaro-backup-test-{}", crate::ids::new_id()));
        let cfg = config(&root);
        let guard = lock(&cfg).unwrap();
        assert!(lock(&cfg).is_err());
        let db = crate::db::Database::open(cfg.database_path()).unwrap();
        drop(db);
        private_dir(&cfg.objects_dir.join("blobs")).unwrap();
        fs::write(cfg.objects_dir.join("blobs/example"), b"backup data").unwrap();
        let target = root.join("backup");
        backup(&cfg, &target).unwrap();
        assert!(backup(&cfg, &target).is_err());
        let restored = config(&root.join("restored"));
        let _restored_guard = lock(&restored).unwrap();
        restore(&restored, &target).unwrap();
        assert_eq!(
            fs::read(restored.objects_dir.join("blobs/example")).unwrap(),
            b"backup data"
        );
        let db = crate::db::Database::open(restored.database_path()).unwrap();
        drop(db);
        assert!(restore(&restored, &target).is_err());
        fs::write(target.join("objects/blobs/example"), b"corrupted").unwrap();
        let empty = config(&root.join("empty"));
        let _empty_guard = lock(&empty).unwrap();
        assert!(restore(&empty, &target).is_err());
        assert!(!empty.database_path().exists());
        assert!(!empty.objects_dir.exists());
        drop(guard);
        let _ = fs::remove_dir_all(root);
    }
}
