//! Explicit delegated login import. Only client-generated network context is read.
use crate::{benefits::ClaimPlatform, client_session::SessionCredential};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn resource_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|_| "无法定位应用资源")?;
    let directory = exe.parent().ok_or("无法定位应用资源")?;
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
    if cfg!(debug_assertions)
        && development
            .join("qoder-rewards/client-worker.cjs")
            .is_file()
    {
        return Ok(development);
    }
    let installed = directory.join("resources");
    if installed.join("qoder-rewards/client-worker.cjs").is_file() {
        return Ok(installed);
    }
    Err("当前安装缺少登录导入组件，请重新安装完整包".into())
}

pub fn decode_channel(output: &str, platform: ClaimPlatform) -> Result<SessionCredential, String> {
    let mut lines = output
        .lines()
        .filter_map(|line| line.strip_prefix("LLMGW_SESSION:"));
    let line = lines.next().ok_or("客户端未返回有效登录上下文")?;
    if lines.next().is_some() || line.len() > 44000 {
        return Err("客户端登录响应无效".into());
    }
    let decoded = STANDARD.decode(line).map_err(|_| "客户端登录响应无效")?;
    let raw = std::str::from_utf8(&decoded).map_err(|_| "客户端登录响应无效")?;
    SessionCredential::parse(raw, platform)
}

pub async fn capture(
    platform: ClaimPlatform,
    resources: &Path,
) -> Result<SessionCredential, String> {
    let output = run_client(platform, resources, "capture-session", &[]).await?;
    decode_channel(&output, platform)
}

async fn run_client(
    platform: ClaimPlatform,
    resources: &Path,
    mode: &str,
    extra: &[&str],
) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("客户端登录导入目前只支持 Windows".into());
    }
    let (folder, name, port) = match platform {
        ClaimPlatform::Qoder => ("Qoder", "Qoder.exe", "9341"),
        ClaimPlatform::QoderCn => ("Qoder CN", "Qoder CN.exe", "9342"),
        ClaimPlatform::TraeCn => ("Trae CN", "Trae CN.exe", "9343"),
        ClaimPlatform::Workbuddy | ClaimPlatform::WorkbuddyCn => {
            return Err("WorkBuddy 请关联本网关已保存的独立登录，无需启动客户端".into());
        }
    };
    let mut roots = vec![
        PathBuf::from("D:/Program Files"),
        PathBuf::from("C:/Program Files"),
    ];
    if let Some(local) = dirs::data_local_dir() {
        roots.push(local.join("Programs"));
    }
    let exe = roots
        .into_iter()
        .map(|root| root.join(folder).join(name))
        .find(|p| p.is_file())
        .ok_or("未找到所选平台客户端，请先安装并登录")?;
    let worker = resources.join(if platform == ClaimPlatform::TraeCn {
        "qoder-rewards/trae-worker.cjs"
    } else {
        "qoder-rewards/client-worker.cjs"
    });
    let script = resources.join("qoder-rewards/private-client.ps1");
    let node = resources.join("node/node.exe");
    if !worker.is_file() || !script.is_file() || !node.is_file() {
        return Err("登录导入组件缺失，请重新安装完整包".into());
    }
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").ok_or("无法定位系统工具")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let values = [
        script.to_string_lossy(),
        exe.to_string_lossy(),
        worker.to_string_lossy(),
        node.to_string_lossy(),
    ];
    let mut arguments = vec![
        "-NoProfile",
        "-NonInteractive",
        "-File",
        &values[0],
        "-Executable",
        &values[1],
        "-Port",
        port,
        "-Worker",
        &values[2],
        "-Node",
        &values[3],
        "-Mode",
        mode,
    ];
    arguments.extend_from_slice(extra);
    let (output, _) = crate::cli_tools::run_path(
        &powershell,
        &arguments,
        Duration::from_secs(if mode == "capture-session" { 100 } else { 180 }),
    )
    .await
    .map_err(|_| "复用登录失败：请关闭已运行的客户端后重试，并确认登录及网络有效")?;
    Ok(output)
}

pub async fn native_trae_claim(
    session: &SessionCredential,
    campaign_id: &str,
    verify_only: bool,
) -> Result<serde_json::Value, String> {
    session.validate(ClaimPlatform::TraeCn)?;
    let identity = session
        .snapshot
        .identity_hash
        .as_deref()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || matches!(c, b'a'..=b'f'))
        })
        .ok_or("Trae 原生账号绑定缺失，请重新复用客户端登录")?;
    let source = session
        .request_source
        .ok_or("Trae 客户端来源无效")?
        .to_string();
    let output = run_client(
        ClaimPlatform::TraeCn,
        &resource_root()?,
        if verify_only {
            "verify-native"
        } else {
            "claim-native"
        },
        &[
            "-ExpectedIdentityHash",
            identity,
            "-RequestSource",
            &source,
            "-CampaignId",
            campaign_id,
        ],
    )
    .await
    .map_err(|_| "Trae 原生签到接口不可用；请检查客户端登录和后台连接，未自动重发")?;
    decode_native_outcome(&output, campaign_id, session.request_source.unwrap())
}

pub fn decode_native_outcome(
    output: &str,
    campaign_id: &str,
    request_source: u8,
) -> Result<serde_json::Value, String> {
    if output.len() > 512 * 1024 {
        return Err("Trae 原生响应超过安全上限".into());
    }
    let mut lines = output
        .lines()
        .filter_map(|s| s.strip_prefix("LLMGW_TRAE_CLAIM:"));
    let raw = lines.next().ok_or("Trae 原生签到未返回可验证结果")?;
    if lines.next().is_some() {
        return Err("Trae 原生签到返回重复结果".into());
    }
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "Trae 原生签到响应格式无效")?;
    let replayed = value["replayed"]
        .as_bool()
        .ok_or("Trae 原生签到结果缺少重放标志")?;
    if value["identity_verified"] != true
        || value["request_source"] != request_source
        || value["native_http_status"] != 200
        || (!value["native_code"].is_null() && value["native_code"] != 0)
        || value["after_checked_in"] != true
        || value["before_checked_in"] != replayed
        || value["native_claim_calls"] != if replayed { 0 } else { 1 }
    {
        return Err("Trae 原生签到确认条件未满足".into());
    }
    crate::benefits::parse_claim(ClaimPlatform::TraeCn, campaign_id, &value)?;
    Ok(value)
}
