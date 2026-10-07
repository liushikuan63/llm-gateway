//! B8 验收：外部适配器协议的**描述文件解析与两条负向校验**。
//!
//! 卡片把两条负向实验写成了判据（白名单外拒绝、`args` 含注入字符拒绝），
//! 它们的共同前提是：**描述文件是外部输入，而它会变成命令行**。
//! 「我们不这么用」不是防护 —— 这两条就是防护。

use std::fs;
use std::path::PathBuf;

use llm_gateway_lib::agent_upstream::plugin::{
    parse_manifest, validate_manifest, PluginManifest, SUPPORTED_PROTOCOL,
};

/// 一个能用的最小描述文件。
fn 描述文件(id: &str) -> String {
    serde_json::json!({
        "id": id,
        "kind": "external",
        "exe": "ref-plugin.exe",
        "args": ["--stdio"],
        "protocol": SUPPORTED_PROTOCOL,
    })
    .to_string()
}

/// 建一个临时目录，里面放一份描述文件。返回 `(目录, 描述文件路径)`。
///
/// **目录名必须用进程内的原子计数器**，不能用时间戳：cargo 默认让同一个
/// 测试文件里的用例**并行**跑，而两个用例在同一纳秒取时间戳是完全可能的 ——
/// 那时它们共享目录，一个用例收尾的 `remove_dir_all` 会把另一个的文件删掉，
/// 症状是毫不相干的断言报「系统找不到指定的文件」。实测踩到过一次。
fn 落地一份描述文件(内容: &str, 名字: &str) -> (PathBuf, PathBuf) {
    static 序号: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = 序号.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("llm-gateway-plugin-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join(名字);
    fs::write(&path, 内容).expect("写描述文件");
    (dir, path)
}

#[test]
fn 合法描述文件在白名单内可以通过() {
    let (dir, path) = 落地一份描述文件(&描述文件("ref"), "plugin.json");
    let manifest = parse_manifest(&描述文件("ref")).expect("应当解析成功");
    assert_eq!(manifest.id, "ref");
    assert_eq!(manifest.args, vec!["--stdio".to_string()]);
    validate_manifest(&manifest, &path, std::slice::from_ref(&dir)).expect("白名单内应当通过");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn 缺字段与不支持的协议被拒() {
    // id 为空
    let err = parse_manifest(
        &serde_json::json!({"id": "  ", "kind": "k", "exe": "e", "protocol": SUPPORTED_PROTOCOL})
            .to_string(),
    )
    .unwrap_err();
    assert!(err.contains("id"), "错误要指向具体字段：{err}");

    // exe 为空
    let err = parse_manifest(
        &serde_json::json!({"id": "a", "kind": "k", "exe": "", "protocol": SUPPORTED_PROTOCOL})
            .to_string(),
    )
    .unwrap_err();
    assert!(err.contains("exe"), "{err}");

    // 协议不认识：**白名单比较**，`jsonl-v1evil` 不能因为前缀相同就放行
    for bad in ["jsonl-v2", "jsonl-v1evil", "JSONL-V1", ""] {
        let err = parse_manifest(
            &serde_json::json!({"id": "a", "kind": "k", "exe": "e", "protocol": bad}).to_string(),
        )
        .unwrap_err();
        assert!(err.contains("协议"), "protocol={bad:?} 应当被拒：{err}");
    }
}

/// **卡片判据 2**：描述文件放在白名单外 → 拒绝，且**报错含路径**。
///
/// 「含路径」不是装饰：只说「不在白名单里」的话，用户分不清是自己填错了目录
/// 还是白名单没配 —— 而这两种情况要做的事完全不同。
#[test]
fn 白名单外的描述文件被拒且报错含路径() {
    let (outside, path) = 落地一份描述文件(&描述文件("ref"), "plugin.json");
    let (allowed, _) = 落地一份描述文件("不是它", "other.json");

    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    let err = validate_manifest(&manifest, &path, std::slice::from_ref(&allowed)).unwrap_err();
    assert!(
        err.contains(&path.canonicalize().unwrap().display().to_string()),
        "报错必须含被拒的路径：{err}"
    );
    assert!(err.contains("允许的目录"), "也要说清允许哪些：{err}");

    // 对照组：同一个文件放进白名单里就该通过 —— 否则上面那条可能只是因为
    // 校验函数永远返回 Err。
    validate_manifest(&manifest, &path, std::slice::from_ref(&outside))
        .expect("在白名单里应当通过");

    // 白名单为空 ⇒ 一律拒绝（默认状态不能是「什么都能加载」）
    assert!(validate_manifest(&manifest, &path, &[]).is_err());

    let _ = fs::remove_dir_all(outside);
    let _ = fs::remove_dir_all(allowed);
}

/// **卡片判据 3**：`args` 里出现会被 shell 解释的字符 → 拒绝启动。
///
/// 卡片的原话是「必须在**参数校验**处拦，不能靠『我们不这么用』」。
/// 这条覆盖 `exe` 与 `args` 两处：`exe` 是 `.cmd` 时 Windows 会走 `cmd.exe`，
/// 那时这些字符真的会被解释。
#[test]
fn 含注入字符的_exe_与_args_被拒() {
    let (dir, path) = 落地一份描述文件(&描述文件("ref"), "plugin.json");
    let 合法 = parse_manifest(&描述文件("ref")).unwrap();

    for 恶意 in [
        "a; rm -rf /",
        "a && calc",
        "a | more",
        "a`whoami`",
        "a$(id)",
        "a\nb",
    ] {
        // args 里
        let mut manifest: PluginManifest = 合法.clone();
        manifest.args = vec![恶意.to_string()];
        let err = validate_manifest(&manifest, &path, std::slice::from_ref(&dir)).unwrap_err();
        assert!(
            err.contains("args[0]"),
            "报错要点名是哪一个参数：{恶意:?} ⇒ {err}"
        );

        // exe 里
        let mut manifest: PluginManifest = 合法.clone();
        manifest.exe = 恶意.to_string();
        let err = validate_manifest(&manifest, &path, std::slice::from_ref(&dir)).unwrap_err();
        assert!(err.contains("exe"), "报错要点名 exe：{恶意:?} ⇒ {err}");
    }

    // 对照组：干净的参数必须通过 —— 否则上面那批可能只是因为校验永远失败。
    validate_manifest(&合法, &path, std::slice::from_ref(&dir)).expect("干净参数应当通过");
    let _ = fs::remove_dir_all(dir);
}

/// 描述文件路径不存在时报错，而不是 panic。
#[test]
fn 描述文件路径不存在时报可读错误() {
    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    let err = validate_manifest(
        &manifest,
        PathBuf::from("C:/definitely/not/here/plugin.json").as_path(),
        &[std::env::temp_dir()],
    )
    .unwrap_err();
    assert!(err.contains("无法解析"), "{err}");
}
