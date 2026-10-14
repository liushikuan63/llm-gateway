# 本机 WorkBuddy / Trae 桥接维护源

本目录纳入原先仅存在于 `D:/Software/workbuddy2api` 的本机定制桥接代码，供版本管理和离线回归。桥接使用 Node.js 内置 HTTP、WebSocket 和 Windows PowerShell/WMI；不新增 npm 依赖。Node.js 需支持全局 `WebSocket`，本轮实测为 24.19.0。

软件内集成入口已接入：`src-tauri/src/client_bridges.rs` 将三个生产 JS 文件直接编译进程序，安装包携带固定 Node v24.19.0，客户端接入页可设置端口、启动客户端及启停自己的桥接。普通用户按 [桌面快速开始](../../docs/桌面快速开始.md) 操作，不需要部署此维护目录。软件管理的桥接随软件退出，外部部署继续使用下述独立管理器；两者的进程归属和生命周期分开。

运行中的部署位置仍是 `D:/Software/workbuddy2api`。维护本目录后，将对应文件同步到该目录，核对文件摘要，再通过 `bridge-ctl.js` 重启已核实归属的桥接；不要覆盖该目录中的 `.env`、凭据或其它文件。测试入口：

```powershell
node --test scripts/local-client-bridges/bridge-contract.test.js
npm run verify:clients
```

WorkBuddy 默认端口 8791 / CDP 9224；Trae 默认端口 8792 / CDP 9226。两家必须已经在客户端本人登录，并带远程调试端口启动。`/health` 会只读检查实际渲染进程 API，不能把任意 CDP 页面或静态模型表当成模型调用通过。

Trae 只暴露 `trae-desktop`，使用客户端当前选中的模型。它不实现指定模型切换、完整历史重放、工具或多模态协议；同时来的第二个生成请求返回 409。WorkBuddy 向自身主进程 HTTP 代理提交完整文本消息，拒绝空或错误 SSE，以及缺少终止帧的响应。两家 HTTP 请求体上限为 1 MiB。

`bridge-ctl.js` 使用 WMI 启动进程，避免继承调用宿主的 Job Object；停止前通过 `bridge-process.ps1` 核对实际可执行文件、完整脚本路径与端口，遇到其它监听者拒绝操作。Qoder 使用独立的 `D:/Software/qoder2api-hub` 实现和管理入口，不在本目录复制第三方网关、账号数据或登录凭据。

完整真机验收与限制见 [接入记录](../../docs/local-clients-dsh-integration.md)。原客户端内部 API 没有稳定兼容保证；客户端升级后需重新跑只读检查和已授权的最小请求。
