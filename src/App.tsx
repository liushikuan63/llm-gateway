import { useEffect, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import "./styles.css";
import { api, AppConfig } from "./api";
import ProvidersPage from "./pages/Providers";
import SessionsPage from "./pages/Sessions";
import StatsPage from "./pages/Stats";
import SettingsPage from "./pages/Settings";

const TABS = [
  { id: "providers", label: "供应商" },
  { id: "sessions", label: "会话上下文" },
  { id: "stats", label: "用量与审计" },
  { id: "settings", label: "设置" },
] as const;

export default function App() {
  const [tab, setTab] = useState<(typeof TABS)[number]["id"]>("providers");
  const [config, setConfig] = useState<AppConfig | null>(null);
  const desktopRuntime = isTauri();

  useEffect(() => {
    if (!desktopRuntime) return;
    void api.getConfig().then(setConfig).catch(() => setConfig(null));
    const onConfigChanged = (event: Event) => {
      const next = (event as CustomEvent<AppConfig>).detail;
      if (next) setConfig(next);
    };
    window.addEventListener("llm-gateway-config-changed", onConfigChanged);
    return () => window.removeEventListener("llm-gateway-config-changed", onConfigChanged);
  }, [desktopRuntime]);

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          LLM Gateway
          <small>统一网关 · 本地优先</small>
        </div>
        {TABS.map((t) => (
          <button
            key={t.id}
            className={`nav ${tab === t.id ? "active" : ""}`}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
        <div style={{ flex: 1 }} />
        <div className="muted" style={{ fontSize: 11, padding: "0 10px", lineHeight: 1.6 }}>
          关闭窗口后仍常驻托盘
          <br />
          {desktopRuntime
            ? config
              ? `配置监听 ${config.bind}:${config.port}`
              : "正在读取网关配置"
            : "浏览器预览模式"}
          {config?.allow_lan && <><br />局域网访问已配置，重启后生效</>}
        </div>
      </aside>
      <main className="main">
        {!desktopRuntime ? (
          <section className="runtime-preview" aria-live="polite">
            <h2>浏览器预览</h2>
            <p>供应商、会话、统计和设置由 LLM Gateway 桌面应用中的本地 IPC 管理。</p>
            <p className="muted">请从 Windows 安装包启动应用以连接和管理网关。</p>
          </section>
        ) : (
          <>
            {tab === "providers" && <ProvidersPage />}
            {tab === "sessions" && <SessionsPage />}
            {tab === "stats" && <StatsPage />}
            {tab === "settings" && <SettingsPage />}
          </>
        )}
      </main>
    </div>
  );
}
