//! Bounded private pipes to our bundled crypto implementation. No SDK or client.
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

// 4 MiB raw JSON may double when quoted inside the private pipe envelope.
// Reserve another bounded 1 MiB for context and envelope fields, matching worker.
const INPUT_LIMIT: usize = 9 * 1024 * 1024;
// A 4 MiB UTF-8 inference body expands to over 5 MiB after the wire codec.
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const ERROR_LIMIT: usize = 4096;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            if let Some(pid) = self.0.id() {
                crate::proc_util::kill_tree_blocking(pid);
            }
            let _ = self.0.start_kill();
        }
    }
}

async fn bounded(mut reader: impl AsyncRead + Unpin, maximum: usize) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let size = reader
            .read(&mut chunk)
            .await
            .map_err(|_| "Qoder签名管道读取失败")?;
        if size == 0 {
            return Ok(result);
        }
        if result.len().saturating_add(size) > maximum {
            return Err("Qoder签名管道输出过大".into());
        }
        result.extend_from_slice(&chunk[..size]);
    }
}

fn resources() -> Result<PathBuf, String> {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
    if cfg!(debug_assertions) && development.join("qoder-protocol/worker.cjs").is_file() {
        return Ok(development);
    }
    let exe = std::env::current_exe().map_err(|_| "无法定位Qoder签名组件")?;
    let installed = exe
        .parent()
        .ok_or("无法定位Qoder签名组件")?
        .join("resources");
    if installed.join("qoder-protocol/worker.cjs").is_file() {
        return Ok(installed);
    }
    Err("当前安装缺少Qoder独立签名组件，请安装完整包".into())
}

pub(super) async fn invoke(request: &Value) -> Result<Value, String> {
    let root = resources()?;
    run(
        &root.join("node/node.exe"),
        &root.join("qoder-protocol/worker.cjs"),
        request,
        Duration::from_secs(10),
    )
    .await
}

async fn run(
    node: &Path,
    worker: &Path,
    request: &Value,
    deadline: Duration,
) -> Result<Value, String> {
    run_limited(node, worker, request, deadline, INPUT_LIMIT, OUTPUT_LIMIT).await
}

pub(crate) async fn invoke_trae(request: &Value) -> Result<Value, String> {
    let root = resources().map_err(|_| "当前安装缺少独立认证组件")?;
    let worker = root.join("trae-protocol/worker.cjs");
    if !worker.is_file() {
        return Err("当前安装缺少Trae独立认证组件，请安装完整包".into());
    }
    run_limited(
        &root.join("node/node.exe"),
        &worker,
        request,
        Duration::from_secs(10),
        64 * 1024,
        16 * 1024,
    )
    .await
    .map_err(|value| value.replace("Qoder", "Trae"))
}

async fn run_limited(
    node: &Path,
    worker: &Path,
    request: &Value,
    deadline: Duration,
    input_limit: usize,
    output_limit: usize,
) -> Result<Value, String> {
    let input = serde_json::to_vec(request).map_err(|_| "Qoder签名输入编码失败")?;
    if input.len() > input_limit {
        return Err("Qoder签名输入过大".into());
    }
    let mut command = Command::new(node);
    command
        .arg(worker)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Clear Node preload/configuration variables, not just NODE_OPTIONS.
    // SystemRoot is an OS loader location, never an authentication input.
    #[cfg(windows)]
    {
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        command.creation_flags(0x08000000);
    }
    let mut child = OwnedChild(command.spawn().map_err(|_| "Qoder独立签名进程启动失败")?);
    let mut stdin = child.0.stdin.take().ok_or("Qoder签名输入管道缺失")?;
    let stdout = child.0.stdout.take().ok_or("Qoder签名输出管道缺失")?;
    let stderr = child.0.stderr.take().ok_or("Qoder签名错误管道缺失")?;
    let exchange = async {
        let write = async {
            stdin
                .write_all(&input)
                .await
                .map_err(|_| "Qoder签名输入发送失败")?;
            stdin
                .shutdown()
                .await
                .map_err(|_| "Qoder签名输入关闭失败")?;
            drop(stdin);
            Ok::<_, String>(())
        };
        let wait = async {
            child
                .0
                .wait()
                .await
                .map_err(|_| "Qoder签名进程等待失败".to_string())
        };
        let (_, output, error, status) = tokio::try_join!(
            write,
            bounded(stdout, output_limit),
            bounded(stderr, ERROR_LIMIT),
            wait
        )?;
        if !status.success() || !error.is_empty() {
            // Neither stdout nor stderr is safe to put in a user error or log.
            return Err("Qoder独立签名组件拒绝请求".into());
        }
        serde_json::from_slice(&output).map_err(|_| "Qoder签名响应格式无效".into())
    };
    match tokio::time::timeout(deadline, exchange).await {
        Ok(result) => result,
        Err(_) => Err("Qoder独立签名超时".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn native_pipe_creates_runtime_fields_and_decodes_without_a_client() {
        let result = invoke(&json!({"operation":"runtime_fields","raw":"{\"uid\":\"synthetic-pipe-user\",\"security_oauth_token\":\"synthetic-job\"}"})).await.unwrap();
        assert!(result["key"].as_str().is_some_and(|v| v.len() == 172));
        assert!(!result.to_string().contains("synthetic-job"));
        assert_eq!(
            invoke(&json!({"operation":"decode_catalog","body":"{\"models\":[]}"}))
                .await
                .unwrap(),
            json!({"models":[]})
        );
        let error = invoke(&json!({"operation":"invalid","raw":"private-synthetic-sentinel"}))
            .await
            .unwrap_err();
        assert!(!error.contains("private-synthetic-sentinel"));
    }

    #[tokio::test]
    async fn native_pipe_prepares_both_inference_regions_and_preserves_complete_request_bytes() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../scripts/qoder-protocol/synthetic-inference-oracle.json"
        ))
        .unwrap();
        let request: crate::domain::ChatRequest = serde_json::from_value(json!({
            "model":"synthetic-alias","stream":true,"max_tokens":32,
            "messages":[{"role":"system","content":"synthetic instruction"},{"role":"user","content":"synthetic prompt"}],
            "thinking":{"effort":"high"}
        })).unwrap();
        let model = crate::qoder_auth::CatalogModel {
            id: "synthetic-model".into(),
            name: "Synthetic".into(),
            format: "openai".into(),
            enabled: Some(true),
            vision: Some(true),
            thinking: Some(true),
            free: None,
            credit_multiplier: None,
            context_window: None,
            max_output_tokens: None,
        };
        let body = crate::qoder_auth::request::build(
            &request,
            &model,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            1700000000000,
        )
        .unwrap();
        let raw = serde_json::to_string(&body).unwrap();
        for region in ["global", "cn"] {
            let mut context = oracle["context"].clone();
            context["region"] = json!(region);
            let prepared = invoke(&json!({"operation":"prepare_inference","context":context,"raw":raw,"model":"synthetic-model"})).await.unwrap();
            let expected = if region == "global" {
                "https://api2.qoder.sh"
            } else {
                "https://gateway.qoder.com.cn"
            };
            assert_eq!(
                reqwest::Url::parse(prepared["url"].as_str().unwrap())
                    .unwrap()
                    .origin()
                    .ascii_serialization(),
                expected
            );
            assert_eq!(prepared["headers"]["X-Model-Key"], "synthetic-model");
            assert_eq!(prepared["headers"]["X-Model-Source"], "system");
            assert_eq!(prepared["headers"]["Cosy-Data-Policy"], "disagree");
            assert_eq!(prepared["headers"].as_object().unwrap().len(), 22);
            // The same independent decoder used by the catalog pipe consumes the
            // actual encoded inference bytes, not a second request builder.
            assert_eq!(
                invoke(&json!({"operation":"decode_catalog","body":prepared["body"]}))
                    .await
                    .unwrap(),
                body
            );
            let rejected=invoke(&json!({"operation":"prepare_inference","context":context,"raw":raw,"model":"different-model"})).await.unwrap_err();
            assert!(!rejected.contains("synthetic prompt"));
        }
    }

    #[tokio::test]
    async fn native_pipe_handles_encoded_body_larger_than_the_old_four_mib_output_limit() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../scripts/qoder-protocol/synthetic-inference-oracle.json"
        ))
        .unwrap();
        let mut context = oracle["context"].clone();
        context["region"] = json!("cn");
        let raw=serde_json::to_string(&json!({"model_config":{"key":"synthetic-model","source":"system"},"messages":[{"role":"user","content":"x".repeat(3*1024*1024+128)}]})).unwrap();
        let result=invoke(&json!({"operation":"prepare_inference","context":context,"raw":raw,"model":"synthetic-model"})).await.unwrap();
        assert!(result["body"].as_str().unwrap().len() > 4 * 1024 * 1024);
        assert_eq!(result["headers"]["X-Model-Key"], "synthetic-model");
    }

    #[tokio::test]
    async fn native_pipe_includes_escaped_raw_json_and_context_in_its_input_budget() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../scripts/qoder-protocol/synthetic-inference-oracle.json"
        ))
        .unwrap();
        let mut context = oracle["context"].clone();
        context["region"] = json!("global");
        let mut body = json!({"model_config":{"key":"synthetic-model","source":"system"},"messages":[{"role":"user","content":""}]});
        let empty = serde_json::to_string(&body).unwrap().len();
        body["messages"][0]["content"] = json!("\\".repeat((4 * 1024 * 1024 - empty) / 2));
        let raw = serde_json::to_string(&body).unwrap();
        assert!(raw.len() <= 4 * 1024 * 1024);
        let input = json!({"operation":"prepare_inference","context":context,"raw":raw,"model":"synthetic-model"});
        assert!(serde_json::to_vec(&input).unwrap().len() > 8 * 1024 * 1024);
        assert!(serde_json::to_vec(&input).unwrap().len() < INPUT_LIMIT);
        let prepared = invoke(&input).await.unwrap();
        assert!(prepared["body"].as_str().unwrap().len() > 4 * 1024 * 1024);
        assert_eq!(prepared["headers"]["X-Model-Key"], "synthetic-model");
    }

    #[tokio::test]
    async fn bounded_reader_rejects_one_byte_over_limit() {
        assert_eq!(bounded(&b"1234"[..], 4).await.unwrap(), b"1234");
        assert!(bounded(&b"12345"[..], 4).await.is_err());
    }

    struct TestDirectory(PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn test_worker(code: &str) -> (TestDirectory, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("llmgw-qoder-pipe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let worker = directory.join("worker.cjs");
        std::fs::write(&worker, code).unwrap();
        (TestDirectory(directory), worker)
    }

    #[tokio::test]
    async fn native_pipe_clears_startup_environment_and_rejects_invalid_output() {
        let node = resources().unwrap().join("node/node.exe");
        let (_directory, worker) = test_worker("process.stdin.resume(); process.stdin.on('end',()=>process.stdout.write(JSON.stringify(Object.keys(process.env))));");
        let result = run(&node, &worker, &json!({}), Duration::from_secs(5))
            .await
            .unwrap();
        for name in result.as_array().unwrap() {
            assert!(name
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case("SystemRoot")));
        }
        let (_directory, worker) = test_worker("process.stdin.resume(); process.stdin.on('end',()=>process.stdout.write('private-invalid-output'));");
        let error = run(&node, &worker, &json!({}), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(!error.contains("private-invalid-output"));
        let (_directory, worker) = test_worker("process.stdin.resume(); process.stdin.on('end',()=>{process.stdout.write(JSON.stringify({ok:true}));process.stderr.write('private-error-output');});");
        let error = run(&node, &worker, &json!({}), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(!error.contains("private-error-output"));
    }

    #[cfg(windows)]
    fn alive(pid: u32) -> bool {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0;
            let read = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            assert_ne!(read, 0, "读取自有Node进程状态");
            code == 259
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_and_task_cancellation_stop_only_the_owned_worker() {
        let node = resources().unwrap().join("node/node.exe");
        for abort in [false, true] {
            let node = node.clone();
            let (directory, worker) = test_worker("");
            let pid_file = directory.0.join("own.pid");
            let literal = serde_json::to_string(&pid_file.to_string_lossy()).unwrap();
            std::fs::write(&worker, format!("require('node:fs').writeFileSync({literal},String(process.pid)); process.stdin.resume();setInterval(()=>{{}},1000);")).unwrap();
            let pending = tokio::spawn(async move {
                run(&node, &worker, &json!({}), Duration::from_secs(2)).await
            });
            let pid = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Ok(text) = tokio::fs::read_to_string(&pid_file).await {
                        if let Ok(pid) = text.parse::<u32>() {
                            break pid;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(alive(pid), "对照：本测试启动的Node确实在运行");
            if abort {
                pending.abort();
                assert!(pending.await.unwrap_err().is_cancelled());
            } else {
                assert_eq!(pending.await.unwrap().unwrap_err(), "Qoder独立签名超时");
            }
            assert!(!alive(pid), "超时或取消后自有Node已退出");
        }
    }
}
