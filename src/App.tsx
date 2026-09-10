import { useEffect, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import "./styles.css";
import "./shell.css";
import { api, AppConfig } from "./api";
import { Icon, IconName } from "./components/Icons";
import ProvidersPage from "./pages/Providers";
import SessionsPage from "./pages/Sessions";
import StatsPage from "./pages/Stats";
import SettingsPage from "./pages/Settings";

const NAVIGATION = [
  { id: "providers", label: "供应商", description: "管理上游、模型映射与路由优先级。", icon: "providers" },
  { id: "sessions", label: "会话上下文", description: "查看持久化消息、摘要与路由续接信息。", icon: "sessions" },
  { id: "stats", label: "用量与审计", description: "核对请求量、降级过程与实际路由记录。", icon: "activity" },
  { id: "settings", label: "设置", description: "配置监听边界、访问控制与网关行为。", icon: "settings" },
] as const;

type TabId = (typeof NAVIGATION)[number]["id"];
type Theme = "light" | "dark";

const THEME_STORAGE_KEY = "llm-gateway-theme";
const NAVIGATION_STORAGE_KEY = "llm-gateway-navigation";

function isTabId(value: string | null): value is TabId {
  return NAVIGATION.some((item) => item.id === value);
}

function readTheme(): Theme {
  try {
    return window.localStorage.getItem(THEME_STORAGE_KEY) === "dark" ? "dark" : "light";
  } catch {
    return "light";
  }
}

function readTab(): TabId {
  try {
    const saved = window.localStorage.getItem(NAVIGATION_STORAGE_KEY);
    return isTabId(saved) ? saved : "providers";
  } catch {
    return "providers";
  }
}

function configuredEndpoint(config: AppConfig) {
  const bind = config.bind.includes(":") && !config.bind.startsWith("[") ? `[${config.bind}]` : config.bind;
  return `http://${bind}:${config.port}`;
}

async function copyText(value: string) {
  if (navigator.clipboard?.writeText && window.isSecureContext) {
    await navigator.clipboard.writeText(value);
    return;
  }

  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) throw new Error("clipboard unavailable");
}

export default function App() {
  const [tab, setTab] = useState<TabId>(readTab);
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [configState, setConfigState] = useState<"loading" | "ready" | "unavailable" | "preview">("loading");
  const [copyFeedback, setCopyFeedback] = useState<"copied" | "failed" | null>(null);
  const desktopRuntime = isTauri();
  const currentPage = NAVIGATION.find((item) => item.id === tab) ?? NAVIGATION[0];
  const endpoint = config ? configuredEndpoint(config) : null;

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, theme);
    } catch {
      // 受限 WebView 或隐私模式下仍可在当前会话切换主题。
    }
  }, [theme]);

  useEffect(() => {
    try {
      window.localStorage.setItem(NAVIGATION_STORAGE_KEY, tab);
    } catch {
      // 导航状态只影响当前界面，不影响网关配置。
    }
  }, [tab]);

  useEffect(() => {
    if (!desktopRuntime) {
      setConfigState("preview");
      return;
    }

    let disposed = false;
    setConfigState("loading");
    void api.getConfig()
      .then((next) => {
        if (disposed) return;
        setConfig(next);
        setConfigState("ready");
      })
      .catch(() => {
        if (disposed) return;
        setConfig(null);
        setConfigState("unavailable");
      });

    const onConfigChanged = (event: Event) => {
      const next = (event as CustomEvent<AppConfig>).detail;
      if (!next) return;
      setConfig(next);
      setConfigState("ready");
    };
    window.addEventListener("llm-gateway-config-changed", onConfigChanged);
    return () => {
      disposed = true;
      window.removeEventListener("llm-gateway-config-changed", onConfigChanged);
    };
  }, [desktopRuntime]);

  useEffect(() => {
    if (!copyFeedback) return;
    const timer = window.setTimeout(() => setCopyFeedback(null), 2200);
    return () => window.clearTimeout(timer);
  }, [copyFeedback]);

  const copyEndpoint = async () => {
    if (!endpoint) return;
    try {
      await copyText(endpoint);
      setCopyFeedback("copied");
    } catch {
      setCopyFeedback("failed");
    }
  };

  return (
    <div className="app" data-runtime={desktopRuntime ? "desktop" : "preview"}>
      <aside className="sidebar" aria-label="LLM Gateway 导航与本地配置">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">LG</span>
          <span>
            <strong>LLM Gateway</strong>
            <small>统一网关 · 本地优先</small>
          </span>
        </div>

        <nav className="nav-list" aria-label="主导航">
          {NAVIGATION.map((item) => {
            const active = tab === item.id;
            return (
              <button type="button" key={item.id} className={`nav ${active ? "active" : ""}`} aria-current={active ? "page" : undefined} onClick={() => setTab(item.id)}>
                <Icon name={item.icon as IconName} size={17} />
                <span>{item.label}</span>
              </button>
            );
          })}
        </nav>

        <div className="sidebar-spacer" />

        <div className="sidebar-footer">
          <div className="local-priority">
            <Icon name="shield" size={17} />
            <div>
              <strong>本地优先</strong>
              <span>{desktopRuntime ? "管理面仅在本机使用" : "预览不会调用本机数据"}</span>
            </div>
          </div>

          <section className="endpoint-card" aria-label="网关配置端点">
            <div className="endpoint-label">网关配置端点</div>
            <div className="endpoint-value">
              <code>{endpoint ?? (configState === "loading" ? "正在读取配置" : configState === "unavailable" ? "配置暂不可读" : "桌面运行时读取")}</code>
              <button type="button" className="shell-icon-button" onClick={() => void copyEndpoint()} disabled={!endpoint} aria-label="复制网关配置端点" title={endpoint ? "复制网关配置端点" : "读取配置后可复制"}>
                <Icon name={copyFeedback === "copied" ? "check" : "copy"} size={16} />
              </button>
            </div>
            <span className="endpoint-hint">
              {copyFeedback === "copied" ? "已复制到剪贴板" : copyFeedback === "failed" ? "复制失败，请手动复制" : config?.allow_lan ? "局域网监听已配置，重启后生效" : "仅显示配置，未检测服务状态"}
            </span>
          </section>
        </div>
      </aside>

      <main className="app-main">
        <header className="workspace-header">
          <div className="workspace-heading">
            <div className="workspace-eyebrow"><Icon name={currentPage.icon as IconName} size={15} />网关工作区</div>
            <h1>{currentPage.label}</h1>
            <p>{currentPage.description}</p>
          </div>
          <div className="workspace-actions">
            <span className={`runtime-indicator ${desktopRuntime ? "desktop" : "preview"}`}><Icon name={desktopRuntime ? "monitor" : "shield"} size={15} />{desktopRuntime ? "桌面应用" : "预览隔离"}</span>
            <button type="button" className="theme-toggle" onClick={() => setTheme((current) => current === "light" ? "dark" : "light")} aria-label={theme === "light" ? "切换到深色主题" : "切换到浅色主题"} aria-pressed={theme === "dark"} title={theme === "light" ? "切换到深色主题" : "切换到浅色主题"}>
              <Icon name={theme === "light" ? "moon" : "sun"} size={18} />
            </button>
          </div>
        </header>

        <section className="workspace-content" aria-labelledby="workspace-page-title">
          <span id="workspace-page-title" className="sr-only">{currentPage.label}</span>
          {!desktopRuntime ? (
            <section className="runtime-preview" aria-live="polite">
              <div className="preview-icon"><Icon name="monitor" size={26} /></div>
              <h2>浏览器预览已隔离</h2>
              <p>供应商、会话、统计和设置仅在桌面应用中访问本机数据。</p>
              <p className="muted">当前预览不会读取、修改或模拟任何网关数据。</p>
            </section>
          ) : (
            <>
              {tab === "providers" && <ProvidersPage />}
              {tab === "sessions" && <SessionsPage />}
              {tab === "stats" && <StatsPage />}
              {tab === "settings" && <SettingsPage />}
            </>
          )}
        </section>
      </main>
    </div>
  );
}
