//! npm registry 的只读烟测：验证「检查更新」依赖的公开接口仍然可用且结构未变。
//! 默认 ignored，只有显式执行时才访问网络；不安装、不修改任何东西。
//!
//! 覆盖范围 = 清单里所有 npm 来源的工具；官方脚本类工具不做版本比对，
//! 它们的地址由 `cli_tools::tests::tool_catalog_is_unique_and_carries_an_executable_install_command`
//! 约束为内置常量。

use llm_gateway_lib::cli_tools::{latest_version, TOOLS};

#[tokio::test]
#[ignore]
async fn npm_registry_reports_a_version_for_every_npm_backed_cli() {
    let mut checked = 0;
    for spec in TOOLS {
        let Some(package) = spec.source.package() else {
            println!("{} -> 官方脚本安装，跳过 registry 查询", spec.id);
            continue;
        };
        let version = latest_version(package, None)
            .await
            .unwrap_or_else(|error| panic!("{package} 查询失败：{error}"));
        let core = version.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        assert!(
            parts.len() >= 3
                && parts
                    .iter()
                    .all(|part| part.chars().all(|c| c.is_ascii_digit())),
            "{package} 返回的版本号不可识别：{version}"
        );
        checked += 1;
        println!("{package} -> {version}");
    }
    // 0 命中是故障不是结论：清单里必须真的有 npm 来源的工具被查到。
    assert!(
        checked >= 15,
        "只有 {checked} 个 npm 工具被核对，清单可能已损坏"
    );
}
