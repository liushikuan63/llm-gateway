# 安装 VS Build Tools（后端编译的唯一障碍）

**为什么需要**：本机原先没有 Rust，也没有 MSVC 链接器。装完 Rust（windows-gnu）后
链接器问题已解决，但 `tauri` 的 build script 在本机 windows-gnu 工具链下以
`0xc0000005 STATUS_ACCESS_VIOLATION` 崩溃。

已用插桩法确认这不是本项目的问题：

- 把 tauri 源码复制到临时目录、插入文件探针后作为**根包**单独构建，build script **完整跑通**；
- 用 `[patch.crates-io]` 把项目指向这份探针版，仍然崩，且**探针文件根本没被创建**
  ⇒ 崩在 `main()` 之前（进程初始化阶段）；
- 新建一个只依赖 `tauri` 的干净 crate（`src/lib.rs` 只有 `pub fn noop()`），
  同样复现，2.11.5 与 2.12.1 都不行；
- 已排除：栈溢出（`-Wl,--stack,33554432` 经 `llvm-readobj` 确认已生效后仍崩）、
  静态 CRT、dlltool 替换、缺 DLL（导入表等价）。

结论：本机 rustup `windows-gnu` 工具链与 tauri 构建脚本存在环境级不兼容。
Tauri 在 Windows 上的官方支持路径是 MSVC，且 MSVC 需要管理员权限安装。

**现状**：安装器已下载到 `C:\Users\Admin\Downloads\vs_BuildTools.exe`（4.3 MB）。

## 一、在管理员 PowerShell 里执行

按 Win+X → 「终端(管理员)」，粘贴：

```powershell
& "$env:USERPROFILE\Downloads\vs_BuildTools.exe" --quiet --wait --norestart --nocache --installPath "C:\BuildTools" --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --add Microsoft.VisualStudio.Component.VCTools.140.x86.x64
```

- 弹 UAC 时点「是」。
- `--quiet` 表示静默安装，**没有进度条**，看起来像没反应是正常的。
- 下载约 3–5 GB，国内网络可能要 10–40 分钟。
- 结束标志：`C:\BuildTools\VC\Tools\MSVC\` 下出现版本目录。

## 二、装完告诉网关用 MSVC

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc --profile minimal
rustup default stable-x86_64-pc-windows-msvc
```

## 三、验证

```powershell
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'   # 会自动切到 MSVC 并加入 VC 工具链
cargo check --jobs 8
```

判据：退出码 0，末尾是 `Finished`。

## 回滚

- 卸载：`C:\Program Files (x86)\Microsoft Visual Studio\Installer\vs_installer.exe` → 卸载 Visual Studio Build Tools 2022。
- 切回 windows-gnu：`rustup default stable-x86_64-pc-windows-gnu`。

## 本轮已装的东西（都可回滚）

| 位置 | 内容 | 回滚 |
| --- | --- | --- |
| `%USERPROFILE%\.cargo` / `\.rustup` | rustup + stable 工具链（windows-gnu） | 删这两个目录 |
| `%LOCALAPPDATA%\llvm-mingw` | llvm-mingw 20260922（提供可用的 dlltool） | 删该目录 |
| `…\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained\dlltool.exe` | 已从 rust 自带的坏版本换成 llvm-mingw 版 | 原文件已重命名为 `dlltool.exe.rustbak`，改回去即可 |
| `scripts/cargo-env.ps1` | 本机工具链引导脚本 | 删该文件 |