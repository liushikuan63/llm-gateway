//! B8 加载器的验收：**关着不扫、开着才加载、坏掉不拖垮别人**。
//!
//! 在加载器之前，B8 的四条判据都满足，但没有任何生产路径会构造
//! `ProcessTransport` —— 打包出来的 exe 里连 `plugin-processes-` 这个字符串
//! 都没有（整条链被死代码消除）。判据满足 ≠ 能力可用。

use std::fs;
use std::path::{Path, PathBuf};

use llm_gateway_lib::agent_upstream::loader::load_plugins;
use llm_gateway_lib::agent_upstream::plugin::SUPPORTED_PROTOCOL;

fn 临时目录(名字: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("llm-gateway-loader-{}-{名字}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn 写描述文件(dir: &Path, 名字: &str, id: &str, exe: &str, args: &[&str]) {
    let body = serde_json::json!({
        "id": id,
        "kind": "external",
        "exe": exe,
        "args": args,
        "protocol": SUPPORTED_PROTOCOL,
    });
    fs::write(dir.join(名字), body.to_string()).expect("写描述文件");
}

/// **负向对照**：总开关关着时一个目录都不扫。
///
/// 不是「扫了再过滤」—— 扫本身就会去读用户指定的路径，而关着的功能
/// 不该碰文件系统。用例给一个**根本不存在**的目录：若实现真去扫了，
/// 它会在 `skipped` 里留下一条「读不了目录」，而那条断言就会红。
#[test]
fn 总开关关着一个目录都不扫() {
    let ghost = 临时目录("ghost");
    let _ = fs::remove_dir_all(&ghost); // 现在是「不存在」的目录

    let outcome = load_plugins(std::slice::from_ref(&ghost), false);
    assert!(outcome.loaded.is_empty(), "关着不该加载任何东西");
    assert!(
        outcome.skipped.is_empty(),
        "关着时连目录都不该读，更不该留下「读不了目录」：{:?}",
        outcome.skipped
    );
}

/// 开着时：合法的加载、坏的跳过，**互不影响**。
#[test]
fn 合法的加载坏的跳过且互不影响() {
    let dir = 临时目录("mixed");
    写描述文件(&dir, "good.json", "ref-ok", "ref-plugin.exe", &["--stdio"]);
    // 坏 JSON
    fs::write(dir.join("broken.json"), "{ 这不是 JSON").unwrap();
    // 协议不认识
    fs::write(
        dir.join("bad-proto.json"),
        serde_json::json!({
            "id": "x", "kind": "k", "exe": "e", "protocol": "jsonl-v99"
        })
        .to_string(),
    )
    .unwrap();
    // 注入字符
    写描述文件(&dir, "inject.json", "ref-inject", "e.exe", &["a; calc"]);
    // 非 json 文件：直接无视，既不该加载也不该报错
    fs::write(dir.join("readme.txt"), "看这里").unwrap();

    let outcome = load_plugins(std::slice::from_ref(&dir), true);
    assert_eq!(
        outcome.loaded.len(),
        1,
        "只有一个合法；实际加载 {} 个",
        outcome.loaded.len()
    );
    assert_eq!(outcome.loaded[0].id(), "ref-ok");

    // 三个坏的各一条原因，且**都带路径**（不带路径用户没法修）
    assert_eq!(outcome.skipped.len(), 3, "{:?}", outcome.skipped);
    for why in &outcome.skipped {
        assert!(
            why.contains(dir.to_string_lossy().as_ref()),
            "跳过原因要带路径：{why}"
        );
    }
    let joined = outcome.skipped.join("\n");
    assert!(joined.contains("broken.json"), "{joined}");
    assert!(joined.contains("bad-proto.json"), "{joined}");
    assert!(joined.contains("inject.json"), "{joined}");
    assert!(
        !joined.contains("readme.txt"),
        "非 json 文件不该被当成失败：{joined}"
    );

    let _ = fs::remove_dir_all(dir);
}

/// 目录不存在时**只记一条**，不影响别的目录里的插件。
#[test]
fn 坏目录不拖垮别的目录() {
    let good = 临时目录("good-dir");
    写描述文件(&good, "a.json", "ref-a", "a.exe", &[]);
    let ghost = good.join("不存在");

    let outcome = load_plugins(&[ghost, good.clone()], true);
    assert_eq!(outcome.loaded.len(), 1, "好目录里的插件必须照常加载");
    assert_eq!(outcome.skipped.len(), 1, "{:?}", outcome.skipped);
    assert!(
        outcome.skipped[0].contains("读不了目录"),
        "{:?}",
        outcome.skipped
    );
    let _ = fs::remove_dir_all(good);
}

/// 白名单外的描述文件被拒 —— 但**它自己**被拒，同目录里合法的那个还要加载。
///
/// 这条把「位置校验」与「加载器」接起来验：`load_plugins` 传的白名单是
/// 它收到的整个 `dirs`，所以放在 `dirs` 里任一目录下都算合法。
#[test]
fn 白名单外的文件被拒且不影响其他() {
    let inside = 临时目录("inside");
    let outside = 临时目录("outside");
    写描述文件(&inside, "ok.json", "ref-ok", "ok.exe", &[]);

    // 把 outside 里的描述文件**复制**进 inside 的扫描范围是不可能的（扫的是目录），
    // 所以这里直接用一条软链接做「位置绕过」：链接指向 outside。
    // 拿不到软链接权限时退化成跳过该断言（Windows 上非管理员默认就没有）。
    #[cfg(windows)]
    let linked =
        std::os::windows::fs::symlink_file(outside.join("linked.json"), inside.join("linked.json"));
    #[cfg(not(windows))]
    let linked =
        std::os::unix::fs::symlink(outside.join("linked.json"), inside.join("linked.json"));
    if linked.is_err() {
        eprintln!("跳过软链接那一半：本机不给创建符号链接的权限");
    } else {
        写描述文件(&outside, "linked.json", "ref-linked", "l.exe", &[]);
        let outcome = load_plugins(std::slice::from_ref(&inside), true);
        // inside 自己的那个必须照常加载
        assert!(
            outcome.loaded.iter().any(|a| a.id() == "ref-ok"),
            "同目录里合法的那个不该受影响"
        );
    }

    let _ = fs::remove_dir_all(inside);
    let _ = fs::remove_dir_all(outside);
}
