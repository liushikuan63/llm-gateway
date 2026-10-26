//! Default Windows LAN/WinINet proxy settings. This does not control TUN or other clients.
//! Snapshots may contain private PAC URLs and must only be persisted with encryption.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
#[path = "system/windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::WindowsSystemProxyBackend;

const DIRECT: u32 = 1;
const MANUAL: u32 = 2;
const PAC: u32 = 4;
const AUTO_DETECT: u32 = 8;
const ENABLED_FLAGS: u32 = MANUAL | PAC | AUTO_DETECT;
const MAX_SETTING_UNITS: usize = 32_768;

/// Opaque private configuration, deliberately without Debug or public raw fields.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemProxySnapshot {
    flags: u32,
    server: String,
    bypass: String,
    pac_url: String,
}

impl SystemProxySnapshot {
    fn validate(&self) -> Result<(), SystemProxyError> {
        for value in [&self.server, &self.bypass, &self.pac_url] {
            if value.contains('\0') || value.encode_utf16().count() > MAX_SETTING_UNITS {
                return Err(SystemProxyError::InvalidSnapshot);
            }
        }
        if self.flags & !(DIRECT | ENABLED_FLAGS) != 0 {
            return Err(SystemProxyError::InvalidSnapshot);
        }
        Ok(())
    }

    /// Whether any manual, PAC, or autodetection flag was set in this snapshot.
    pub fn is_enabled(&self) -> bool {
        self.flags & ENABLED_FLAGS != 0
    }

    /// Validate an active manual restore target against the current LLM listener.
    pub fn validate_for_gateway(&self, forbidden_port: u16) -> Result<(), SystemProxyError> {
        self.validate()?;
        if self.flags & MANUAL != 0 {
            validate_server(&self.server, forbidden_port)?;
        }
        Ok(())
    }

    fn usable_restore(&self) -> bool {
        self.is_enabled()
            && (self.flags & MANUAL == 0 || valid_server(&self.server))
            && (self.flags & PAC == 0 || !self.pac_url.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemProxyMode {
    Direct,
    Manual,
    Pac,
    AutoDetect,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemProxySource {
    External,
    Application,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemProxyStatus {
    pub supported: bool,
    pub enabled: bool,
    pub mode: SystemProxyMode,
    /// Presence only: no host, credentials, PAC URL, or bypass domains are exposed.
    pub manual_server: Option<String>,
    pub source: SystemProxySource,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SystemProxyError {
    #[error("此平台不支持 Windows 系统网络代理设置")]
    Unsupported,
    #[error("软件正在退出，不能再更改系统网络代理")]
    ShuttingDown,
    #[error("系统代理已被其他程序更改，请刷新状态后重试")]
    ExternalChange,
    #[error("没有可恢复的通用网络代理地址；请先配置有效代理地址")]
    MissingServer,
    #[error(
        "通用代理地址无效；需要不含凭据的 host:port 或 Windows 代理映射，不能使用 LLM API 地址"
    )]
    InvalidServer,
    #[error("系统代理备份格式无效")]
    InvalidSnapshot,
    #[error("Windows 系统代理操作失败（{operation}，错误码 {code}）")]
    Windows { operation: &'static str, code: u32 },
    #[error("无法读回确认系统代理，未覆盖可能的外部更改")]
    Unverified,
    #[error("系统代理变更失败，恢复原状态也失败；请检查 Windows 网络设置")]
    RollbackFailed,
}

/// Implementations must compare the current state immediately before writing.
/// WinINet offers no interprocess transaction; callers still verify the resulting state.
pub trait SystemProxyBackend: Send + Sync {
    fn snapshot(&self) -> Result<SystemProxySnapshot, SystemProxyError>;
    fn write_if_unchanged(
        &self,
        expected: &SystemProxySnapshot,
        desired: &SystemProxySnapshot,
    ) -> Result<(), SystemProxyError>;
}

#[derive(Default)]
struct ControlState {
    closing: bool,
    restore: Option<SystemProxySnapshot>,
    last_written: Option<SystemProxySnapshot>,
    error: Option<String>,
}

/// Synchronizes tray/UI mutations. Construction and status queries never write settings.
pub struct SystemProxyManager<B: SystemProxyBackend = WindowsSystemProxyBackend> {
    backend: B,
    state: Mutex<ControlState>,
}

impl Default for SystemProxyManager<WindowsSystemProxyBackend> {
    fn default() -> Self {
        Self::new(WindowsSystemProxyBackend)
    }
}

impl<B: SystemProxyBackend> SystemProxyManager<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            state: Mutex::new(ControlState::default()),
        }
    }

    pub fn snapshot(&self) -> Result<SystemProxySnapshot, SystemProxyError> {
        let _guard = self.state.lock();
        let snapshot = self.backend.snapshot()?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn status(&self) -> Result<SystemProxyStatus, SystemProxyError> {
        let mut state = self.state.lock();
        match self.backend.snapshot().and_then(|snapshot| {
            snapshot.validate()?;
            Ok(snapshot)
        }) {
            Ok(snapshot) => Ok(status_for(&snapshot, &state)),
            Err(error) => {
                state.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Validate every supplied record component before publishing any of it.
    /// This seeds memory only; never query/write the OS or change write policy.
    pub(super) fn seed_recovery_snapshots(
        &self,
        restore: SystemProxySnapshot,
        before: Option<SystemProxySnapshot>,
        owned: Option<SystemProxySnapshot>,
    ) -> Result<(), SystemProxyError> {
        restore.validate()?;
        if let Some(before) = &before {
            before.validate()?;
        }
        if let Some(owned) = &owned {
            owned.validate()?;
        }
        let mut state = self.state.lock();
        state.restore = Some(restore);
        // Preserve legacy semantics: absence of ownership never manufactures
        // ownership or clears already-known state in a reused manager.
        if let Some(owned) = owned {
            state.last_written = Some(owned);
        }
        Ok(())
    }

    /// Load a decrypted backup after restart. This does not write Windows settings.
    pub fn seed_restore_snapshot(
        &self,
        snapshot: SystemProxySnapshot,
    ) -> Result<(), SystemProxyError> {
        snapshot.validate()?;
        self.state.lock().restore = Some(snapshot);
        Ok(())
    }

    /// Last verified settings written by this manager, for encrypted ownership metadata.
    /// A matching live snapshot is still required before attributing settings to us.
    pub fn last_written_snapshot(&self) -> Option<SystemProxySnapshot> {
        self.state.lock().last_written.clone()
    }

    /// Load encrypted ownership metadata after restart; never writes or claims live state.
    pub fn seed_last_written_snapshot(
        &self,
        snapshot: SystemProxySnapshot,
    ) -> Result<(), SystemProxyError> {
        snapshot.validate()?;
        self.state.lock().last_written = Some(snapshot);
        Ok(())
    }

    /// Select the recovery record before encrypting it: preserve the original
    /// external settings while changing our own settings, capture new external
    /// settings after another program has replaced them.
    pub fn restore_snapshot_for_change(
        &self,
        before: &SystemProxySnapshot,
    ) -> Result<SystemProxySnapshot, SystemProxyError> {
        let state = self.state.lock();
        ensure_open(&state)?;
        before.validate()?;
        if self.backend.snapshot()? != *before {
            return Err(SystemProxyError::ExternalChange);
        }
        Ok(restoration_before_change(before, &state))
    }

    /// Recovery target for the requested operation, including a legacy loaded
    /// active backup restored from currently disabled settings.
    pub fn restore_snapshot_for_set_enabled(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
        before: &SystemProxySnapshot,
    ) -> Result<SystemProxySnapshot, SystemProxyError> {
        let state = self.state.lock();
        ensure_open(&state)?;
        before.validate()?;
        if self.backend.snapshot()? != *before {
            return Err(SystemProxyError::ExternalChange);
        }
        Ok(recovery_target_for_set(
            enabled,
            configured_server,
            before,
            &state,
        ))
    }

    /// Exact intended state for the durable pre-write ownership record. Planning
    /// only reads settings; after restart ownership still requires an exact live match.
    pub fn planned_snapshot_for_set_enabled(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
        before: &SystemProxySnapshot,
        forbidden_ports: &[u16],
    ) -> Result<SystemProxySnapshot, SystemProxyError> {
        let state = self.state.lock();
        ensure_open(&state)?;
        before.validate()?;
        if self.backend.snapshot()? != *before {
            return Err(SystemProxyError::ExternalChange);
        }
        plan_desired_snapshot(enabled, configured_server, before, &state, forbidden_ports)
    }

    pub fn set_enabled(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
    ) -> Result<SystemProxyStatus, SystemProxyError> {
        self.set_enabled_inner(enabled, configured_server, None, &[])
    }

    /// Use after durably encrypting `expected`; refuses a change since that backup.
    pub fn set_enabled_if_unchanged(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
        expected: &SystemProxySnapshot,
    ) -> Result<SystemProxyStatus, SystemProxyError> {
        self.set_enabled_inner(enabled, configured_server, Some(expected), &[])
    }

    /// Guard every effective manual target, including a cached/seeded restore address.
    pub fn set_enabled_if_unchanged_for_gateway(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
        expected: &SystemProxySnapshot,
        forbidden_port: u16,
    ) -> Result<SystemProxyStatus, SystemProxyError> {
        self.set_enabled_if_unchanged_for_gateway_ports(
            enabled,
            configured_server,
            expected,
            &[forbidden_port],
        )
    }

    /// Guard both configured and actually bound listener ports during a port change.
    pub fn set_enabled_if_unchanged_for_gateway_ports(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
        expected: &SystemProxySnapshot,
        forbidden_ports: &[u16],
    ) -> Result<SystemProxyStatus, SystemProxyError> {
        self.set_enabled_inner(enabled, configured_server, Some(expected), forbidden_ports)
    }

    fn set_enabled_inner(
        &self,
        enabled: bool,
        configured_server: Option<&str>,
        expected: Option<&SystemProxySnapshot>,
        forbidden_ports: &[u16],
    ) -> Result<SystemProxyStatus, SystemProxyError> {
        let mut state = self.state.lock();
        let result = (|| {
            ensure_open(&state)?;
            let before = self.backend.snapshot()?;
            before.validate()?;
            if expected.is_some_and(|expected| expected != &before) {
                return Err(SystemProxyError::ExternalChange);
            }
            if enabled && before.is_enabled() && configured_server.is_none() {
                state.error = None;
                return Ok(status_for(&before, &state));
            }
            if !enabled && !before.is_enabled() {
                state.error = None;
                return Ok(status_for(&before, &state));
            }
            let desired = plan_desired_snapshot(
                enabled,
                configured_server,
                &before,
                &state,
                forbidden_ports,
            )?;
            let restore_target =
                recovery_target_for_set(enabled, configured_server, &before, &state);
            self.apply(&before, &desired)?;
            state.restore = Some(restore_target);
            state.last_written = Some(desired.clone());
            state.error = None;
            Ok(status_for(&desired, &state))
        })();
        if let Err(error) = &result {
            state.error = Some(error.to_string());
        }
        result
    }

    /// Restore an encrypted backup after a higher-level persistence failure.
    /// `expected` must be the observed state produced by this application.
    pub fn restore_if_unchanged(
        &self,
        target: &SystemProxySnapshot,
        expected: &SystemProxySnapshot,
    ) -> Result<SystemProxyStatus, SystemProxyError> {
        let mut state = self.state.lock();
        let result = (|| {
            ensure_open(&state)?;
            target.validate()?;
            expected.validate()?;
            let current = self.backend.snapshot()?;
            if &current != expected {
                return Err(SystemProxyError::ExternalChange);
            }
            self.apply(expected, target)?;
            state.last_written = Some(target.clone());
            state.error = None;
            Ok(status_for(target, &state))
        })();
        if let Err(error) = &result {
            state.error = Some(error.to_string());
        }
        result
    }

    /// Close the controller before shutting down owned proxy kernels. Restore
    /// only our exact active settings, preserving manual off and external changes.
    /// The mutex also rejects activation that was waiting for durable SQL storage.
    pub fn shutdown_restore_owned(&self) -> Result<bool, SystemProxyError> {
        let mut state = self.state.lock();
        state.closing = true;
        let result: Result<bool, SystemProxyError> = (|| {
            let Some(owned) = state
                .last_written
                .as_ref()
                .filter(|s| s.is_enabled())
                .cloned()
            else {
                return Ok(false);
            };
            let current = self.backend.snapshot()?;
            current.validate()?;
            if current != owned {
                return Ok(false);
            }
            let target = state
                .restore
                .clone()
                .ok_or(SystemProxyError::InvalidSnapshot)?;
            target.validate()?;
            self.apply(&current, &target)?;
            state.last_written = None;
            state.error = None;
            Ok(true)
        })();
        if let Err(error) = &result {
            state.error = Some(error.to_string());
        }
        result
    }

    fn apply(
        &self,
        before: &SystemProxySnapshot,
        desired: &SystemProxySnapshot,
    ) -> Result<(), SystemProxyError> {
        desired.validate()?;
        if before == desired {
            return Ok(());
        }
        let written = self.backend.write_if_unchanged(before, desired);
        let current = self
            .backend
            .snapshot()
            .map_err(|_| SystemProxyError::Unverified)?;
        if written.is_ok() && &current == desired {
            return Ok(());
        }
        // A failed notification may follow a successful write. Roll back only our exact state.
        if &current == desired {
            self.backend
                .write_if_unchanged(desired, before)
                .map_err(|_| SystemProxyError::RollbackFailed)?;
            if self
                .backend
                .snapshot()
                .map_err(|_| SystemProxyError::RollbackFailed)?
                != *before
            {
                return Err(SystemProxyError::RollbackFailed);
            }
        } else if &current != before {
            return Err(SystemProxyError::ExternalChange);
        }
        written.and(Err(SystemProxyError::Unverified))
    }
}

fn ensure_open(state: &ControlState) -> Result<(), SystemProxyError> {
    if state.closing {
        return Err(SystemProxyError::ShuttingDown);
    }
    Ok(())
}

fn restoration_before_change(
    before: &SystemProxySnapshot,
    state: &ControlState,
) -> SystemProxySnapshot {
    if state.last_written.as_ref() == Some(before) {
        if let Some(original) = &state.restore {
            return original.clone();
        }
    }
    before.clone()
}

fn recovery_target_for_set(
    enabled: bool,
    configured_server: Option<&str>,
    before: &SystemProxySnapshot,
    state: &ControlState,
) -> SystemProxySnapshot {
    if enabled && configured_server.is_none() && !before.is_enabled() {
        if let Some(restore) = state
            .restore
            .as_ref()
            .filter(|snapshot| snapshot.usable_restore())
        {
            return restore.clone();
        }
    }
    restoration_before_change(before, state)
}

fn plan_desired_snapshot(
    enabled: bool,
    configured_server: Option<&str>,
    before: &SystemProxySnapshot,
    state: &ControlState,
    forbidden_ports: &[u16],
) -> Result<SystemProxySnapshot, SystemProxyError> {
    if (enabled && before.is_enabled() && configured_server.is_none())
        || (!enabled && !before.is_enabled())
    {
        return Ok(before.clone());
    }
    let mut desired = before.clone();
    if !enabled {
        // Retain all strings while disabling manual/PAC/autodetection flags.
        desired.flags = DIRECT;
    } else if let Some(server) = configured_server {
        let server = server.trim();
        if !valid_server(server) {
            return Err(SystemProxyError::InvalidServer);
        }
        desired.server = server.to_owned();
        desired.flags = DIRECT | MANUAL;
    } else if let Some(restore) = state
        .restore
        .as_ref()
        .filter(|snapshot| snapshot.usable_restore())
    {
        desired = restore.clone();
    } else if valid_server(&before.server) {
        desired.flags = DIRECT | MANUAL;
    } else {
        return Err(SystemProxyError::MissingServer);
    }
    if enabled {
        for port in forbidden_ports {
            desired.validate_for_gateway(*port)?;
        }
    }
    desired.validate()?;
    Ok(desired)
}

fn status_for(snapshot: &SystemProxySnapshot, state: &ControlState) -> SystemProxyStatus {
    let bits = snapshot.flags & ENABLED_FLAGS;
    let mode = match bits {
        0 => SystemProxyMode::Direct,
        MANUAL => SystemProxyMode::Manual,
        PAC => SystemProxyMode::Pac,
        AUTO_DETECT => SystemProxyMode::AutoDetect,
        _ => SystemProxyMode::Mixed,
    };
    SystemProxyStatus {
        supported: true,
        enabled: snapshot.is_enabled(),
        mode,
        manual_server: (!snapshot.server.is_empty()).then(|| "已配置（地址已隐藏）".to_owned()),
        source: if state.last_written.as_ref() == Some(snapshot) {
            SystemProxySource::Application
        } else {
            SystemProxySource::External
        },
        error: state.error.clone(),
    }
}

/// Accept WinINet's single host:port or protocol=host:port mapping, never userinfo/paths.
/// Reject the gateway's default API port even if written as a protocol mapping.
fn valid_server(server: &str) -> bool {
    if server.is_empty() || server.len() > 4096 || server.chars().any(|c| c.is_control()) {
        return false;
    }
    server.split(';').all(|entry| {
        let entry = entry.trim();
        let endpoint = match entry.split_once('=') {
            Some(("http" | "https" | "ftp" | "socks", endpoint)) => endpoint,
            Some(_) => return false,
            None => entry,
        };
        let endpoint = endpoint
            .strip_prefix("http://")
            .or_else(|| endpoint.strip_prefix("https://"))
            .unwrap_or(endpoint);
        let Some((host, port)) = endpoint.rsplit_once(':') else {
            return false;
        };
        if !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let Ok(port) = port.parse::<u16>() else {
            return false;
        };
        if port == 0 || port == 15721 || host.is_empty() {
            return false;
        }
        if host.starts_with('[') && host.ends_with(']') {
            return host[1..host.len() - 1]
                .parse::<std::net::Ipv6Addr>()
                .is_ok();
        }
        if host
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            return host.parse::<std::net::Ipv4Addr>().is_ok();
        }
        host.len() <= 253
            && host.is_ascii()
            && host.trim_end_matches('.').split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
    })
}

/// Validate a user configured general proxy and reject the current LLM listener port.
/// The returned error never contains the address or potential credentials.
pub fn validate_server(server: &str, forbidden_port: u16) -> Result<(), SystemProxyError> {
    let server = server.trim();
    if !valid_server(server)
        || server.split(';').any(|entry| {
            entry
                .trim()
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok())
                == Some(forbidden_port)
        })
    {
        return Err(SystemProxyError::InvalidServer);
    }
    Ok(())
}

#[cfg(not(windows))]
pub struct WindowsSystemProxyBackend;

#[cfg(not(windows))]
impl SystemProxyBackend for WindowsSystemProxyBackend {
    fn snapshot(&self) -> Result<SystemProxySnapshot, SystemProxyError> {
        Err(SystemProxyError::Unsupported)
    }
    fn write_if_unchanged(
        &self,
        _: &SystemProxySnapshot,
        _: &SystemProxySnapshot,
    ) -> Result<(), SystemProxyError> {
        Err(SystemProxyError::Unsupported)
    }
}

#[cfg(test)]
mod tests;
