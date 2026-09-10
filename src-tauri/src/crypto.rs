//! 上游 API Key 的静态加密。
//!
//! 设计对齐 FreeLLMAPI：Key 在写入 SQLite 前用 AES-256-GCM 加密，
//! 只在真正发起上游请求前于内存中解密，绝不以明文落盘或写日志。
//!
//! 主密钥（master key）来源优先级：
//!   1. 环境变量 LLMGW_MASTER_KEY（便于 CI / 多机复制）
//!   2. 本地密钥文件 master.key（Windows 由当前用户的 DPAPI 封装）
//!   3. 首次运行自动生成
//!
//! Windows 的 DPAPI 文件与当前用户绑定；复制到其他用户或设备时需要通过
//! `LLMGW_MASTER_KEY` 恢复同一把 AES 密钥后重新封装。非 Windows 平台保留
//! 原有的最小权限文件策略，避免在没有系统凭据服务时降低可用性。
//! 对 EFS 加密的旧文件，迁移会先同步一个 DPAPI 恢复信封，再原位改写并校验，
//! 以保留其 EFS 属性且能从进程中断中恢复。

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};
#[cfg(windows)]
use uuid::Uuid;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{LocalFree, HLOCAL},
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
    Storage::FileSystem::{
        GetFileAttributesW, ReplaceFileW, FILE_ATTRIBUTE_ENCRYPTED, REPLACEFILE_WRITE_THROUGH,
    },
};

use crate::error::GatewayError;

const MASTER_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const COMPETING_KEY_READ_ATTEMPTS: usize = 5;
const COMPETING_KEY_READ_DELAY: Duration = Duration::from_millis(10);
#[cfg(windows)]
const DPAPI_FILE_PREFIX: &str = "llmgw-dpapi-v1:";

static MASTER_KEY: OnceLock<[u8; MASTER_KEY_LEN]> = OnceLock::new();
static MASTER_KEY_INIT: Mutex<()> = Mutex::new(());

struct LoadedFileMasterKey {
    key: [u8; MASTER_KEY_LEN],
    #[cfg(windows)]
    needs_migration: bool,
}

#[cfg(windows)]
enum MigrationRecovery {
    Missing,
    Valid([u8; MASTER_KEY_LEN]),
    Invalid,
}

fn master_key_path() -> PathBuf {
    crate::config::app_data_dir().join("master.key")
}

fn key_from_bytes(bytes: &[u8], source: &str) -> Result<[u8; MASTER_KEY_LEN], String> {
    if bytes.len() != MASTER_KEY_LEN {
        return Err(format!(
            "{source} must decode to {MASTER_KEY_LEN} bytes, got {}",
            bytes.len()
        ));
    }

    let mut key = [0u8; MASTER_KEY_LEN];
    key.copy_from_slice(bytes);
    Ok(key)
}

fn decode_master_key(encoded: &str, source: &str) -> Result<[u8; MASTER_KEY_LEN], String> {
    let bytes = B64
        .decode(encoded.trim())
        .map_err(|error| format!("{source} is not valid base64: {error}"))?;
    key_from_bytes(&bytes, source)
}

#[cfg(windows)]
struct LocalDataBlob {
    blob: CRYPT_INTEGER_BLOB,
}

#[cfg(windows)]
impl LocalDataBlob {
    fn empty() -> Self {
        Self {
            blob: CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            },
        }
    }

    fn copy_bytes(&self, source: &str) -> Result<Vec<u8>, String> {
        if self.blob.cbData == 0 || self.blob.pbData.is_null() {
            return Err(format!("{source} returned an empty data blob"));
        }
        let len = usize::try_from(self.blob.cbData)
            .map_err(|_| format!("{source} returned an invalid data length"))?;
        // DPAPI allocates this buffer with LocalAlloc and guarantees it remains valid until
        // LocalFree in Drop. Copy it before the wrapper releases ownership.
        Ok(unsafe { std::slice::from_raw_parts(self.blob.pbData, len) }.to_vec())
    }
}

#[cfg(windows)]
impl Drop for LocalDataBlob {
    fn drop(&mut self) {
        if !self.blob.pbData.is_null() {
            // LocalFree returns null on success; cleanup cannot usefully recover from failure.
            let _ = unsafe { LocalFree(self.blob.pbData.cast::<std::ffi::c_void>() as HLOCAL) };
        }
    }
}

#[cfg(windows)]
fn input_blob(bytes: &[u8], source: &str) -> Result<CRYPT_INTEGER_BLOB, String> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| format!("{source} is too large for Windows DPAPI"))?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

#[cfg(windows)]
fn protect_master_key(key: &[u8; MASTER_KEY_LEN]) -> Result<Vec<u8>, String> {
    let input = input_blob(key, "master key")?;
    let mut output = LocalDataBlob::empty();
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.blob,
        )
    };
    if ok == 0 {
        return Err(format!(
            "Windows DPAPI failed to protect master key: {}",
            io::Error::last_os_error()
        ));
    }
    output.copy_bytes("Windows DPAPI protect")
}

#[cfg(windows)]
fn unprotect_master_key(blob: &[u8]) -> Result<[u8; MASTER_KEY_LEN], String> {
    let input = input_blob(blob, "DPAPI master key blob")?;
    let mut output = LocalDataBlob::empty();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.blob,
        )
    };
    if ok == 0 {
        return Err(format!(
            "Windows DPAPI failed to unprotect master key: {}",
            io::Error::last_os_error()
        ));
    }
    let bytes = output.copy_bytes("Windows DPAPI unprotect")?;
    key_from_bytes(&bytes, "Windows DPAPI master key")
}

#[cfg(windows)]
fn encode_persisted_master_key(key: &[u8; MASTER_KEY_LEN]) -> Result<String, String> {
    Ok(format!(
        "{DPAPI_FILE_PREFIX}{}",
        B64.encode(protect_master_key(key)?)
    ))
}

#[cfg(not(windows))]
fn encode_persisted_master_key(key: &[u8; MASTER_KEY_LEN]) -> Result<String, String> {
    Ok(B64.encode(key))
}

#[cfg(windows)]
fn decode_persisted_master_key(encoded: &str) -> Result<LoadedFileMasterKey, String> {
    let encoded = encoded.trim();
    if let Some(protected) = encoded.strip_prefix(DPAPI_FILE_PREFIX) {
        let protected = B64
            .decode(protected)
            .map_err(|error| format!("DPAPI master key file is not valid base64: {error}"))?;
        return Ok(LoadedFileMasterKey {
            key: unprotect_master_key(&protected)?,
            needs_migration: false,
        });
    }

    // A successful legacy read is migrated before the key can be cached. A malformed DPAPI
    // envelope never reaches this branch and therefore cannot be replaced with a new key.
    Ok(LoadedFileMasterKey {
        key: decode_master_key(encoded, "master key file")?,
        needs_migration: true,
    })
}

#[cfg(not(windows))]
fn decode_persisted_master_key(encoded: &str) -> Result<LoadedFileMasterKey, String> {
    Ok(LoadedFileMasterKey {
        key: decode_master_key(encoded, "master key file")?,
    })
}

fn load_master_key_from_file(path: &Path) -> Result<Option<LoadedFileMasterKey>, String> {
    match fs::read_to_string(path) {
        Ok(encoded) => decode_persisted_master_key(&encoded).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to read master key file {}: {error}",
            path.display()
        )),
    }
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn source_uses_efs(path: &Path) -> Result<bool, String> {
    let path_wide = wide_path(path);
    let attributes = unsafe { GetFileAttributesW(path_wide.as_ptr()) };
    if attributes == u32::MAX {
        return Err(format!(
            "failed to inspect master key file attributes {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(attributes & FILE_ATTRIBUTE_ENCRYPTED != 0)
}

#[cfg(windows)]
fn migration_recovery_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("master key path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("master key path has no valid file name: {}", path.display()))?;
    Ok(parent.join(format!(".{file_name}.dpapi-recovery")))
}

#[cfg(windows)]
fn load_migration_recovery(path: &Path) -> Result<MigrationRecovery, String> {
    let recovery_path = migration_recovery_path(path)?;
    let encoded = match fs::read_to_string(&recovery_path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(MigrationRecovery::Missing);
        }
        // A non-UTF-8 recovery file is corrupt content, not an unreadable file. It can be
        // rebuilt only after the main file has independently decoded to a complete key.
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return Ok(MigrationRecovery::Invalid);
        }
        Err(error) => {
            return Err(format!(
                "failed to read migration recovery file {}: {error}",
                recovery_path.display()
            ));
        }
    };

    match decode_persisted_master_key(&encoded) {
        Ok(loaded) if !loaded.needs_migration => Ok(MigrationRecovery::Valid(loaded.key)),
        // This includes a truncated/empty file, malformed base64, a DPAPI blob that cannot be
        // unprotected, and an accidental legacy-format recovery file. None is trusted until the
        // main file independently proves which existing key must be preserved.
        Ok(_) | Err(_) => Ok(MigrationRecovery::Invalid),
    }
}

#[cfg(windows)]
fn ensure_migration_recovery(path: &Path, key: &[u8; MASTER_KEY_LEN]) -> Result<PathBuf, String> {
    let recovery_path = migration_recovery_path(path)?;
    match open_new_master_key(&recovery_path) {
        Ok(mut file) => {
            persist_master_key(&mut file, key, &recovery_path)?;
            Ok(recovery_path)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let Some(existing) = load_master_key_from_file(&recovery_path)? else {
                return Err(format!(
                    "migration recovery file disappeared: {}",
                    recovery_path.display()
                ));
            };
            if existing.needs_migration || existing.key != *key {
                return Err(format!(
                    "migration recovery file does not match master key: {}",
                    recovery_path.display()
                ));
            }
            Ok(recovery_path)
        }
        Err(error) => Err(format!(
            "failed to create migration recovery file {}: {error}",
            recovery_path.display()
        )),
    }
}

#[cfg(windows)]
fn rebuild_invalid_migration_recovery(
    path: &Path,
    key: &[u8; MASTER_KEY_LEN],
) -> Result<(), String> {
    let recovery_path = migration_recovery_path(path)?;
    // The caller has already fully decoded the still-intact main file to `key`. Do not mutate
    // that main file until this replacement envelope is synced and read back. If the process
    // stops while this write is in progress, the next startup repeats the same main-file
    // validation before attempting another repair.
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&recovery_path)
        .map_err(|error| {
            format!(
                "failed to rebuild migration recovery file {}: {error}",
                recovery_path.display()
            )
        })?;
    persist_master_key(&mut file, key, &recovery_path)?;
    drop(file);

    let Some(loaded) = load_master_key_from_file(&recovery_path)? else {
        return Err(format!(
            "migration recovery file disappeared after rebuild: {}",
            recovery_path.display()
        ));
    };
    if loaded.needs_migration || loaded.key != *key {
        return Err(format!(
            "migration recovery file verification failed after rebuild: {}",
            recovery_path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn recover_invalid_migration_recovery(path: &Path) -> Result<[u8; MASTER_KEY_LEN], String> {
    let Some(main) = load_master_key_from_file(path).map_err(|error| {
        format!(
            "migration recovery file is invalid and master key file cannot be verified {}: {error}",
            path.display()
        )
    })?
    else {
        return Err(format!(
            "migration recovery file is invalid and master key file is missing: {}",
            path.display()
        ));
    };

    // `main.key` is the only key accepted for self-healing. In particular, an invalid recovery
    // file never permits falling through to first-run random-key generation.
    rebuild_invalid_migration_recovery(path, &main.key)?;
    recover_pending_migration(path, &main.key)
}

#[cfg(windows)]
fn persist_master_key_in_place(path: &Path, key: &[u8; MASTER_KEY_LEN]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to open EFS master key for migration {}: {error}",
                path.display()
            )
        })?;
    persist_master_key(&mut file, key, path)?;
    drop(file);

    let Some(loaded) = load_master_key_from_file(path)? else {
        return Err(format!(
            "master key disappeared after migration: {}",
            path.display()
        ));
    };
    if loaded.needs_migration || loaded.key != *key {
        return Err(format!(
            "master key verification failed after migration: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn migrate_efs_master_key_in_place(path: &Path, key: &[u8; MASTER_KEY_LEN]) -> Result<(), String> {
    let recovery_path = ensure_migration_recovery(path, key)?;
    persist_master_key_in_place(path, key)?;
    // A stale DPAPI recovery envelope is safe, and a later startup cleans it after verifying
    // the main file. Do not turn a successful migration into a startup failure on cleanup.
    let _ = fs::remove_file(recovery_path);
    Ok(())
}

#[cfg(windows)]
fn recover_pending_migration(
    path: &Path,
    recovery_key: &[u8; MASTER_KEY_LEN],
) -> Result<[u8; MASTER_KEY_LEN], String> {
    match load_master_key_from_file(path) {
        Ok(Some(loaded)) if !loaded.needs_migration && loaded.key == *recovery_key => {
            let _ = fs::remove_file(migration_recovery_path(path)?);
            Ok(*recovery_key)
        }
        Ok(Some(loaded)) if loaded.needs_migration && loaded.key == *recovery_key => {
            migrate_efs_master_key_in_place(path, recovery_key)?;
            Ok(*recovery_key)
        }
        Ok(Some(_)) => Err(format!(
            "master key and migration recovery key do not match: {}",
            path.display()
        )),
        Ok(None) | Err(_) => {
            // The previous process may have stopped after truncating the EFS file. The recovery
            // envelope is DPAPI-protected and was synced before that mutation, so it is safe to
            // restore the original path in place.
            persist_master_key_in_place(path, recovery_key)?;
            let _ = fs::remove_file(migration_recovery_path(path)?);
            Ok(*recovery_key)
        }
    }
}

#[cfg(windows)]
fn replace_master_key_file(temp_path: &Path, path: &Path) -> Result<(), String> {
    let temp_wide = wide_path(temp_path);
    let path_wide = wide_path(path);
    // ReplaceFile provides an atomic same-volume replacement while preserving the original
    // file's metadata. EFS files take the recoverable in-place path before reaching here.
    let ok = unsafe {
        ReplaceFileW(
            path_wide.as_ptr(),
            temp_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "failed to atomically replace master key file {} from {}: {}",
            path.display(),
            temp_path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn persist_master_key(
    file: &mut fs::File,
    key: &[u8; MASTER_KEY_LEN],
    path: &Path,
) -> Result<(), String> {
    let encoded = encode_persisted_master_key(key)?;
    file.write_all(encoded.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("failed to persist master key {}: {error}", path.display()))
}

#[cfg(windows)]
fn migrate_legacy_master_key(path: &Path, key: &[u8; MASTER_KEY_LEN]) -> Result<(), String> {
    if source_uses_efs(path)? {
        return migrate_efs_master_key_in_place(path, key);
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("master key path has no parent: {}", path.display()))?;
    let temp_path = parent.join(format!(".master-key-{}.tmp", Uuid::new_v4()));
    let migration = (|| {
        let mut file = open_new_master_key(&temp_path).map_err(|error| {
            format!(
                "failed to create temporary DPAPI master key {}: {error}",
                temp_path.display()
            )
        })?;
        persist_master_key(&mut file, key, &temp_path)?;
        drop(file);
        replace_master_key_file(&temp_path, path)
    })();

    if let Err(error) = migration {
        let _ = fs::remove_file(&temp_path);
        // Another process may have migrated the same old key while this process was writing.
        // Accept that completed state only when it decrypts to the identical key.
        if let Ok(Some(current)) = load_master_key_from_file(path) {
            if !current.needs_migration && current.key == *key {
                return Ok(());
            }
        }
        return Err(error);
    }
    Ok(())
}

fn load_existing_master_key(path: &Path) -> Result<Option<[u8; MASTER_KEY_LEN]>, String> {
    #[cfg(windows)]
    match load_migration_recovery(path)? {
        MigrationRecovery::Missing => {}
        MigrationRecovery::Valid(recovery_key) => {
            return recover_pending_migration(path, &recovery_key).map(Some);
        }
        MigrationRecovery::Invalid => {
            return recover_invalid_migration_recovery(path).map(Some);
        }
    }

    let Some(loaded) = load_master_key_from_file(path)? else {
        return Ok(None);
    };

    #[cfg(windows)]
    if loaded.needs_migration {
        migrate_legacy_master_key(path, &loaded.key)?;
    }

    Ok(Some(loaded.key))
}

fn read_competing_master_key(path: &Path) -> Result<[u8; MASTER_KEY_LEN], String> {
    let mut last_error = "master key file was not visible".to_owned();

    for attempt in 0..COMPETING_KEY_READ_ATTEMPTS {
        match load_existing_master_key(path) {
            Ok(Some(key)) => return Ok(key),
            Ok(None) => last_error = "master key file was not visible".to_owned(),
            Err(error) => last_error = error,
        }

        if attempt + 1 < COMPETING_KEY_READ_ATTEMPTS {
            std::thread::sleep(COMPETING_KEY_READ_DELAY);
        }
    }

    Err(format!(
        "master key was created by another process but could not be read: {last_error}"
    ))
}

fn open_new_master_key(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn load_or_create_file_master_key(path: &Path) -> Result<[u8; MASTER_KEY_LEN], String> {
    if let Some(key) = load_existing_master_key(path)? {
        return Ok(key);
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("master key path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create master key directory {}: {error}",
            parent.display()
        )
    })?;

    let mut key = [0u8; MASTER_KEY_LEN];
    OsRng.fill_bytes(&mut key);

    match open_new_master_key(path) {
        Ok(mut file) => {
            // Never replace an existing key. A failed write intentionally leaves its file in
            // place so a later initialization cannot overwrite a potentially recoverable key.
            persist_master_key(&mut file, &key, path)?;
            Ok(key)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            read_competing_master_key(path)
        }
        Err(error) => Err(format!(
            "failed to create master key file {}: {error}",
            path.display()
        )),
    }
}

fn load_master_key_uncached() -> Result<[u8; MASTER_KEY_LEN], String> {
    match std::env::var("LLMGW_MASTER_KEY") {
        Ok(encoded) => decode_master_key(&encoded, "LLMGW_MASTER_KEY"),
        Err(std::env::VarError::NotPresent) => load_or_create_file_master_key(&master_key_path()),
        Err(error) => Err(format!("failed to read LLMGW_MASTER_KEY: {error}")),
    }
}

fn load_master_key() -> std::result::Result<[u8; MASTER_KEY_LEN], GatewayError> {
    if let Some(key) = MASTER_KEY.get() {
        return Ok(*key);
    }

    // OnceLock retains the successfully loaded key. The mutex permits retries after a
    // recoverable I/O failure while ensuring only one thread can perform first-run I/O.
    let _initializing = MASTER_KEY_INIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(key) = MASTER_KEY.get() {
        return Ok(*key);
    }

    let key = load_master_key_uncached().map_err(GatewayError::Crypto)?;
    if MASTER_KEY.set(key).is_ok() {
        return Ok(key);
    }

    MASTER_KEY
        .get()
        .copied()
        .ok_or_else(|| GatewayError::Crypto("master key cache initialization failed".into()))
}

fn cipher() -> std::result::Result<Aes256Gcm, GatewayError> {
    let key = load_master_key()?;
    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key)))
}

/// 加密，输出 base64(nonce || ciphertext)
pub fn encrypt(plaintext: &str) -> std::result::Result<String, GatewayError> {
    let c = cipher()?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ct = c
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|e| GatewayError::Crypto(e.to_string()))?;

    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(B64.encode(out))
}

pub fn decrypt(b64: &str) -> std::result::Result<String, GatewayError> {
    let c = cipher()?;
    let raw = B64
        .decode(b64.trim())
        .map_err(|e| GatewayError::Crypto(e.to_string()))?;
    if raw.len() <= NONCE_LEN {
        return Err(GatewayError::Crypto("ciphertext too short".into()));
    }
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    let pt = c
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|e| GatewayError::Crypto(e.to_string()))?;
    String::from_utf8(pt).map_err(|e| GatewayError::Crypto(e.to_string()))
}

/// 生成对外统一网关 Key
pub fn new_unified_key() -> String {
    let mut b = [0u8; 24];
    OsRng.fill_bytes(&mut b);
    format!("lgw-{}", B64.encode(b).replace(['+', '/', '='], ""))
}

/// 对外只暴露脱敏形式，UI 与日志统一调用
pub fn mask(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 8 {
        return "****".into();
    }
    let prefix: String = secret.chars().take(4).collect();
    let suffix: String = secret.chars().skip(n - 4).collect();
    format!("{prefix}****{suffix}")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::EncryptFileW;

    fn test_key(seed: u8) -> [u8; MASTER_KEY_LEN] {
        [seed; MASTER_KEY_LEN]
    }

    fn test_master_key_path() -> (PathBuf, PathBuf) {
        let directory = std::env::temp_dir().join(format!("llm-gateway-crypto-{}", Uuid::new_v4()));
        fs::create_dir(&directory).expect("应创建隔离的 DPAPI 测试目录");
        let path = directory.join("master.key");
        (directory, path)
    }

    #[test]
    fn dpapi_envelope_round_trips_without_plaintext_key() {
        let key = test_key(0x5a);
        let encoded = encode_persisted_master_key(&key).expect("DPAPI 应能封装主密钥");
        assert!(encoded.starts_with(DPAPI_FILE_PREFIX));
        assert_ne!(encoded, B64.encode(key));

        let loaded = decode_persisted_master_key(&encoded).expect("DPAPI 信封应能解封");
        assert_eq!(loaded.key, key);
        assert!(!loaded.needs_migration);
    }

    #[test]
    fn legacy_master_key_is_migrated_to_dpapi_atomically() {
        let (directory, path) = test_master_key_path();
        let key = test_key(0x2c);
        fs::write(&path, B64.encode(key)).expect("应写入旧格式测试主密钥");

        let loaded = load_or_create_file_master_key(&path).expect("旧格式主密钥应迁移");
        assert_eq!(loaded, key);
        let migrated = fs::read_to_string(&path).expect("应读回迁移后的主密钥");
        assert!(migrated.starts_with(DPAPI_FILE_PREFIX));
        assert!(!migrated.contains(&B64.encode(key)));

        let reread = load_or_create_file_master_key(&path).expect("迁移后主密钥应可重读");
        assert_eq!(reread, key);
        fs::remove_dir_all(&directory).expect("应清理隔离的 DPAPI 测试目录");
    }

    #[test]
    fn efs_legacy_master_key_preserves_its_encryption_metadata_during_migration() {
        let (directory, path) = test_master_key_path();
        let key = test_key(0x39);
        fs::write(&path, B64.encode(key)).expect("应写入旧格式 EFS 测试主密钥");
        let path_wide = wide_path(&path);
        if unsafe { EncryptFileW(path_wide.as_ptr()) } == 0 {
            // Some CI images disable EFS. The regular migration test still covers the
            // platform-independent path; EFS-specific behavior runs where the OS enables it.
            let _ = fs::remove_dir_all(&directory);
            return;
        }
        assert!(source_uses_efs(&path).expect("应读取 EFS 文件属性"));

        let loaded = load_or_create_file_master_key(&path).expect("EFS 旧主密钥应迁移");
        assert_eq!(loaded, key);
        assert!(source_uses_efs(&path).expect("迁移后应保留 EFS 属性"));
        assert!(fs::read_to_string(&path)
            .expect("应读回迁移后的 EFS 主密钥")
            .starts_with(DPAPI_FILE_PREFIX));
        fs::remove_dir_all(&directory).expect("应清理隔离的 EFS 测试目录");
    }

    #[test]
    fn interrupted_efs_migration_recovers_from_the_synced_dpapi_envelope() {
        let (directory, path) = test_master_key_path();
        let key = test_key(0x4e);
        fs::write(&path, B64.encode(key)).expect("应写入旧格式 EFS 恢复测试主密钥");
        let path_wide = wide_path(&path);
        if unsafe { EncryptFileW(path_wide.as_ptr()) } == 0 {
            let _ = fs::remove_dir_all(&directory);
            return;
        }

        let recovery_path =
            ensure_migration_recovery(&path, &key).expect("应先同步 DPAPI 恢复信封");
        fs::write(&path, "interrupted-write").expect("应模拟原位迁移中断");

        assert_eq!(
            load_existing_master_key(&path)
                .expect("应从 DPAPI 恢复信封恢复")
                .expect("恢复后主密钥应存在"),
            key
        );
        assert!(source_uses_efs(&path).expect("恢复后应保留 EFS 属性"));
        assert!(fs::read_to_string(&path)
            .expect("应读回恢复后的主密钥")
            .starts_with(DPAPI_FILE_PREFIX));
        assert!(!recovery_path.exists(), "恢复确认后应清理 DPAPI 恢复信封");
        fs::remove_dir_all(&directory).expect("应清理隔离的 EFS 恢复测试目录");
    }

    #[test]
    fn empty_or_partial_recovery_rebuilds_from_an_intact_legacy_master_key() {
        for invalid_recovery in ["", "llmgw-dpapi-v1:AAAA"] {
            let (directory, path) = test_master_key_path();
            let key = test_key(0x63);
            fs::write(&path, B64.encode(key)).expect("应写入旧格式测试主密钥");
            let recovery_path = migration_recovery_path(&path).expect("应生成恢复文件路径");
            fs::write(&recovery_path, invalid_recovery).expect("应模拟未完成的恢复信封");

            assert_eq!(
                load_or_create_file_master_key(&path)
                    .expect("完好的旧主密钥应能重建无效恢复信封并完成迁移"),
                key
            );
            assert!(fs::read_to_string(&path)
                .expect("应读回迁移后的主密钥")
                .starts_with(DPAPI_FILE_PREFIX));
            assert!(!recovery_path.exists(), "迁移确认后应清理重建的恢复信封");
            fs::remove_dir_all(&directory).expect("应清理隔离的恢复自愈测试目录");
        }
    }

    #[test]
    fn damaged_recovery_with_valid_dpapi_master_key_is_cleaned() {
        let (directory, path) = test_master_key_path();
        let key = test_key(0x71);
        let encoded = encode_persisted_master_key(&key).expect("应创建 DPAPI 主密钥");
        fs::write(&path, &encoded).expect("应写入 DPAPI 主密钥");
        let recovery_path = migration_recovery_path(&path).expect("应生成恢复文件路径");
        fs::write(&recovery_path, "llmgw-dpapi-v1:AAAA").expect("应写入损坏的恢复信封");

        assert_eq!(
            load_or_create_file_master_key(&path)
                .expect("有效 DPAPI 主密钥应能安全清理损坏恢复信封"),
            key
        );
        assert_eq!(
            fs::read_to_string(&path).expect("有效 DPAPI 主密钥必须保留"),
            encoded
        );
        assert!(!recovery_path.exists(), "确认主文件后应清理重建的恢复信封");
        fs::remove_dir_all(&directory).expect("应清理隔离的 DPAPI 恢复测试目录");
    }

    #[test]
    fn damaged_recovery_and_master_key_fail_closed_without_replacement() {
        let (directory, path) = test_master_key_path();
        let damaged_master_key = "not-a-valid-master-key";
        fs::write(&path, damaged_master_key).expect("应写入损坏的主密钥");
        let recovery_path = migration_recovery_path(&path).expect("应生成恢复文件路径");
        let damaged_recovery = "llmgw-dpapi-v1:AAAA";
        fs::write(&recovery_path, damaged_recovery).expect("应写入损坏的恢复信封");

        assert!(
            load_or_create_file_master_key(&path).is_err(),
            "恢复文件和主文件都无效时不得生成或写入新主密钥"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("损坏的主文件必须保留"),
            damaged_master_key
        );
        assert_eq!(
            fs::read_to_string(&recovery_path).expect("损坏的恢复文件必须保留"),
            damaged_recovery
        );
        fs::remove_dir_all(&directory).expect("应清理隔离的 fail-closed 测试目录");
    }

    #[test]
    fn damaged_dpapi_envelope_never_falls_back_to_a_new_key() {
        let (directory, path) = test_master_key_path();
        let encoded = encode_persisted_master_key(&test_key(0x7f)).expect("应创建 DPAPI 信封");
        let mut protected = B64
            .decode(
                encoded
                    .strip_prefix(DPAPI_FILE_PREFIX)
                    .expect("DPAPI 信封应有版本前缀"),
            )
            .expect("应解码 DPAPI 信封");
        protected[0] ^= 0x01;
        let damaged = format!("{DPAPI_FILE_PREFIX}{}", B64.encode(protected));
        fs::write(&path, &damaged).expect("应写入损坏的 DPAPI 测试主密钥");

        assert!(
            load_or_create_file_master_key(&path).is_err(),
            "损坏的 DPAPI 文件不得被替换为新随机主密钥"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("损坏文件仍应保留"),
            damaged
        );
        fs::remove_dir_all(&directory).expect("应清理隔离的 DPAPI 测试目录");
    }
}
