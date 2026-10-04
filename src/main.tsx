import React from "react";
import { createRoot } from "react-dom/client";

type BootState = {
  status: "loading" | "ready" | "error";
  error: string | null;
  /** 非致命降级提示：配置坏掉时后端仍会起来，但原配置没生效。 */
  warning?: string | null;
};

type TauriRuntimeWindow = Window & {
  isTauri?: boolean;
  __TAURI_INTERNALS__?: {
    invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  };
};

const runtimeWindow = window as TauriRuntimeWindow;
const isTauriRuntime = () => runtimeWindow.isTauri === true;

const BOOT_MESSAGES = [
  "正在读取本地配置…",
  "正在打开 SQLite 数据库…",
  "正在恢复会话上下文…",
  "正在准备模型路由…",
  "正在启动本地网关服务…",
];
const BOOT_TIMEOUT_MS = 30_000;
const rootElement = document.getElementById("root");
if (!rootElement) throw new Error("missing #root");
const root = createRoot(rootElement);

function bootStatusElement() {
  return document.querySelector<HTMLElement>("[data-boot-status]");
}

function bootHintElement() {
  return document.querySelector<HTMLElement>("[data-boot-hint]");
}

function setBootMessage(message: string) {
  const status = bootStatusElement();
  if (status) status.textContent = message;
}

function hideBootSplash() {
  const splash = document.getElementById("boot-splash");
  if (!splash) return;
  splash.classList.add("boot-splash-hidden");
  window.setTimeout(() => splash.remove(), 420);
}

function showBootError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  const status = bootStatusElement();
  const hint = bootHintElement();
  const detail = document.querySelector<HTMLElement>("[data-boot-error]");
  if (status) status.textContent = "启动失败";
  if (hint) hint.textContent = "无法完成后端初始化";
  if (detail) {
    detail.hidden = false;
    detail.textContent = `${message}\n请查看应用日志或重启应用。`;
  }
}

/**
 * 等后端就绪，并**把非致命的降级提示带出来**。
 *
 * 配置读坏了后端会用默认值启动（status 仍是 ready），但用户必须知道
 * 自己的设置没生效 —— 不说的话，界面上一切正常，而排查时毫无线索。
 */
async function waitForBackend() {
  const deadline = Date.now() + BOOT_TIMEOUT_MS;
  for (;;) {
    if (Date.now() > deadline) throw new Error("后端初始化超时");
    const invoke = runtimeWindow.__TAURI_INTERNALS__?.invoke;
    if (!invoke) throw new Error("Tauri IPC 不可用");
    const state = await invoke<BootState>("get_boot_state");
    if (state.status === "ready") return state.warning ?? null;
    if (state.status === "error") throw new Error(state.error ?? "后端初始化失败");
    await new Promise((resolve) => window.setTimeout(resolve, 150));
  }
}

async function start() {
  // 提前加载主界面代码，但先不挂载；它与后端初始化并行进行。
  const appModule = import("./App");
  let messageIndex = 0;
  setBootMessage(BOOT_MESSAGES[messageIndex]);
  const messageTimer = window.setInterval(() => {
    messageIndex = (messageIndex + 1) % BOOT_MESSAGES.length;
    setBootMessage(BOOT_MESSAGES[messageIndex]);
  }, 1200);

  try {
    const bootWarning = isTauriRuntime() ? await waitForBackend() : null;
    const [{ default: App }] = await Promise.all([appModule]);
    window.clearInterval(messageTimer);
    setBootMessage("正在绘制界面…");
    root.render(
      <React.StrictMode>
        <App bootWarning={bootWarning} />
      </React.StrictMode>
    );
    window.requestAnimationFrame(() => {
      window.requestAnimationFrame(hideBootSplash);
    });
  } catch (error) {
    window.clearInterval(messageTimer);
    showBootError(error);
  }
}

void start();