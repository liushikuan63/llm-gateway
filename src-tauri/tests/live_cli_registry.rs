//! npm registry 的只读烟测：验证「检查更新」依赖的公开接口仍然可用且结构未变。
//! 默认 ignored，只有显式执行时才访问网络；不安装、不修改任何东西。

use llm_gateway_lib::cli_tools::{latest_version, TOOLS};

#[tokio::test]
#[ignore]
async fn npm_registry_reports_a_version_for_every_supported_cli() {
    for spec in TOOLS {
        let version = latest_version(spec.npm_package, None)
            .await
            .unwrap_or_else(|error| panic!("{} 查询失败：{error}", spec.npm_package));
        let core = version.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        assert!(
            parts.len() >= 3
                && parts
                    .iter()
                    .all(|part| part.chars().all(|c| c.is_ascii_digit())),
            "{} 返回的版本号不可识别：{version}",
            spec.npm_package
        );
        println!("{} -> {version}", spec.npm_package);
    }
}
