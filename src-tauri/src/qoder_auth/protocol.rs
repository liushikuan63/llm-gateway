//! Bounded private pipes to our bundled crypto implementation. No SDK or client.
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
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

// Diagnostics contain only closed vocabulary and counters, never pipe contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipePhase {
    Encode,
    Spawn,
    Pipes,
    Write,
    Shutdown,
    ReadStdout,
    ReadStderr,
    Wait,
    Worker,
    Decode,
    Complete,
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipeErrorClass {
    None,
    InputLimit,
    Io,
    OutputLimit,
    WorkerRejected,
    UnclassifiedStderr,
    NonzeroExit,
    InvalidResponse,
    Timeout,
}

#[derive(Debug, PartialEq, Eq)]
struct PipeDiagnostic {
    phase: PipePhase,
    error_class: PipeErrorClass,
    exit_code: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
}

impl PipeDiagnostic {
    fn emit(&self) {
        tracing::warn!(
            target: "llm_gateway::private_pipe",
            phase = ?self.phase,
            error_class = ?self.error_class,
            exit_code = ?self.exit_code,
            stdout_bytes = self.stdout_bytes,
            stderr_bytes = self.stderr_bytes,
            "private pipe failed"
        );
    }
}

struct PipeObservation {
    diagnostic: Mutex<PipeDiagnostic>,
    stdout_bytes: AtomicUsize,
    stderr_bytes: AtomicUsize,
}

impl PipeObservation {
    fn new() -> Self {
        Self {
            diagnostic: Mutex::new(PipeDiagnostic {
                phase: PipePhase::Encode,
                error_class: PipeErrorClass::None,
                exit_code: None,
                stdout_bytes: 0,
                stderr_bytes: 0,
            }),
            stdout_bytes: AtomicUsize::new(0),
            stderr_bytes: AtomicUsize::new(0),
        }
    }

    fn error(&self, phase: PipePhase, class: PipeErrorClass, message: &'static str) -> String {
        let mut diagnostic = self.diagnostic.lock().unwrap_or_else(|e| e.into_inner());
        diagnostic.phase = phase;
        diagnostic.error_class = class;
        message.into()
    }

    fn snapshot(&self) -> PipeDiagnostic {
        let diagnostic = self.diagnostic.lock().unwrap_or_else(|e| e.into_inner());
        PipeDiagnostic {
            phase: diagnostic.phase,
            error_class: diagnostic.error_class,
            exit_code: diagnostic.exit_code,
            stdout_bytes: self.stdout_bytes.load(Ordering::Relaxed),
            stderr_bytes: self.stderr_bytes.load(Ordering::Relaxed),
        }
    }
}

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

async fn bounded(
    mut reader: impl AsyncRead + Unpin,
    maximum: usize,
    bytes: &AtomicUsize,
) -> Result<Vec<u8>, &'static str> {
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
        bytes.fetch_add(size, Ordering::Relaxed);
        if result.len().saturating_add(size) > maximum {
            return Err("Qoder签名管道输出过大");
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
    let observation = PipeObservation::new();
    let result = run_limited_observed(
        node,
        worker,
        request,
        deadline,
        input_limit,
        output_limit,
        &observation,
    )
    .await;
    if result.is_err() {
        observation.snapshot().emit();
    }
    result
}

async fn run_limited_observed(
    node: &Path,
    worker: &Path,
    request: &Value,
    deadline: Duration,
    input_limit: usize,
    output_limit: usize,
    observation: &PipeObservation,
) -> Result<Value, String> {
    let input = serde_json::to_vec(request).map_err(|_| {
        observation.error(
            PipePhase::Encode,
            PipeErrorClass::Io,
            "Qoder签名输入编码失败",
        )
    })?;
    if input.len() > input_limit {
        return Err(observation.error(
            PipePhase::Encode,
            PipeErrorClass::InputLimit,
            "Qoder签名输入过大",
        ));
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
    let mut child = OwnedChild(command.spawn().map_err(|_| {
        observation.error(
            PipePhase::Spawn,
            PipeErrorClass::Io,
            "Qoder独立签名进程启动失败",
        )
    })?);
    let mut stdin = child.0.stdin.take().ok_or_else(|| {
        observation.error(
            PipePhase::Pipes,
            PipeErrorClass::Io,
            "Qoder签名输入管道缺失",
        )
    })?;
    let stdout = child.0.stdout.take().ok_or_else(|| {
        observation.error(
            PipePhase::Pipes,
            PipeErrorClass::Io,
            "Qoder签名输出管道缺失",
        )
    })?;
    let stderr = child.0.stderr.take().ok_or_else(|| {
        observation.error(
            PipePhase::Pipes,
            PipeErrorClass::Io,
            "Qoder签名错误管道缺失",
        )
    })?;
    let exchange = async {
        let write = async {
            stdin.write_all(&input).await.map_err(|_| {
                observation.error(
                    PipePhase::Write,
                    PipeErrorClass::Io,
                    "Qoder签名输入发送失败",
                )
            })?;
            stdin.shutdown().await.map_err(|_| {
                observation.error(
                    PipePhase::Shutdown,
                    PipeErrorClass::Io,
                    "Qoder签名输入关闭失败",
                )
            })?;
            drop(stdin);
            Ok::<_, String>(())
        };
        let wait = async {
            let status = child.0.wait().await.map_err(|_| {
                observation.error(PipePhase::Wait, PipeErrorClass::Io, "Qoder签名进程等待失败")
            })?;
            observation
                .diagnostic
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .exit_code = status.code();
            Ok::<_, String>(status)
        };
        let read = |phase, message| {
            observation.error(
                phase,
                if message == "Qoder签名管道输出过大" {
                    PipeErrorClass::OutputLimit
                } else {
                    PipeErrorClass::Io
                },
                message,
            )
        };
        let (_, output, error, status) = tokio::try_join!(
            write,
            async {
                bounded(stdout, output_limit, &observation.stdout_bytes)
                    .await
                    .map_err(|message| read(PipePhase::ReadStdout, message))
            },
            async {
                bounded(stderr, ERROR_LIMIT, &observation.stderr_bytes)
                    .await
                    .map_err(|message| read(PipePhase::ReadStderr, message))
            },
            wait
        )?;
        if !status.success() || !error.is_empty() {
            // Neither stdout nor stderr is safe to put in a user error or log.
            let class = if error == b"QODER_PROTOCOL_REQUEST_REJECTED\n" {
                PipeErrorClass::WorkerRejected
            } else if !error.is_empty() {
                PipeErrorClass::UnclassifiedStderr
            } else {
                PipeErrorClass::NonzeroExit
            };
            return Err(observation.error(PipePhase::Worker, class, "Qoder独立签名组件拒绝请求"));
        }
        let result = serde_json::from_slice(&output).map_err(|_| {
            observation.error(
                PipePhase::Decode,
                PipeErrorClass::InvalidResponse,
                "Qoder签名响应格式无效",
            )
        })?;
        observation
            .diagnostic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase = PipePhase::Complete;
        Ok(result)
    };
    match tokio::time::timeout(deadline, exchange).await {
        Ok(result) => result,
        Err(_) => Err(observation.error(
            PipePhase::Timeout,
            PipeErrorClass::Timeout,
            "Qoder独立签名超时",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::Write,
        sync::{Arc, Once},
    };
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct DiagnosticWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for DiagnosticWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DiagnosticWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

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
        // Keep the original invoke path observable under default lib parallelism.
        // A persistent, narrowly filtered test dispatch avoids scoped callsite
        // registration racing with other tests that have no default subscriber.
        static PIPE_TRACE: Once = Once::new();
        PIPE_TRACE.call_once(|| {
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_test_writer()
                .with_env_filter("off,llm_gateway::private_pipe=warn")
                .try_init()
                .expect("install test-only private pipe diagnostics");
        });
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
        let bytes = AtomicUsize::new(0);
        assert_eq!(bounded(&b"1234"[..], 4, &bytes).await.unwrap(), b"1234");
        assert_eq!(bytes.load(Ordering::Relaxed), 4);
        let bytes = AtomicUsize::new(0);
        assert!(bounded(&b"12345"[..], 4, &bytes).await.is_err());
        assert_eq!(bytes.load(Ordering::Relaxed), 5);
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
    async fn diagnostic_contract_distinguishes_worker_outcomes_without_raw_errors() {
        let node = resources().unwrap().join("node/node.exe");
        let fixtures = [
            (
                "process.stdout.write('{}');",
                None,
                PipePhase::Complete,
                PipeErrorClass::None,
                0,
                2,
                0,
            ),
            (
                "process.stderr.write('QODER_PROTOCOL_REQUEST_REJECTED\\n');process.exitCode=1;",
                Some("Qoder独立签名组件拒绝请求"),
                PipePhase::Worker,
                PipeErrorClass::WorkerRejected,
                1,
                0,
                32,
            ),
            (
                "process.stdout.write('private-stdout-sentinel');process.stderr.write('Error: private-dynamic-error-sentinel\\n');process.exitCode=17;",
                Some("Qoder独立签名组件拒绝请求"),
                PipePhase::Worker,
                PipeErrorClass::UnclassifiedStderr,
                17,
                23,
                38,
            ),
            (
                "process.exitCode=19;",
                Some("Qoder独立签名组件拒绝请求"),
                PipePhase::Worker,
                PipeErrorClass::NonzeroExit,
                19,
                0,
                0,
            ),
            (
                "process.stdout.write('private-invalid-json-sentinel');",
                Some("Qoder签名响应格式无效"),
                PipePhase::Decode,
                PipeErrorClass::InvalidResponse,
                0,
                29,
                0,
            ),
        ];
        for (code, error, phase, class, exit_code, stdout_bytes, stderr_bytes) in fixtures {
            let code = format!("process.stdin.resume();process.stdin.on('end',()=>{{{code}}});");
            let (_directory, worker) = test_worker(&code);
            let observation = PipeObservation::new();
            let result = run_limited_observed(
                &node,
                &worker,
                &json!({"raw":"private-request-sentinel","context":{"token":"private-context-sentinel"}}),
                Duration::from_secs(5),
                INPUT_LIMIT,
                OUTPUT_LIMIT,
                &observation,
            )
            .await;
            assert_eq!(result.as_ref().err().map(String::as_str), error);
            assert_eq!(
                observation.snapshot(),
                PipeDiagnostic {
                    phase,
                    error_class: class,
                    exit_code: Some(exit_code),
                    stdout_bytes,
                    stderr_bytes,
                }
            );
            assert!(!format!("{:?}", observation.snapshot()).contains("private-"));
        }
    }

    #[tokio::test]
    async fn diagnostic_logging_emits_one_safe_event_only_for_failure() {
        // Scoped tracing subscribers share a process-wide callsite cache. Keep
        // this capture contract independent of other parallel tests' defaults.
        const HELPER: &str = "LLMGW_PRIVATE_PIPE_LOG_CONTRACT_HELPER";
        if std::env::var_os(HELPER).is_none() {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "qoder_auth::protocol::tests::diagnostic_logging_emits_one_safe_event_only_for_failure",
                    "--nocapture",
                ])
                .env(HELPER, "1")
                .stdin(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            let mut child = OwnedChild(command.spawn().unwrap());
            let status = tokio::time::timeout(Duration::from_secs(20), child.0.wait())
                .await
                .expect("isolated diagnostic capture completed within its test deadline")
                .unwrap();
            assert!(status.success(), "isolated diagnostic capture failed");
            return;
        }
        let node = resources().unwrap().join("node/node.exe");
        for rejected in [false, true] {
            let code = if rejected {
                "process.stdout.write('private-stdout-sentinel');process.stderr.write('private-dynamic-error-sentinel');process.exitCode=7;"
            } else {
                "process.stdout.write('{}');"
            };
            let (_directory, worker) = test_worker(&format!(
                "process.stdin.resume();process.stdin.on('end',()=>{{{code}}});"
            ));
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_target(false)
                .with_writer(DiagnosticWriter(bytes.clone()))
                .finish();
            let result = run(
                &node,
                &worker,
                &json!({"raw":"private-request-sentinel","context":{"token":"private-context-sentinel"}}),
                Duration::from_secs(5),
            )
            .with_subscriber(subscriber)
            .await;
            assert_eq!(result.is_err(), rejected);
            let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
            assert_eq!(log.lines().count(), usize::from(rejected));
            assert!(!log.contains("private-"));
            assert!(!log.contains(&worker.to_string_lossy().to_string()));
            if rejected {
                assert!(log.contains("phase=Worker"), "{log}");
                assert!(log.contains("error_class=UnclassifiedStderr"), "{log}");
                assert!(log.contains("exit_code=Some(7)"), "{log}");
                assert!(log.contains("stdout_bytes=23"), "{log}");
                assert!(log.contains("stderr_bytes=30"), "{log}");
            }
        }
    }

    #[tokio::test]
    async fn diagnostic_contract_preserves_input_spawn_limit_and_timeout_boundaries() {
        let node = resources().unwrap().join("node/node.exe");
        let (_directory, worker) = test_worker(
            "process.stdin.resume();process.stdin.on('end',()=>process.stdout.write('{}'));",
        );
        for (executable, input_limit, phase, class, message) in [
            (
                node.clone(),
                1,
                PipePhase::Encode,
                PipeErrorClass::InputLimit,
                "Qoder签名输入过大",
            ),
            (
                worker.with_file_name("private-missing-node-sentinel.exe"),
                INPUT_LIMIT,
                PipePhase::Spawn,
                PipeErrorClass::Io,
                "Qoder独立签名进程启动失败",
            ),
        ] {
            let observation = PipeObservation::new();
            assert_eq!(
                run_limited_observed(
                    &executable,
                    &worker,
                    &json!({"raw":"private-request-sentinel"}),
                    Duration::from_secs(5),
                    input_limit,
                    OUTPUT_LIMIT,
                    &observation,
                )
                .await
                .unwrap_err(),
                message
            );
            assert_eq!(
                observation.snapshot(),
                PipeDiagnostic {
                    phase,
                    error_class: class,
                    exit_code: None,
                    stdout_bytes: 0,
                    stderr_bytes: 0,
                }
            );
        }
        for (code, output_limit, phase, class, message, stdout_bytes, stderr_bytes) in [
            (
                "process.stdout.write('12345');",
                4,
                PipePhase::ReadStdout,
                PipeErrorClass::OutputLimit,
                "Qoder签名管道输出过大",
                5,
                0,
            ),
            (
                "process.stderr.write('x'.repeat(4097));",
                OUTPUT_LIMIT,
                PipePhase::ReadStderr,
                PipeErrorClass::OutputLimit,
                "Qoder签名管道输出过大",
                0,
                4097,
            ),
            (
                "process.stdout.write('123');setInterval(()=>{},1000);",
                OUTPUT_LIMIT,
                PipePhase::Timeout,
                PipeErrorClass::Timeout,
                "Qoder独立签名超时",
                3,
                0,
            ),
        ] {
            let (_directory, worker) = test_worker(&format!(
                "process.stdin.resume();process.stdin.on('end',()=>{{{code}}});"
            ));
            let observation = PipeObservation::new();
            assert_eq!(
                run_limited_observed(
                    &node,
                    &worker,
                    &json!({}),
                    Duration::from_secs(2),
                    INPUT_LIMIT,
                    output_limit,
                    &observation,
                )
                .await
                .unwrap_err(),
                message
            );
            let diagnostic = observation.snapshot();
            assert_eq!(diagnostic.phase, phase);
            assert_eq!(diagnostic.error_class, class);
            assert_eq!(diagnostic.stdout_bytes, stdout_bytes);
            assert_eq!(diagnostic.stderr_bytes, stderr_bytes);
        }
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
