//! A single headless daily owner. No model requests, gateway listener or webview.
use crate::{benefit_config::BenefitsConfig, benefits::BenefitsCenter, config::AppConfig};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub async fn register(cfg: &BenefitsConfig, data: &Path, resources: &Path) -> Result<(), String> {
    cfg.validate().map_err(|_| "权益设置无效")?;
    if !cfg!(windows) || !data.is_absolute() {
        return Err("后台领取任务只支持 Windows 绝对数据目录".into());
    }
    let exe = std::env::current_exe().map_err(|_| "无法定位应用")?;
    let script = resources.join("qoder-rewards/register-task.ps1");
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").ok_or("无法定位系统工具")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let hour = cfg.auto_claim_after_hour.to_string();
    let minute = cfg.auto_claim_after_minute.to_string();
    let times = cfg
        .trigger_minutes()
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let enabled = if cfg.enabled && cfg.auto_claim && cfg.scheduler_mode == "windows_task" {
        "true"
    } else {
        "false"
    };
    let values = [
        script.to_string_lossy(),
        exe.to_string_lossy(),
        data.to_string_lossy(),
    ];
    crate::cli_tools::run_path(
        &powershell,
        &[
            "-NoProfile",
            "-NonInteractive",
            "-File",
            &values[0],
            "-Executable",
            &values[1],
            "-DataDirectory",
            &values[2],
            "-Hour",
            &hour,
            "-Minute",
            &minute,
            "-TriggerMinutes",
            &times,
            "-Timezone",
            &cfg.auto_claim_timezone,
            "-Enabled",
            enabled,
        ],
        Duration::from_secs(30),
    )
    .await
    .map_err(|_| "Windows 后台任务保存失败，未更新权益设置")?;
    Ok(())
}

pub fn load_existing(data: &Path) -> Result<AppConfig, String> {
    if !data.is_absolute()
        || !data.join("gateway.db").is_file()
        || !data.join("master.key").is_file()
    {
        return Err("必须指定已有完整数据目录；不会新建账户或密钥".into());
    }
    let raw = std::fs::read_to_string(data.join("config.toml")).map_err(|_| "无法读取既有配置")?;
    let cfg: AppConfig = toml::from_str(&raw).map_err(|_| "既有配置无效，未替换或修复")?;
    cfg.validate_local()
        .map_err(|_| "既有配置无效，未替换或修复")?;
    Ok(cfg)
}

pub async fn execute_once(
    cfg: &AppConfig,
    center: &BenefitsCenter,
    data: &Path,
) -> Result<serde_json::Value, String> {
    let now = chrono::Local::now();
    let settings = &cfg.benefits;
    settings.validate().map_err(|_| "权益设置无效")?;
    if !settings.enabled || !settings.auto_claim || settings.scheduler_mode != "windows_task" {
        return Ok(serde_json::json!({"status":"disabled_or_not_due","claims":[]}));
    }
    let day = if settings.auto_claim_timezone == "Asia/Shanghai" {
        now.with_timezone(&chrono::FixedOffset::east_opt(28800).unwrap())
            .format("%Y-%m-%d")
            .to_string()
    } else {
        now.format("%Y-%m-%d").to_string()
    };
    let directory = data.join("benefits-receipts");
    std::fs::create_dir_all(&directory).map_err(|_| "无法创建领取收据目录")?;
    let mut claims = Vec::new();
    let mut failed = false;
    for account in settings
        .accounts
        .iter()
        .filter(|a| a.enabled && settings.account_is_due(&a.id, now))
    {
        let path = directory.join(format!("{day}-{}.json", account.id));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let prior = std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
                if prior
                    .as_ref()
                    .and_then(|v| v.get("result"))
                    .and_then(|v| v.get("run"))
                    .and_then(|v| v.get("verdict"))
                    .and_then(|v| v.as_str())
                    .map_or(true, |v| v == "error")
                {
                    failed = true;
                }
                claims.push(
                    serde_json::json!({"account_id":account.id,"status":"receipt_exists_no_retry"}),
                );
                continue;
            }
            Err(_) => {
                failed = true;
                claims.push(serde_json::json!({"account_id":account.id,"status":"receipt_unavailable_no_request"}));
                continue;
            }
        };
        file.write_all(b"{\"status\":\"reserved_no_automatic_retry\"}")
            .and_then(|_| file.sync_all())
            .map_err(|_| "无法持久化领取收据，未发起请求")?;
        let result = center
            .claim_at(settings, cfg.http_proxy.as_deref(), &account.id, false, now)
            .await;
        let entry = match result {
            Ok(value) => {
                if value.run.verdict == crate::benefits::BenefitVerdict::Error {
                    failed = true;
                }
                serde_json::json!({"account_id":account.id,"result":value})
            }
            Err(_) => {
                failed = true;
                serde_json::json!({"account_id":account.id,"status":"error","message":"领取执行失败，请查看权益记录；今日不自动重试"})
            }
        };
        // The reservation remains even if the final receipt cannot be written.
        let bytes = serde_json::to_vec_pretty(&entry).map_err(|_| "无法编码领取收据")?;
        std::fs::write(&path, bytes).map_err(|_| "领取已执行，但收据写入失败；不会自动重试")?;
        claims.push(entry);
    }
    let summary = serde_json::json!({"day":day,"status":if failed{"partial_error"}else{"completed"},"claims":claims});
    std::fs::write(
        directory.join("last-run.json"),
        serde_json::to_vec_pretty(&summary).map_err(|_| "无法编码领取结果")?,
    )
    .map_err(|_| "无法保存领取结果")?;
    if failed {
        return Err("部分账号领取失败，请查看权益记录和 last-run.json".into());
    }
    Ok(summary)
}

pub async fn headless(
    action: &str,
    data: &Path,
    platform: Option<&str>,
) -> Result<serde_json::Value, String> {
    let mut cfg = load_existing(data)?;
    let db = crate::db::Db::connect(&cfg)
        .await
        .map_err(|_| "无法打开既有数据库")?;
    let center = BenefitsCenter::new(db.pool().clone());
    match action {
        "--qoder-login-start"
        | "--qoder-login-poll"
        | "--qoder-login-status"
        | "--qoder-login-cancel"
        | "--qoder-login-refresh"
        | "--qoder-job-status"
        | "--qoder-job-prepare"
        | "--qoder-job-refresh"
        | "--qoder-signing-prepare"
        | "--qoder-model-catalog"
        | "--qoder-job-cancel" => {
            let (region, id) = platform
                .and_then(|value| value.split_once(':'))
                .ok_or("Qoder 登录必须指定 global:账号或 cn:账号（轮询/取消使用会话 ID）")?;
            let region = match region {
                "global" => crate::qoder_auth::Region::Global,
                "cn" => crate::qoder_auth::Region::Cn,
                _ => return Err("Qoder 区域必须是 global 或 cn".into()),
            };
            let auth =
                crate::qoder_auth::QoderAuth::new(db.pool().clone(), cfg.http_proxy.as_deref())?;
            let value = match action {
                "--qoder-login-start" => serde_json::to_value(auth.start(region, id).await?),
                "--qoder-login-poll" => serde_json::to_value(auth.poll(region, id).await?),
                "--qoder-login-status" => {
                    serde_json::to_value(auth.session_status(region, id).await?)
                }
                "--qoder-login-refresh" => serde_json::to_value(auth.refresh(region, id).await?),
                "--qoder-job-status" => serde_json::to_value(auth.job_status(region, id).await?),
                "--qoder-job-prepare" => serde_json::to_value(auth.job_prepare(region, id).await?),
                "--qoder-job-refresh" => serde_json::to_value(auth.job_refresh(region, id).await?),
                "--qoder-signing-prepare" => {
                    serde_json::to_value(auth.model_signing_prepare(region, id).await?)
                }
                "--qoder-model-catalog" => {
                    serde_json::to_value(auth.model_catalog(region, id).await?)
                }
                "--qoder-job-cancel" => {
                    auth.job_cancel(region, id).await?;
                    Ok(serde_json::json!({"status":"cancelled", "region":region}))
                }
                _ => {
                    auth.cancel(region, id).await?;
                    Ok(serde_json::json!({"status":"cancelled", "region":region}))
                }
            };
            value.map_err(|_| "无法编码 Qoder 登录状态".into())
        }
        "--workbuddy-login-start"
        | "--workbuddy-login-poll"
        | "--workbuddy-login-status"
        | "--workbuddy-login-cancel"
        | "--workbuddy-login-refresh" => {
            let auth = crate::workbuddy_auth::WorkBuddyAuth::new(
                db.pool().clone(),
                cfg.http_proxy.as_deref(),
            )?;
            let id = platform
                .filter(|id| !id.starts_with("--"))
                .ok_or("缺少账号或登录会话 ID")?;
            let value = match action {
                "--workbuddy-login-start" => serde_json::to_value(auth.start(id).await?),
                "--workbuddy-login-poll" => serde_json::to_value(auth.poll(id).await?),
                "--workbuddy-login-status" => serde_json::to_value(auth.session_status(id).await?),
                "--workbuddy-login-refresh" => serde_json::to_value(auth.refresh(id).await?),
                _ => {
                    auth.cancel(id).await?;
                    Ok(serde_json::json!({"status":"cancelled"}))
                }
            };
            value.map_err(|_| "无法编码登录状态".into())
        }
        "--benefits-verify-native" => {
            if platform != Some("trae_cn") {
                return Err("原生只读验收仅支持 trae_cn".into());
            }
            let account = cfg
                .benefits
                .accounts
                .iter()
                .find(|a| a.platform == "trae_cn" && a.enabled)
                .ok_or("Trae 账号未启用")?;
            let session = center
                .session_for_native(&cfg.benefits, &account.id)
                .await?;
            let day = chrono::Utc::now()
                .with_timezone(&chrono::FixedOffset::east_opt(28800).unwrap())
                .format("daily_%Y-%m-%d")
                .to_string();
            let value = crate::benefit_login::native_trae_claim(&session, &day, true).await?;
            Ok(
                serde_json::json!({"status":"native_readonly_verified","platform":"trae_cn","claim_requests":value["native_claim_calls"],"identity_verified":value["identity_verified"],"claimed":value["after_checked_in"],"request_source":value["request_source"]}),
            )
        }
        "--setup-trae-benefits" => {
            cfg.benefits.enabled = true;
            cfg.benefits.auto_claim = true;
            cfg.benefits.auto_claim_timezone = "Asia/Shanghai".into();
            if !cfg.benefits.accounts.iter().any(|a| a.id == "trae_cn") {
                cfg.benefits
                    .accounts
                    .push(crate::benefit_config::BenefitAccount {
                        id: "trae_cn".into(),
                        platform: "trae_cn".into(),
                        label: "Trae 中国版".into(),
                        enabled: true,
                    });
            }
            let account = cfg
                .benefits
                .accounts
                .iter_mut()
                .find(|a| a.id == "trae_cn")
                .ok_or("Trae 账号未配置")?;
            if account.platform != "trae_cn" {
                return Err("账号 ID 已被其他平台占用".into());
            }
            account.enabled = true;
            cfg.benefits
                .account_claim_times
                .insert("trae_cn".into(), "00:05".into());
            cfg.benefits.validate().map_err(|_| "权益设置无效")?;
            cfg.save().map_err(|_| "无法保存权益设置")?;
            Ok(
                serde_json::json!({"status":"configured","platform":"trae_cn","time":"00:05","timezone":"Asia/Shanghai","default_time":format!("{:02}:{:02}",cfg.benefits.auto_claim_after_hour,cfg.benefits.auto_claim_after_minute)}),
            )
        }
        "--setup-benefits" => {
            cfg.benefits.enabled = true;
            cfg.benefits.auto_claim = true;
            cfg.benefits.auto_claim_after_hour = 12;
            cfg.benefits.auto_claim_after_minute = 5;
            cfg.benefits.auto_claim_timezone = "Asia/Shanghai".into();
            for platform in ["qoder", "qoder_cn"] {
                if !cfg.benefits.accounts.iter().any(|a| a.platform == platform) {
                    cfg.benefits
                        .accounts
                        .push(crate::benefit_config::BenefitAccount {
                            id: platform.into(),
                            platform: platform.into(),
                            label: if platform == "qoder" {
                                "Qoder 国际版"
                            } else {
                                "Qoder 中国版"
                            }
                            .into(),
                            enabled: true,
                        });
                }
            }
            cfg.save().map_err(|_| "无法保存权益设置")?;
            Ok(serde_json::json!({"status":"configured","time":"12:05","timezone":"Asia/Shanghai"}))
        }
        "--import-benefit-client" => {
            if !cfg.benefits.enabled {
                return Err("权益中心未启用".into());
            }
            let platform = crate::benefits::ClaimPlatform::parse(platform.ok_or("缺少平台")?)
                .ok_or("权益平台未适配")?;
            let account = cfg
                .benefits
                .accounts
                .iter()
                .find(|a| a.platform == platform.key() && a.enabled)
                .ok_or("权益账号不存在或未启用")?;
            let session =
                crate::benefit_login::capture(platform, &crate::benefit_login::resource_root()?)
                    .await?;
            center
                .set_session(&cfg.benefits, &account.id, &session)
                .await?;
            Ok(
                serde_json::json!({"status":"session_saved_encrypted","platform":platform.key(),"identity_verified":session.snapshot.identity_hash.is_some()}),
            )
        }
        "--install-benefits-task" => {
            let previous = cfg.benefits.clone();
            cfg.benefits.scheduler_mode = "windows_task".into();
            register(&cfg.benefits, data, &crate::benefit_login::resource_root()?).await?;
            if cfg.save().is_err() {
                let _ = register(&previous, data, &crate::benefit_login::resource_root()?).await;
                return Err("权益设置保存失败，已请求恢复原任务状态".into());
            }
            Ok(
                serde_json::json!({"status":"windows_task_installed","trigger_minutes":cfg.benefits.trigger_minutes(),"timezone":cfg.benefits.auto_claim_timezone}),
            )
        }
        "--benefits-once" => execute_once(&cfg, &center, data).await,
        "--benefits-check-login" => {
            let view = center
                .overview(&cfg.benefits, cfg.http_proxy.as_deref())
                .await?;
            Ok(
                serde_json::json!({"accounts":view.accounts.into_iter().map(|a|serde_json::json!({"platform":a.platform,"has_session":a.session_account.is_some(),"campaign_status":a.status.as_ref().map(|s|s.campaigns.iter().map(|c|&c.claim_status).collect::<Vec<_>>()),"session_error":a.session_error,"error":a.error})).collect::<Vec<_>>()}),
            )
        }
        _ => Err("后台操作未适配".into()),
    }
}

pub fn try_headless() -> Option<i32> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let action = args.first()?;
    if !matches!(
        action.as_str(),
        "--benefits-once"
            | "--qoder-login-start"
            | "--qoder-login-poll"
            | "--qoder-login-status"
            | "--qoder-login-cancel"
            | "--qoder-login-refresh"
            | "--qoder-job-status"
            | "--qoder-job-prepare"
            | "--qoder-job-refresh"
            | "--qoder-signing-prepare"
            | "--qoder-model-catalog"
            | "--qoder-job-cancel"
            | "--workbuddy-login-start"
            | "--workbuddy-login-poll"
            | "--workbuddy-login-status"
            | "--workbuddy-login-cancel"
            | "--workbuddy-login-refresh"
            | "--benefits-verify-native"
            | "--setup-trae-benefits"
            | "--import-benefit-client"
            | "--setup-benefits"
            | "--install-benefits-task"
            | "--benefits-check-login"
    ) {
        return None;
    }
    let run = || -> Result<serde_json::Value, String> {
        let index = args
            .iter()
            .position(|s| s == "--data-dir")
            .ok_or("后台操作必须指定已有数据目录")?;
        let data = PathBuf::from(args.get(index + 1).ok_or("缺少数据目录")?);
        if !data.is_absolute() {
            return Err("数据目录必须是绝对路径".into());
        }
        std::env::set_var("LLMGW_DATA_DIR", &data);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "无法启动后台执行")?;
        runtime.block_on(headless(action, &data, args.get(1).map(String::as_str)))
    };
    match run() {
        Ok(value) => {
            println!("{value}");
            Some(0)
        }
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}
