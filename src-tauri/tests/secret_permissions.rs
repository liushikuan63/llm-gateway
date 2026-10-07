//! B7 判据 3：凭据材料的**文件权限复核**。
//!
//! ## 本项目的凭据落在哪
//!
//! 不在明文文件里 —— 搜索 API Key 之类存在 SQLite 的 `app_secrets` 表，
//! 是 AES-256-GCM 密文。但**主密钥 `master.key` 是落盘的**（DPAPI 信封 + EFS），
//! 所以它才是这一条判据真正的对象：**谁能读它，谁就能解开库里那些密文**。
//! 只盯着「有没有明文凭据文件」会漏掉这一层。
//!
//! ## 为什么用 `icacls` 而不是 winapi
//!
//! 本仓有「不许加依赖」的硬约束，而 `icacls` 是系统自带命令。
//! 它的输出是人读的，所以下面的解析器**必须自己能被测**（见末尾那条用例）——
//! 一个解析错了的 ACL 检查比没有检查更糟：它会安静地放行一切。
//!
//! ## 实测底数（2026-10-08，本机）
//!
//! `master.key` 与它所在目录的 ACL 都是：
//! `NT AUTHORITY\SYSTEM` / `BUILTIN\Administrators` / 当前用户 ——
//! 继承自 `%LOCALAPPDATA%` 的默认 ACL，**没有** Everyone / Users。
//! 也就是说这条判据在当前机器上是**通过**的，而不是「没测过」。

use std::path::Path;

/// 宽松主体：出现在授权列表里就说明这个文件不该被信任。
///
/// 同时收名字与 SID：`icacls` 在不同语言 / 域环境下会输出其中一种，
/// 只认名字的话，一个本地化系统上 "Everyone" 可能写作 "所有人"。
const BROAD_PRINCIPALS: [&str; 6] = [
    "Everyone",
    "BUILTIN\\Users",
    "Authenticated Users",
    // 对应的 SID —— 名字被本地化时靠它们兜底
    "S-1-1-0",      // Everyone
    "S-1-5-11",     // Authenticated Users
    "S-1-5-32-545", // BUILTIN\Users
];

/// 从 `icacls` 的输出里取出被授权的主体名。
///
/// 输出形如（第一行带路径，后续行对齐到同一列）：
/// ```text
/// C:\...\master.key NT AUTHORITY\SYSTEM:(I)(F)
///                   BUILTIN\Administrators:(I)(F)
/// ```
/// 所以：先削掉第一行里的路径，再取 `:(` 之前的部分。
pub fn parse_principals(output: &str, path: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // 末行是 "Successfully processed N files; ..."
        .filter(|line| !line.starts_with("Successfully"))
        .filter_map(|line| {
            let body = line.strip_prefix(path).unwrap_or(line).trim();
            let name = body.split(":(").next()?.trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// 某个主体是不是宽松主体。
pub fn is_broad(principal: &str) -> bool {
    BROAD_PRINCIPALS
        .iter()
        .any(|broad| principal.eq_ignore_ascii_case(broad) || principal.contains(broad))
}

/// 读一个路径的 ACL（`icacls`）。路径不存在时返回 `None`。
fn acl_of(path: &Path) -> Option<String> {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// **解析器本身必须被测**：它错了的话，整条 ACL 检查会安静地放行一切。
#[test]
fn 解析器能从_icacls_输出里取出主体() {
    let sample = "C:\\Users\\Admin\\AppData\\Local\\llm-gateway\\master.key NT AUTHORITY\\SYSTEM:(I)(F)\n\
                  \x20                                                                BUILTIN\\Administrators:(I)(F)\n\
                  \x20                                                                DESKTOP-X\\Admin:(I)(F)\n\
                  Successfully processed 1 files; Failed processing 0 files\n";
    let principals = parse_principals(
        sample,
        "C:\\Users\\Admin\\AppData\\Local\\llm-gateway\\master.key",
    );
    assert_eq!(
        principals,
        vec![
            "NT AUTHORITY\\SYSTEM".to_string(),
            "BUILTIN\\Administrators".to_string(),
            "DESKTOP-X\\Admin".to_string(),
        ],
        "主体名必须逐条取出来，且不带权限括号"
    );
    // 末行不是主体
    assert!(!principals.iter().any(|p| p.starts_with("Successfully")));
}

/// 宽泛主体的识别：名字与 SID 两条路都要认。
#[test]
fn 宽泛主体按名字与_sid_都能认出来() {
    for broad in [
        "Everyone",
        "BUILTIN\\Users",
        "Authenticated Users",
        "*S-1-1-0",
        "S-1-5-32-545",
    ] {
        assert!(is_broad(broad), "{broad} 应当被判成宽泛主体");
    }
    for narrow in [
        "NT AUTHORITY\\SYSTEM",
        "BUILTIN\\Administrators",
        "DESKTOP-3K8GT9X\\Admin",
    ] {
        assert!(!is_broad(narrow), "{narrow} 不该被判成宽泛主体");
    }
}

/// 反例组：一段**含 Everyone** 的 ACL 必须被判成不合格。
///
/// 少了这条，`is_broad` 永远返回 false 也能让上面那条通过。
#[test]
fn 含_everyone_的_acl_会被判不合格() {
    let bad = "C:\\tmp\\master.key Everyone:(F)\n\
               \x20                NT AUTHORITY\\SYSTEM:(F)\n\
               Successfully processed 1 files; Failed processing 0 files\n";
    let principals = parse_principals(bad, "C:\\tmp\\master.key");
    assert!(
        principals.iter().any(|p| is_broad(p)),
        "含 Everyone 的 ACL 必须被认出来：{principals:?}"
    );
}

/// **判据 3 本体**：主密钥文件的 ACL 不含宽泛主体。
///
/// 失败时给出可直接粘贴的收紧命令 —— 只报「不合格」而不给修法，
/// 用户下一步只能去搜「icacls 怎么改权限」。
#[test]
fn 主密钥文件的_acl_不含宽泛主体() {
    // 触发一次加密，确保密钥文件已经存在（新装的机器上它可能还没建）。
    let _ = llm_gateway_lib::crypto::encrypt("acl-probe");

    let path = llm_gateway_lib::config::app_data_dir().join("master.key");
    let Some(output) = acl_of(&path) else {
        // 拿不到 ACL（路径不存在 / icacls 不可用）时**不能静默通过** ——
        // 那正是「跳过被记成通过」的老毛病。
        panic!(
            "读不到主密钥文件的 ACL：{}。\
             判据 3 要求复核它，读不到就不算验过。",
            path.display()
        );
    };
    let principals = parse_principals(&output, &path.to_string_lossy());
    assert!(
        !principals.is_empty(),
        "ACL 解析出 0 个主体，解析器可能坏了。原始输出：\n{output}"
    );
    let broad: Vec<&String> = principals.iter().filter(|p| is_broad(p)).collect();
    assert!(
        broad.is_empty(),
        "主密钥文件对所有用户可读，等于把库里那些密文一起交出去。\n\
         实际主体：{principals:?}\n\
         收紧命令（按需替换用户名）：\n\
         \x20 icacls \"{}\" /inheritance:r /grant:r \"%USERNAME%:(F)\" /grant:r \"SYSTEM:(F)\"",
        path.display()
    );
}

/// 同一条判据也适用于**网关数据库**：`app_secrets` 表就在里面。
#[test]
fn 数据库文件的_acl_不含宽泛主体() {
    let path = llm_gateway_lib::config::app_data_dir().join("gateway.db");
    if !path.exists() {
        // 数据库还没建（全新环境）—— 这里**可以**跳过，因为对象确实不存在；
        // 但它与上面那条不同：主密钥一定会有（任何一次加密都会建它）。
        eprintln!("跳过：{} 还不存在", path.display());
        return;
    }
    let output = acl_of(&path).expect("数据库存在却读不到 ACL");
    let principals = parse_principals(&output, &path.to_string_lossy());
    assert!(!principals.is_empty(), "解析出 0 个主体：\n{output}");
    let broad: Vec<&String> = principals.iter().filter(|p| is_broad(p)).collect();
    assert!(
        broad.is_empty(),
        "数据库对所有用户可读 —— 里面有加密凭据与全部审计记录。\n\
         实际主体：{principals:?}"
    );
}
