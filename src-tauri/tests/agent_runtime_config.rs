//! Offline runtime/configuration and real child-process regressions. No vendor calls.

use llm_gateway_lib::agent_upstream::headless::{
    invocation, parse_reply, verify_opencode_config, ClientKind,
};
use llm_gateway_lib::agent_upstream::{
    call_agent_with_runtime, resolve_runtime, run_agent_request_with_runtime, AdapterRegistry,
};
use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{AgentRuntime, Message};
use serde_json::{json, Value};

#[tokio::test]
async fn custom_runtime_kind_and_model_aliases_reach_the_actual_adapter() {
    let db = Db::connect_in_memory().await.unwrap();
    let registry = AdapterRegistry::with_builtins();
    let mut runtime = AgentRuntime::new("personal-account", "fake", "Account");
    runtime.options = Some(json!({"model_aliases":{"public-model":"actual-model"}}));
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    let reply = call_agent_with_runtime(
        db.pool(),
        &registry,
        &runtime.id,
        "public-model",
        &[Message::user("config-regression")],
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(reply.model, "public-model");
    let text = reply.content;
    assert!(text.contains("model=actual-model") && text.contains("config-regression"));
    runtime.options = None;
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    let reply = call_agent_with_runtime(
        db.pool(),
        &registry,
        &runtime.id,
        "public-model",
        &[Message::user("config-regression")],
        1_000,
    )
    .await
    .unwrap();
    assert!(reply.content.contains("model=public-model"));
}

#[tokio::test]
async fn disabled_database_row_overrides_even_a_builtin_id() {
    let db = Db::connect_in_memory().await.unwrap();
    let registry = AdapterRegistry::with_builtins();
    assert_eq!(
        resolve_runtime(db.pool(), &registry, "fake")
            .await
            .unwrap()
            .kind,
        "fake"
    );
    let mut runtime = AgentRuntime::new("fake", "fake", "Disabled");
    runtime.enabled = false;
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    let error = call_agent_with_runtime(db.pool(), &registry, "fake", "m", &[], 1_000)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("停用"));
    runtime.enabled = true;
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    assert!(
        call_agent_with_runtime(db.pool(), &registry, "fake", "m", &[], 1_000)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn database_kind_is_authoritative_and_changes_apply_without_restart() {
    let db = Db::connect_in_memory().await.unwrap();
    let registry = AdapterRegistry::with_builtins();
    let mut runtime = AgentRuntime::new("fake", "unknown-kind", "Kind");
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    assert!(resolve_runtime(db.pool(), &registry, "fake")
        .await
        .err()
        .unwrap()
        .contains("unknown-kind"));
    runtime.kind = "fake".into();
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    assert_eq!(
        resolve_runtime(db.pool(), &registry, "fake")
            .await
            .unwrap()
            .kind,
        "fake"
    );
}

#[tokio::test]
async fn legacy_builtin_ids_work_but_missing_user_ids_do_not_fall_back() {
    let db = Db::connect_in_memory().await.unwrap();
    let registry = AdapterRegistry::with_builtins();
    for id in [
        "fake",
        "codex",
        "qoder",
        "qoder-cn",
        "claude-code",
        "opencode",
    ] {
        assert_eq!(
            resolve_runtime(db.pool(), &registry, id)
                .await
                .unwrap()
                .adapter
                .id(),
            id
        );
    }
    assert!(resolve_runtime(db.pool(), &registry, "absent-personal-id")
        .await
        .is_err());
}

#[tokio::test]
async fn unsafe_or_malformed_options_are_rejected_instead_of_ignored() {
    let db = Db::connect_in_memory().await.unwrap();
    let registry = AdapterRegistry::with_builtins();
    for options in [
        json!({"args":["--tools=default"]}),
        json!({"env":{"SECRET":"fixture"}}),
        json!({"executable":false}),
        json!({"exe":"a","executable":"b"}),
        json!({"model_aliases":{"m":[]}}),
        json!({"model_aliases":[]}),
    ] {
        let mut runtime = AgentRuntime::new("bad-options", "qoder-cn", "Bad");
        runtime.options = Some(options);
        repo::upsert_agent_runtime(db.pool(), &runtime)
            .await
            .unwrap();
        assert!(resolve_runtime(db.pool(), &registry, &runtime.id)
            .await
            .is_err());
    }
}

#[tokio::test]
async fn corrupt_stored_options_never_revert_to_a_default_builtin_command() {
    let db = Db::connect_in_memory().await.unwrap();
    let registry = AdapterRegistry::with_builtins();
    let runtime = AgentRuntime::new("fake", "fake", "Stored config");
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    for raw in ["{secret-fixture", "null", "[]", "42", "\"secret-fixture\""] {
        sqlx::query("UPDATE agent_runtimes SET options_json = ? WHERE id = 'fake'")
            .bind(raw)
            .execute(db.pool())
            .await
            .unwrap();
        let error = call_agent_with_runtime(db.pool(), &registry, "fake", "m", &[], 1_000)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("secret-fixture"));
        assert!(
            repo::list_agent_runtimes(db.pool()).await.is_ok(),
            "display list must remain accessible for repair"
        );
    }
    sqlx::query("UPDATE agent_runtimes SET options_json = NULL WHERE id = 'fake'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        call_agent_with_runtime(db.pool(), &registry, "fake", "m", &[], 1_000)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn global_agent_gate_precedes_db_reads_and_workspace_creation() {
    let db = Db::connect_in_memory().await.unwrap();
    db.pool().close().await;
    let cfg = llm_gateway_lib::config::AgentConfig::default();
    let error = run_agent_request_with_runtime(
        db.pool(),
        &cfg,
        &AdapterRegistry::with_builtins(),
        "fake",
        "m",
        "p",
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("未启用"),
        "a disabled gate must never reach the closed DB: {error}"
    );
}

#[test]
fn qoder_cn_and_claude_invocations_cannot_inherit_builtin_or_mcp_tools() {
    for kind in [ClientKind::QoderCn, ClientKind::ClaudeCode] {
        let command = invocation(kind, "vendor-model", "--tools=default", "fixture-agent");
        for flag in [
            "--tools=",
            "--strict-mcp-config",
            "--setting-sources=",
            "--no-session-persistence",
        ] {
            assert!(command.args.iter().any(|arg| arg == flag));
        }
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["--mcp-config", "{\"mcpServers\":{}}"]));
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["--model", "vendor-model"]));
        assert_eq!(
            &command.args[command.args.len() - 2..],
            ["--", "--tools=default"]
        );
        assert!(command.env.is_empty());
    }
    let command = invocation(ClientKind::ClaudeCode, "", "text", "fixture-agent");
    assert!(command.args.iter().any(|arg| arg == "--safe-mode"));
    assert!(command
        .args
        .windows(2)
        .any(|args| args == ["--disallowedTools", "mcp__*"]));
}

#[test]
fn opencode_uses_an_explicit_deny_agent_and_stdin_without_auto_approval_or_share() {
    let command = invocation(
        ClientKind::OpenCode,
        "provider/model",
        "sensitive prompt",
        "fixture-unique-agent",
    );
    assert_eq!(command.stdin.as_deref(), Some("sensitive prompt"));
    assert!(!command
        .args
        .iter()
        .any(|arg| ["--auto", "--share", "--attach", "sensitive prompt"].contains(&arg.as_str())));
    assert!(command
        .args
        .windows(2)
        .any(|args| args == ["--agent", "fixture-unique-agent"]));
    let config: Value = serde_json::from_str(
        &command
            .env
            .iter()
            .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(
        config["agent"]["fixture-unique-agent"]["permission"],
        "deny"
    );
    assert_eq!(config["agent"]["fixture-unique-agent"]["mode"], "primary");
    assert_eq!(config["share"], "disabled");
    assert!(command
        .env
        .contains(&("OPENCODE_AUTO_SHARE".into(), "false".into())));
}

#[test]
fn final_json_requires_success_and_does_not_leak_error_bodies() {
    for kind in [ClientKind::QoderCn, ClientKind::ClaudeCode] {
        assert_eq!(
            parse_reply(
                kind,
                r#"{"type":"result","subtype":"success","is_error":false,"result":"complete"}"#
            )
            .unwrap(),
            "complete"
        );
        for value in [
            json!({"type":"result","subtype":"success","is_error":true,"result":"secret-fixture"}),
            json!({"type":"assistant","result":"secret-fixture"}),
            json!({"type":"result","subtype":"error_max_turns","is_error":false,"result":"partial"}),
        ] {
            let error = parse_reply(kind, &value.to_string()).unwrap_err();
            assert!(!error.contains("secret-fixture") && !error.contains("partial"));
        }
    }
}

#[test]
fn opencode_requires_complete_stop_and_rejects_errors_or_tool_events() {
    let text = r#"{"type":"text","part":{"text":"complete"}}"#;
    let stop = r#"{"type":"step_finish","part":{"reason":"stop"}}"#;
    assert_eq!(
        parse_reply(ClientKind::OpenCode, &format!("{text}\n{stop}")).unwrap(),
        "complete"
    );
    for suffix in [
        "",
        r#"{"type":"step_finish","part":{"reason":"tool-calls"}}"#,
        r#"{"type":"error","error":{"message":"secret-fixture"}}"#,
        r#"{"type":"tool_use","part":{"tool":"bash"}}"#,
        "{invalid-json}",
    ] {
        let error = parse_reply(ClientKind::OpenCode, &format!("{text}\n{suffix}")).unwrap_err();
        assert!(!error.contains("secret-fixture"));
    }
}

#[test]
fn opencode_resolved_config_prevents_agent_fallback_and_user_extensions_before_prompt() {
    let good = json!({"share":"disabled", "agent":{"fixture-agent":{"mode":"primary","permission":{"*":"deny"}}}, "plugin":[],"mcp":{"disabled-server":{"enabled":false}}});
    verify_opencode_config(&good.to_string(), "fixture-agent").unwrap();
    let mut variants = vec![json!({"agent":{},"share":"disabled"})];
    let mut bad = good.clone();
    bad["agent"]["fixture-agent"]["permission"]["bash"] = json!("allow");
    variants.push(bad);
    let mut bad = good.clone();
    bad["agent"]["fixture-agent"]["mode"] = json!("subagent");
    variants.push(bad);
    let mut bad = good.clone();
    bad["agent"]["fixture-agent"]["disable"] = json!(true);
    variants.push(bad);
    let mut bad = good.clone();
    bad["share"] = json!("auto");
    variants.push(bad);
    let mut bad = good.clone();
    bad["plugin"] = json!(["fixture-private-plugin"]);
    variants.push(bad);
    let mut bad = good.clone();
    bad["mcp"]["enabled-server"] = json!({"command":"fixture-sensitive-command"});
    variants.push(bad);
    for value in variants {
        let error = verify_opencode_config(&value.to_string(), "fixture-agent").unwrap_err();
        assert!(
            !error.contains("fixture-private-plugin")
                && !error.contains("fixture-sensitive-command")
        );
    }
}

#[cfg(windows)]
struct TempFixture(std::path::PathBuf);
#[cfg(windows)]
impl TempFixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("llmgw-runtime-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn script(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_string()
    }
}
#[cfg(windows)]
impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn database_executable_override_and_legacy_exe_alias_run_the_fixture_not_vendor_cli() {
    let fixture = TempFixture::new();
    let executable = fixture.script("fixture.cmd", "@echo off\r\n@echo {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"override-fixture\"}\r\n");
    let db = Db::connect_in_memory().await.unwrap();
    for field in ["executable", "exe"] {
        let mut runtime = AgentRuntime::new("own-cli", "claude-code", "Fixture");
        runtime.options = Some(json!({ (field): executable }));
        repo::upsert_agent_runtime(db.pool(), &runtime)
            .await
            .unwrap();
        let reply = call_agent_with_runtime(
            db.pool(),
            &AdapterRegistry::with_builtins(),
            &runtime.id,
            "",
            &[Message::user("fixture")],
            10_000,
        )
        .await
        .unwrap();
        assert_eq!(reply.content, "override-fixture");
    }
}

#[cfg(windows)]
#[tokio::test]
async fn real_child_receives_exact_args_stdin_and_child_only_environment() {
    use llm_gateway_lib::agent_upstream::run_json_cli_with_input;
    let fixture = TempFixture::new();
    let script = fixture.script("capture.ps1", "$ErrorActionPreference='Stop'\n[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)\n[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)\n[ordered]@{args=@($args);input=[Console]::In.ReadToEnd();config=$env:OPENCODE_CONFIG_CONTENT}|ConvertTo-Json -Compress -Depth 15\n");
    let parent = std::env::var_os("OPENCODE_CONFIG_CONTENT");
    for kind in [
        ClientKind::QoderCn,
        ClientKind::ClaudeCode,
        ClientKind::OpenCode,
    ] {
        let command = invocation(kind, "provider/model", "prompt-fixture", "fixture-agent");
        let mut args = vec!["-NoProfile".into(), "-File".into(), script.clone()];
        args.extend(command.args.clone());
        let stdout = run_json_cli_with_input(
            "powershell.exe",
            &args,
            10_000,
            "fixture-capture",
            &command.env,
            command.stdin.as_deref(),
        )
        .await
        .unwrap();
        let captured: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(captured["args"], json!(command.args));
        assert_eq!(captured["input"], command.stdin.unwrap_or_default());
        if kind == ClientKind::OpenCode {
            assert!(captured["config"]
                .as_str()
                .unwrap()
                .contains("fixture-agent"));
        }
    }
    assert_eq!(std::env::var_os("OPENCODE_CONFIG_CONTENT"), parent);
}

#[cfg(windows)]
#[tokio::test]
async fn runner_redacts_stderr_and_bounds_captured_output() {
    use llm_gateway_lib::agent_upstream::run_json_cli;
    let fixture = TempFixture::new();
    let error_script = fixture.script("error.ps1", "[Console]::Error.WriteLine('authentication failed secret-fixture prompt-fixture'); exit 9\n");
    let error = run_json_cli(
        "powershell.exe",
        &["-NoProfile".into(), "-File".into(), error_script],
        10_000,
        "fixture-error",
    )
    .await
    .unwrap_err();
    assert!(error.contains('9') && error.contains("凭据"));
    assert!(!error.contains("secret-fixture") && !error.contains("prompt-fixture"));
    let oversized = fixture.script(
        "large.ps1",
        "[Console]::Out.Write(('x' * (8*1024*1024+1))); Start-Sleep -Seconds 60\n",
    );
    let began = std::time::Instant::now();
    let error = run_json_cli(
        "powershell.exe",
        &["-NoProfile".into(), "-File".into(), oversized],
        10_000,
        "fixture-limit",
    )
    .await
    .unwrap_err();
    assert!(error.contains("大小限制"), "{error}");
    assert!(began.elapsed() < std::time::Duration::from_secs(10));
}

#[cfg(windows)]
#[tokio::test]
async fn runner_deadline_includes_a_blocked_stdin_writer() {
    use llm_gateway_lib::agent_upstream::run_json_cli_with_input;
    let fixture = TempFixture::new();
    let script = fixture.script("no-input.ps1", "Start-Sleep -Seconds 60\n");
    let args = vec!["-NoProfile".into(), "-File".into(), script];
    let began = std::time::Instant::now();
    let error = run_json_cli_with_input(
        "powershell.exe",
        &args,
        600,
        "fixture-timeout",
        &[],
        Some(&"x".repeat(1024 * 1024)),
    )
    .await
    .unwrap_err();
    assert!(error.contains("超时"));
    assert!(began.elapsed() < std::time::Duration::from_secs(5));
}

#[cfg(windows)]
#[tokio::test]
async fn opencode_preflight_rejects_bad_agent_plugins_and_mcp_without_run_or_prompt() {
    use llm_gateway_lib::agent_upstream::headless::HeadlessAdapter;
    use llm_gateway_lib::agent_upstream::{AgentAdapter, AgentRequest};
    let fixture = TempFixture::new();
    fixture.script("fixture.ps1", r#"$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$root=Split-Path -Parent $MyInvocation.MyCommand.Path
[IO.File]::AppendAllText((Join-Path $root 'actions.txt'), ($args[0]+"`n"))
if($args[0] -eq 'debug') {
  $cfg=$env:OPENCODE_CONFIG_CONTENT|ConvertFrom-Json
  $mode=[IO.File]::ReadAllText((Join-Path $root 'mode.txt'))
  if($mode -eq 'agent') { $cfg.agent=[PSCustomObject]@{} }
  if($mode -eq 'plugin') { $cfg|Add-Member -NotePropertyName plugin -NotePropertyValue @('fixture-plugin') }
  if($mode -eq 'mcp') { $cfg|Add-Member -NotePropertyName mcp -NotePropertyValue @{fixture=@{command='fixture-command'}} }
  $cfg|ConvertTo-Json -Compress -Depth 15
  exit 0
}
[IO.File]::WriteAllText((Join-Path $root 'received-prompt.txt'), [Console]::In.ReadToEnd())
[Console]::Out.WriteLine('{"type":"text","part":{"text":"fixture-result"}}')
[Console]::Out.WriteLine('{"type":"step_finish","part":{"reason":"stop"}}')
"#);
    let executable = fixture.script(
        "fixture.cmd",
        "@echo off\r\n@powershell.exe -NoProfile -File \"%~dp0fixture.ps1\" %*\r\n",
    );
    let adapter = HeadlessAdapter::new(ClientKind::OpenCode, Some(executable));
    let request = || AgentRequest {
        model: "provider/model".into(),
        prompt: "public-prompt-fixture".into(),
        timeout_ms: 20_000,
    };
    for mode in ["agent", "plugin", "mcp"] {
        std::fs::write(fixture.0.join("mode.txt"), mode).unwrap();
        assert!(
            adapter.send(request()).await.is_err(),
            "bad {mode} must stop before a model run"
        );
        assert!(!fixture.0.join("received-prompt.txt").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("actions.txt")).unwrap(),
            "debug\n"
        );
        std::fs::remove_file(fixture.0.join("actions.txt")).unwrap();
    }
    std::fs::write(fixture.0.join("mode.txt"), "valid").unwrap();
    assert_eq!(
        adapter.send(request()).await.unwrap().text,
        "fixture-result"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("actions.txt")).unwrap(),
        "debug\nrun\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("received-prompt.txt")).unwrap(),
        "public-prompt-fixture"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn qoder_cn_receives_an_empty_mcp_file_and_removes_it_on_every_exit() {
    use llm_gateway_lib::agent_upstream::headless::HeadlessAdapter;
    use llm_gateway_lib::agent_upstream::{AgentAdapter, AgentRequest};
    let fixture = TempFixture::new();
    fixture.script("fixture.ps1", r#"$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$root=Split-Path -Parent $MyInvocation.MyCommand.Path
$index=[Array]::IndexOf($args, '--mcp-config')
if($index -lt 0) { exit 11 }
$mcpPath=$args[$index+1]
$mcp=[IO.File]::ReadAllText($mcpPath)|ConvertFrom-Json
$capture=[ordered]@{path=$mcpPath;config=$mcp;argv=@($args)}|ConvertTo-Json -Compress -Depth 15
[IO.File]::WriteAllText((Join-Path $root 'capture.json'), $capture, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $root 'ready.txt'), 'ready')
$mode=[IO.File]::ReadAllText((Join-Path $root 'mode.txt'))
if($mode -eq 'error') { [Console]::Error.WriteLine('fixture-error'); exit 7 }
if($mode -eq 'hang') { Start-Sleep -Seconds 60 }
[Console]::Out.WriteLine('{"type":"result","subtype":"success","is_error":false,"result":"cn-file-fixture"}')
"#);
    let executable = fixture.script(
        "fixture.cmd",
        "@echo off\r\n@powershell.exe -NoProfile -File \"%~dp0fixture.ps1\" %*\r\n",
    );
    let adapter = HeadlessAdapter::new(ClientKind::QoderCn, Some(executable));
    let request = |timeout_ms| AgentRequest {
        model: "".into(),
        prompt: "public-cn-prompt-fixture".into(),
        timeout_ms,
    };
    let capture_path = || {
        let capture: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture.0.join("capture.json")).unwrap())
                .unwrap();
        assert_eq!(capture["config"], json!({"mcpServers":{}}));
        assert!(capture["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--strict-mcp-config"));
        let path = std::path::PathBuf::from(capture["path"].as_str().unwrap());
        assert!(path.is_absolute(), "CN must receive an absolute file path");
        path
    };
    for mode in ["success", "error", "hang"] {
        std::fs::write(fixture.0.join("mode.txt"), mode).unwrap();
        let result = adapter
            .send(request(if mode == "hang" { 5_000 } else { 20_000 }))
            .await;
        match mode {
            "success" => assert_eq!(result.unwrap().text, "cn-file-fixture"),
            "error" => assert!(result.unwrap_err().contains("退出码 7")),
            _ => assert!(result.unwrap_err().contains("超时")),
        }
        let path = capture_path();
        assert!(!path.exists(), "{mode} left its owned MCP file behind");
        std::fs::remove_file(fixture.0.join("capture.json")).unwrap();
        std::fs::remove_file(fixture.0.join("ready.txt")).unwrap();
    }
    std::fs::write(fixture.0.join("mode.txt"), "hang").unwrap();
    let task = tokio::spawn(async move { adapter.send(request(30_000)).await });
    let ready = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !fixture.0.join("ready.txt").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    // Abort the owned request even if the readiness assertion fails.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    ready.expect("the fixture must read the MCP file before cancellation");
    let path = capture_path();
    assert!(!path.exists(), "cancelled request left its MCP file behind");
}
