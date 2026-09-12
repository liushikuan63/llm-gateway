import { useEffect, useRef, useState } from "react";
import {
  api,
  AppConfig,
  CliToolReport,
  ConfigUpdateResult,
  CustomRouteRule,
  CustomRouteRuleAction,
  Dialect,
  ProviderView,
  RemoteAccessKeyView,
  SelfCheckResult,
  SnapshotApplyResult,
  SnapshotView,
  TakeoverResult,
} from "../api";
import PetCard from "./PetCard";

type Message = { kind: "ok" | "err"; text: string };

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function restartMessage(result: ConfigUpdateResult | SnapshotApplyResult) {
  if (!result.restart_required) return "已保存并热生效";
  return `已保存。${result.restart_reasons.join("、")}已变更；请从系统托盘退出并重新启动 LLM Gateway 后生效。`;
}

function formatDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function defaultRuleAction(
  type: CustomRouteRuleAction["type"],
  providerId: string,
): CustomRouteRuleAction {
  switch (type) {
    case "only_dialect":
      return { type, dialect: "openai" };
    case "exclude_provider":
      return { type, provider_id: providerId };
    case "boost_provider":
      return { type, provider_id: providerId, bonus: 10 };
  }
}

function newCustomRule(providerId: string): CustomRouteRule {
  return {
    prefix: "",
    action: defaultRuleAction("only_dialect", providerId),
  };
}

export default function SettingsPage() {
  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [keyInfo, setKeyInfo] = useState<Record<string, string> | null>(null);
  const [msg, setMsg] = useState<Message | null>(null);
  const [snaps, setSnaps] = useState<SnapshotView[]>([]);
  const [remoteKeys, setRemoteKeys] = useState<RemoteAccessKeyView[]>([]);
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [customRulesDraft, setCustomRulesDraft] = useState<CustomRouteRule[] | null>(null);
  const [snapName, setSnapName] = useState("");
  const [remoteKeyLabel, setRemoteKeyLabel] = useState("");
  const [remoteKeyRpmLimit, setRemoteKeyRpmLimit] = useState(60);
  const [editingRemoteKey, setEditingRemoteKey] = useState<RemoteAccessKeyView | null>(null);
  const [editingRemoteKeyLabel, setEditingRemoteKeyLabel] = useState("");
  const [editingRemoteKeyRpmLimit, setEditingRemoteKeyRpmLimit] = useState(60);
  const [lanConfirmationOpen, setLanConfirmationOpen] = useState(false);
  const [remoteConfirmationOpen, setRemoteConfirmationOpen] = useState(false);
  const [oneTimeSecretLabel, setOneTimeSecretLabel] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [takeoverResults, setTakeoverResults] = useState<TakeoverResult[]>([]);
  const [cliTools, setCliTools] = useState<CliToolReport[]>([]);
  const [selfCheck, setSelfCheck] = useState<SelfCheckResult | null>(null);
  // 原始远程 Key 不进入 React state，关闭一次性展示窗口后立即清除。
  const oneTimeSecretRef = useRef<string | null>(null);

  const notifyConfigChanged = (config: AppConfig) => {
    window.dispatchEvent(new CustomEvent<AppConfig>("llm-gateway-config-changed", { detail: config }));
  };

  const load = async () => {
    try {
      const [config, key, snapshots, keys, providerList] = await Promise.all([
        api.getConfig(),
        api.getUnifiedKey(),
        api.listSnapshots(),
        api.listRemoteAccessKeys(),
        api.listProviders(),
      ]);
      setCfg(config);
      setKeyInfo(key);
      setSnaps(snapshots);
      setRemoteKeys(keys);
      setProviders(providerList);
      setCustomRulesDraft(null);
    } catch (error) {
      setMsg({ kind: "err", text: `加载设置失败：${errorText(error)}` });
    }
  };

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => () => {
    oneTimeSecretRef.current = null;
  }, []);

  const patch = async (changes: Partial<AppConfig>) => {
    if (!cfg) return;
    const next = { ...cfg, ...changes };
    if (!Number.isInteger(next.port) || next.port < 1 || next.port > 65535) {
      setMsg({ kind: "err", text: "端口必须是 1 到 65535 之间的整数" });
      return;
    }

    setBusy("config");
    try {
      const result = await api.updateConfig(next);
      setCfg(result.config);
      notifyConfigChanged(result.config);
      setKeyInfo(await api.getUnifiedKey());
      setMsg({ kind: "ok", text: restartMessage(result) });
    } catch (error) {
      setMsg({ kind: "err", text: `保存设置失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const saveNumber = (field: keyof AppConfig, value: number) => {
    if (!Number.isFinite(value)) {
      setMsg({ kind: "err", text: "请输入有效数字" });
      return;
    }
    void patch({ [field]: value } as Partial<AppConfig>);
  };

  const closeOneTimeSecret = () => {
    oneTimeSecretRef.current = null;
    setOneTimeSecretLabel(null);
  };

  const createRemoteAccessKey = async () => {
    const label = remoteKeyLabel.trim();
    if (!label) {
      setMsg({ kind: "err", text: "请为远程访问 Key 填写用途或设备名称" });
      return;
    }
    if (!Number.isInteger(remoteKeyRpmLimit) || remoteKeyRpmLimit < 1 || remoteKeyRpmLimit > 100000) {
      setMsg({ kind: "err", text: "每分钟请求上限必须是 1 到 100000 之间的整数" });
      return;
    }

    setBusy("remote-key-create");
    try {
      const result = await api.createRemoteAccessKey({ label, rpm_limit: remoteKeyRpmLimit });
      // 只在内存引用中短暂保留，列表和任何可持久化 state 都不会保存 secret。
      oneTimeSecretRef.current = result.secret;
      setOneTimeSecretLabel(result.key.label);
      setRemoteKeyLabel("");
      setRemoteKeyRpmLimit(60);
      await load();
      setMsg({ kind: "ok", text: `已创建“${result.key.label}”的远程访问 Key，请立即保存一次性 secret。` });
    } catch (error) {
      setMsg({ kind: "err", text: `创建远程访问 Key 失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const updateRemoteAccessKey = async (
    key: RemoteAccessKeyView,
    changes: Pick<RemoteAccessKeyView, "label" | "enabled" | "rpm_limit">,
  ) => {
    if (!changes.label.trim()) {
      setMsg({ kind: "err", text: "访问 Key 名称不能为空" });
      return;
    }
    if (!Number.isInteger(changes.rpm_limit) || changes.rpm_limit < 1 || changes.rpm_limit > 100000) {
      setMsg({ kind: "err", text: "每分钟请求上限必须是 1 到 100000 之间的整数" });
      return;
    }
    if (
      cfg?.remote_mode.enabled &&
      key.enabled &&
      !changes.enabled &&
      enabledRemoteKeyCount <= 1
    ) {
      setMsg({ kind: "err", text: "远程模式至少需要一个启用的独立 Key；请先关闭远程模式或新建并启用另一个 Key。" });
      return;
    }

    setBusy(`remote-key-${key.id}`);
    try {
      await api.updateRemoteAccessKey({ id: key.id, ...changes, label: changes.label.trim() });
      setEditingRemoteKey(null);
      await load();
      setMsg({ kind: "ok", text: `已更新远程访问 Key“${changes.label.trim()}”` });
    } catch (error) {
      setMsg({ kind: "err", text: `更新远程访问 Key 失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const deleteRemoteAccessKey = async (key: RemoteAccessKeyView) => {
    if (
      cfg?.remote_mode.enabled &&
      key.enabled &&
      enabledRemoteKeyCount <= 1
    ) {
      setMsg({ kind: "err", text: "远程模式至少需要一个启用的独立 Key；请先关闭远程模式或新建并启用另一个 Key。" });
      return;
    }
    if (!window.confirm(`确定删除远程访问 Key“${key.label}”吗？此操作无法恢复。`)) return;

    setBusy(`remote-key-${key.id}`);
    try {
      await api.deleteRemoteAccessKey(key.id);
      await load();
      setMsg({ kind: "ok", text: `已删除远程访问 Key“${key.label}”` });
    } catch (error) {
      setMsg({ kind: "err", text: `删除远程访问 Key 失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const prepareRemoteModeEnable = () => {
    if (enabledRemoteKeyCount === 0) {
      setMsg({ kind: "err", text: "请先创建并启用至少一个独立远程访问 Key，再开启远程 HTTPS 反代模式。" });
      return;
    }
    const publicUrl = cfg?.remote_mode.public_url?.trim();
    if (!publicUrl || !/^https:\/\//i.test(publicUrl)) {
      setMsg({ kind: "err", text: "请先填写以 https:// 开头的公开反代地址。" });
      return;
    }
    setRemoteConfirmationOpen(true);
  };

  const saveRemotePublicUrl = () => {
    if (!cfg) return;
    const publicUrl = cfg.remote_mode.public_url?.trim() || null;
    if (cfg.remote_mode.enabled && (!publicUrl || !/^https:\/\//i.test(publicUrl))) {
      setMsg({ kind: "err", text: "启用远程 HTTPS 反代模式时，公开反代地址必须以 https:// 开头。" });
      return;
    }
    void patch({ remote_mode: { ...cfg.remote_mode, public_url: publicUrl } });
  };

  const customRules = customRulesDraft ?? cfg?.custom_rules ?? [];
  const updateCustomRules = (updater: (rules: CustomRouteRule[]) => CustomRouteRule[]) => {
    if (!cfg) return;
    setCustomRulesDraft((current) => updater(current ?? cfg.custom_rules));
  };
  const replaceCustomRule = (index: number, next: CustomRouteRule) => {
    updateCustomRules((rules) => rules.map((rule, position) => position === index ? next : rule));
  };
  const saveCustomRules = async () => {
    if (!cfg) return;
    const rules = customRules.map((rule) => {
      const prefix = rule.prefix.trim();
      switch (rule.action.type) {
        case "only_dialect":
          return { prefix, action: rule.action };
        case "exclude_provider":
          return { prefix, action: { ...rule.action, provider_id: rule.action.provider_id.trim() } };
        case "boost_provider":
          return { prefix, action: { ...rule.action, provider_id: rule.action.provider_id.trim() } };
      }
    });
    for (const [index, rule] of rules.entries()) {
      const position = index + 1;
      if (!rule.prefix) {
        setMsg({ kind: "err", text: `第 ${position} 条规则缺少模型名前缀` });
        return;
      }
      if (rule.action.type !== "only_dialect" && !rule.action.provider_id) {
        setMsg({ kind: "err", text: `第 ${position} 条规则缺少目标 Provider` });
        return;
      }
      if (
        rule.action.type === "boost_provider"
        && (!Number.isInteger(rule.action.bonus)
          || rule.action.bonus < -2147483648
          || rule.action.bonus > 2147483647)
      ) {
        setMsg({ kind: "err", text: `第 ${position} 条规则的加分必须是 32 位整数` });
        return;
      }
    }

    setBusy("custom-rules");
    try {
      const result = await api.updateConfig({ ...cfg, custom_rules: rules });
      setCfg(result.config);
      notifyConfigChanged(result.config);
      setCustomRulesDraft(null);
      setMsg({ kind: "ok", text: "自定义路由规则已保存并热生效" });
    } catch (error) {
      setMsg({ kind: "err", text: `保存自定义路由规则失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  if (!cfg || !keyInfo) return <div className="empty">加载设置中</div>;

  const enabledRemoteKeyCount = remoteKeys.filter((key) => key.enabled).length;

  return (
    <div>
      <h2>设置</h2>
      <div className="sub">
        当前保存的监听地址为 <span className="mono">{cfg.bind}:{cfg.port}</span>。
        {cfg.remote_mode.enabled ? " 远程 HTTPS 反代模式已配置，网关仅监听本机回环地址。" : " 默认仅监听本机回环地址。"}
      </div>

      {msg && <div role={msg.kind === "err" ? "alert" : "status"} className={`msg ${msg.kind}`}>{msg.text}</div>}

      <div className="card">
        <strong>统一接入地址</strong>
        <div className="sub">
          客户端只需要这个地址和 Key；切换后端供应商无需改动客户端配置。
        </div>
        <div className="code">
{`# OpenAI SDK / LangChain / Continue / Cursor
export OPENAI_BASE_URL="${keyInfo.openai_endpoint}"
export OPENAI_API_KEY="${keyInfo.key}"

# Claude Code
export ANTHROPIC_BASE_URL="${keyInfo.anthropic_endpoint}"
export ANTHROPIC_AUTH_TOKEN="${keyInfo.key}"
export ANTHROPIC_API_KEY=""

# Codex CLI: ~/.codex/config.toml
base_url = "${keyInfo.openai_endpoint}"

# Ollama 兼容
${keyInfo.ollama_endpoint}`}
        </div>
        <div className="row" style={{ marginTop: 10 }}>
          <button
            disabled={busy !== null}
            onClick={() => {
              void (async () => {
                try {
                  await navigator.clipboard.writeText(keyInfo.key);
                  setMsg({ kind: "ok", text: "已复制统一 Key" });
                } catch (error) {
                  setMsg({ kind: "err", text: `复制失败：${errorText(error)}` });
                }
              })();
            }}
          >
            复制 Key
          </button>
          <button
            disabled={busy !== null}
            onClick={() => {
              if (!window.confirm("轮换后，所有客户端都必须更新为新的统一 Key。确定继续吗？")) return;
              void (async () => {
                setBusy("rotate-key");
                try {
                  await api.rotateUnifiedKey();
                  await load();
                  setMsg({ kind: "ok", text: "统一 Key 已轮换，请更新已接入的客户端" });
                } catch (error) {
                  setMsg({ kind: "err", text: `轮换失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            轮换 Key
          </button>
        </div>
      </div>

      <div className="card">
        <strong>服务监听</strong>
        <div className="grid3" style={{ marginTop: 10 }}>
          <div className="field">
            <label>监听地址</label>
            <input value={cfg.bind} disabled />
          </div>
          <div className="field">
            <label>端口</label>
            <input
              type="number"
              min={1}
              max={65535}
              value={cfg.port}
              disabled={busy === "config"}
              onChange={(event) => setCfg({ ...cfg, port: Number(event.target.value) })}
              onBlur={() => saveNumber("port", cfg.port)}
            />
          </div>
          <div className="field">
            <label>上游超时（秒）</label>
            <input
              type="number"
              min={1}
              value={cfg.upstream_timeout_secs}
              disabled={busy === "config"}
              onChange={(event) => setCfg({ ...cfg, upstream_timeout_secs: Number(event.target.value) })}
              onBlur={() => saveNumber("upstream_timeout_secs", cfg.upstream_timeout_secs)}
            />
          </div>
        </div>
        <label className="row setting-toggle" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={cfg.allow_lan}
            disabled={busy === "config" || cfg.remote_mode.enabled}
            onChange={(event) => {
              if (event.target.checked) {
                setLanConfirmationOpen(true);
              } else {
                void patch({ allow_lan: false });
              }
            }}
          />
          允许局域网访问
        </label>
        <div className="sub" style={{ marginTop: 8, marginBottom: 0 }}>
          关闭时强制绑定 <span className="mono">127.0.0.1</span>；确认开启后保存为 <span className="mono">0.0.0.0</span>，监听地址变更必须重启网关进程。
        </div>
      </div>

      <div className="card">
        <div className="row" style={{ justifyContent: "space-between", alignItems: "center" }}>
          <strong>远程 HTTPS 反代</strong>
          <span className={`tag ${cfg.remote_mode.enabled ? "ok" : "warn"}`}>
            {cfg.remote_mode.enabled ? "已启用" : "未启用"}
          </span>
        </div>
        <div className="sub">
          仅适用于 Caddy、Nginx 等已配置 TLS 的 HTTPS 反向代理。启用后网关强制只监听
          <span className="mono">127.0.0.1</span>，公网请求必须先经过反代；不要直接暴露网关 HTTP 端口或管理面。
        </div>
        <div className="grid2" style={{ marginTop: 10 }}>
          <div className="field">
            <label>公开 HTTPS 地址</label>
            <input
              type="url"
              value={cfg.remote_mode.public_url ?? ""}
              placeholder="https://llm.example.com"
              disabled={busy === "config"}
              onChange={(event) => setCfg({
                ...cfg,
                remote_mode: { ...cfg.remote_mode, public_url: event.target.value },
              })}
              onBlur={saveRemotePublicUrl}
            />
          </div>
          <div className="field">
            <label>已启用访问 Key</label>
            <div className="row" style={{ minHeight: 35 }}>
              <span className={`tag ${enabledRemoteKeyCount > 0 ? "ok" : "warn"}`}>
                {enabledRemoteKeyCount} 个
              </span>
              <span className="muted">
                {enabledRemoteKeyCount > 0 ? "可用于远程客户端认证" : "请先创建并启用至少一个 Key"}
              </span>
            </div>
          </div>
        </div>
        <label className="row setting-toggle" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={cfg.remote_mode.enabled}
            disabled={busy === "config" || enabledRemoteKeyCount === 0}
            onChange={(event) => {
              if (event.target.checked) {
                prepareRemoteModeEnable();
              } else {
                void patch({
                  allow_lan: false,
                  remote_mode: { ...cfg.remote_mode, enabled: false },
                });
              }
            }}
          />
          启用远程 HTTPS 反代模式
        </label>
        <div className="sub" style={{ marginTop: 8, marginBottom: 0 }}>
          {enabledRemoteKeyCount === 0
            ? "创建一个独立访问 Key 后才可启用；后端也会拒绝没有 Key 的远程模式。"
            : "开关变更会写入配置，监听边界变更需重启网关进程后生效。"}
        </div>

        <div className="grid2" style={{ marginTop: 18 }}>
          <div className="field">
            <label>新访问 Key 名称</label>
            <input
              value={remoteKeyLabel}
              disabled={busy !== null}
              maxLength={64}
              placeholder="例如 我的笔记本"
              onChange={(event) => setRemoteKeyLabel(event.target.value)}
            />
          </div>
          <div className="field">
            <label>每分钟请求上限（1-100000）</label>
            <input
              type="number"
              min={1}
              max={100000}
              value={remoteKeyRpmLimit}
              disabled={busy !== null}
              onChange={(event) => setRemoteKeyRpmLimit(Number(event.target.value))}
            />
          </div>
        </div>
        <div className="row" style={{ marginTop: -2 }}>
          <button
            className="primary"
            disabled={busy !== null || !remoteKeyLabel.trim()}
            onClick={() => void createRemoteAccessKey()}
          >
            创建并显示一次性 Key
          </button>
          <span className="muted">创建后只显示一次原始 Key，列表不会保存或展示它。</span>
        </div>

        <div className="table-card" style={{ marginTop: 14 }}>
          {remoteKeys.length === 0 ? (
            <div className="muted">暂无远程访问 Key。请为每台远程客户端创建独立 Key，便于单独限流或停用。</div>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>名称</th>
                  <th>状态</th>
                  <th>RPM</th>
                  <th>创建时间</th>
                  <th style={{ width: 230 }}>操作</th>
                </tr>
              </thead>
              <tbody>
                {remoteKeys.map((key) => (
                  <tr key={key.id}>
                    <td>{key.label}</td>
                    <td>
                      <span className={`tag ${key.enabled ? "ok" : "warn"}`}>
                        {key.enabled ? "已启用" : "已停用"}
                      </span>
                    </td>
                    <td>{key.rpm_limit}</td>
                    <td className="muted" style={{ fontSize: 11 }}>{formatDate(key.created_at)}</td>
                    <td>
                      <div className="row compact-actions">
                        <button
                          className="ghost"
                          disabled={busy !== null}
                          onClick={() => {
                            setEditingRemoteKey(key);
                            setEditingRemoteKeyLabel(key.label);
                            setEditingRemoteKeyRpmLimit(key.rpm_limit);
                          }}
                        >
                          编辑
                        </button>
                        <button
                          disabled={busy !== null}
                          onClick={() => void updateRemoteAccessKey(key, {
                            label: key.label,
                            enabled: !key.enabled,
                            rpm_limit: key.rpm_limit,
                          })}
                        >
                          {key.enabled ? "停用" : "启用"}
                        </button>
                        <button
                          className="danger"
                          disabled={busy !== null}
                          onClick={() => void deleteRemoteAccessKey(key)}
                        >
                          删除
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      <div className="card">
        <strong>降级与容错</strong>
        <div className="grid3" style={{ marginTop: 10 }}>
          <div className="field">
            <label>最大降级次数</label>
            <input
              type="number"
              min={0}
              value={cfg.max_fallback_attempts}
              disabled={busy === "config"}
              onChange={(event) => setCfg({ ...cfg, max_fallback_attempts: Number(event.target.value) })}
              onBlur={() => saveNumber("max_fallback_attempts", cfg.max_fallback_attempts)}
            />
          </div>
          <div className="field">
            <label>粘性会话时长（秒）</label>
            <input
              type="number"
              min={0}
              value={cfg.sticky_ttl_secs}
              disabled={busy === "config"}
              onChange={(event) => setCfg({ ...cfg, sticky_ttl_secs: Number(event.target.value) })}
              onBlur={() => saveNumber("sticky_ttl_secs", cfg.sticky_ttl_secs)}
            />
          </div>
          <div className="field">
            <label>压缩阈值（tokens）</label>
            <input
              type="number"
              min={1}
              value={cfg.compact_threshold_tokens}
              disabled={busy === "config"}
              onChange={(event) => setCfg({ ...cfg, compact_threshold_tokens: Number(event.target.value) })}
              onBlur={() => saveNumber("compact_threshold_tokens", cfg.compact_threshold_tokens)}
            />
          </div>
        </div>
        <label className="row setting-toggle" style={{ gap: 8 }}>
          <input
            type="checkbox"
            checked={cfg.failover_enabled}
            disabled={busy === "config"}
            onChange={(event) => void patch({ failover_enabled: event.target.checked })}
          />
          启用自动故障转移
        </label>
      </div>

      {cfg.routing_strategy === "custom" && (
        <div className="card">
          <div className="spread">
            <div>
              <strong>自定义前缀路由</strong>
              <div className="sub" style={{ marginBottom: 0 }}>
                规则只在 custom 策略下生效。按模型请求名前缀匹配；筛选后的候选仍受健康、限流和故障转移约束。
              </div>
            </div>
            <button
              className="ghost"
              disabled={busy !== null}
              onClick={() => updateCustomRules((rules) => [...rules, newCustomRule(providers[0]?.id ?? "")])}
            >
              添加规则
            </button>
          </div>

          {customRules.length === 0 ? (
            <div className="empty custom-rules-empty">尚未配置规则；custom 策略会使用均衡排序直到添加规则。</div>
          ) : (
            <div className="custom-rules">
              {customRules.map((rule, index) => {
                const action = rule.action;
                const missingProvider = action.type !== "only_dialect"
                  && action.provider_id
                  && !providers.some((provider) => provider.id === action.provider_id);
                return (
                  <div className="custom-route-rule" key={`${index}-${action.type}`}>
                    <div className="field">
                      <label>模型名前缀</label>
                      <input
                        value={rule.prefix}
                        disabled={busy !== null}
                        placeholder="例如 claude-"
                        onChange={(event) => replaceCustomRule(index, { ...rule, prefix: event.target.value })}
                      />
                    </div>
                    <div className="field">
                      <label>动作</label>
                      <select
                        value={action.type}
                        disabled={busy !== null}
                        onChange={(event) => replaceCustomRule(index, {
                          ...rule,
                          action: defaultRuleAction(
                            event.target.value as CustomRouteRuleAction["type"],
                            providers[0]?.id ?? "",
                          ),
                        })}
                      >
                        <option value="only_dialect">仅使用方言</option>
                        <option value="exclude_provider">排除 Provider</option>
                        <option value="boost_provider">优先 Provider</option>
                      </select>
                    </div>
                    {action.type === "only_dialect" ? (
                      <div className="field">
                        <label>方言</label>
                        <select
                          value={action.dialect}
                          disabled={busy !== null}
                          onChange={(event) => replaceCustomRule(index, {
                            ...rule,
                            action: { type: "only_dialect", dialect: event.target.value as Dialect },
                          })}
                        >
                          <option value="openai">OpenAI 兼容</option>
                          <option value="anthropic">Anthropic</option>
                          <option value="gemini">Gemini</option>
                          <option value="ollama">Ollama</option>
                        </select>
                      </div>
                    ) : (
                      <div className="field">
                        <label>目标 Provider</label>
                        <select
                          value={action.provider_id}
                          disabled={busy !== null}
                          onChange={(event) => {
                            const provider_id = event.target.value;
                            replaceCustomRule(index, {
                              ...rule,
                              action: action.type === "exclude_provider"
                                ? { type: "exclude_provider", provider_id }
                                : { type: "boost_provider", provider_id, bonus: action.bonus },
                            });
                          }}
                        >
                          <option value="">选择 Provider</option>
                          {missingProvider && <option value={action.provider_id}>已不存在：{action.provider_id}</option>}
                          {providers.map((provider) => (
                            <option key={provider.id} value={provider.id}>
                              {provider.name} ({provider.id})
                            </option>
                          ))}
                        </select>
                      </div>
                    )}
                    {action.type === "boost_provider" && (
                      <div className="field">
                        <label>加分</label>
                        <input
                          type="number"
                          step={1}
                          value={action.bonus}
                          disabled={busy !== null}
                          onChange={(event) => replaceCustomRule(index, {
                            ...rule,
                            action: {
                              type: "boost_provider",
                              provider_id: action.provider_id,
                              bonus: Number(event.target.value),
                            },
                          })}
                        />
                      </div>
                    )}
                    <button
                      className="danger ghost icon-button custom-rule-delete"
                      title="删除规则"
                      aria-label={`删除第 ${index + 1} 条规则`}
                      disabled={busy !== null}
                      onClick={() => updateCustomRules((rules) => rules.filter((_, position) => position !== index))}
                    >
                      ×
                    </button>
                  </div>
                );
              })}
            </div>
          )}

          <div className="row end" style={{ marginTop: 12 }}>
            <button
              disabled={busy !== null || JSON.stringify(customRules) === JSON.stringify(cfg.custom_rules)}
              onClick={() => setCustomRulesDraft(null)}
            >
              还原
            </button>
            <button
              className="primary"
              disabled={busy !== null || JSON.stringify(customRules) === JSON.stringify(cfg.custom_rules)}
              onClick={() => void saveCustomRules()}
            >
              {busy === "custom-rules" ? "保存中" : "保存规则"}
            </button>
          </div>
        </div>
      )}

      <div className="card">
        <strong>本机 CLI 工具</strong>
        <div className="sub">
          管理本机安装的 AI 编码 CLI：检测路径与版本，未安装的也能直接下载安装，并支持一键更新。覆盖 Claude Code、Codex、Gemini CLI、Qoder CLI（国际/国内版）、
          OpenCode、OpenClaw、Pi、DeepSeek Harness、WorkBuddy、Cline、Amp、Auggie、Continue CLI、Crush、Factory Droid、iFlow CLI、
          Grok Build、Cursor CLI、TRAE CLI、Hermes Agent。
          「检测本机 CLI」只读取 PATH 与常见安装目录、不联网；「检测并检查更新」会查询 npm 最新版本（官方脚本类工具除外，脚本始终安装最新版）。
          安装命令全部来自内置常量，执行前会展示确切命令；npm 类通过 npm 全局安装，脚本类执行官方 PowerShell 安装脚本。
        </div>
        <div className="row">
          <button
            disabled={busy !== null}
            onClick={() => {
              void (async () => {
                setBusy("cli-detect");
                try {
                  const reports = await api.detectCliTools();
                  setCliTools(reports);
                  setMsg({ kind: "ok", text: `已检测 ${reports.length} 个工具` });
                } catch (error) {
                  setMsg({ kind: "err", text: `检测失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            {busy === "cli-detect" ? "检测中…" : "检测本机 CLI"}
          </button>
          <button
            disabled={busy !== null}
            onClick={() => {
              void (async () => {
                setBusy("cli-check");
                try {
                  const reports = await api.detectCliToolsWithUpdates();
                  setCliTools(reports);
                  const updatable = reports.filter((report) => report.update_available).length;
                  const failed = reports.filter((report) => report.check_error).length;
                  setMsg({
                    kind: failed && !reports.some((report) => report.latest_version) ? "err" : "ok",
                    text: `已检测并查询最新版本；可更新 ${updatable} 个${
                      failed ? `；${failed} 个工具未能查询到最新版本（不影响本机检测结果）` : ""
                    }`,
                  });
                } catch (error) {
                  setMsg({ kind: "err", text: `检查更新失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            {busy === "cli-check" ? "查询中…" : "检测并检查更新"}
          </button>
        </div>
        {cliTools.length > 0 && (
          <div style={{ marginTop: 12 }}>
            {cliTools.some((tool) => tool.source === "npm" && !tool.can_install) && (
              <div className="msg" role="status">未检测到 npm：npm 类工具无法一键安装或更新，请先安装 Node.js。</div>
            )}
            {cliTools.some((tool) => tool.source === "script" && !tool.can_install) && (
              <div className="msg" role="status">当前平台没有可用的 PowerShell：官方脚本类工具请按各自官方文档手动安装，其余工具不受影响。</div>
            )}
            <table>
              <thead>
                <tr>
                  <th>工具</th>
                  <th>状态</th>
                  <th>版本</th>
                  <th>最新</th>
                  <th style={{ width: 150 }}>操作</th>
                </tr>
              </thead>
              <tbody>
                {cliTools.map((report) => {
                  const installing = busy === `cli-install-${report.id}`;
                  const isScript = report.source === "script";
                  // 每个按钮都必须能完成它声称的事：未安装 → 安装；查到新版本 → 更新；
                  // 其余（已是最新、尚未查询、官方脚本）→ 重新安装，同样装到最新版。
                  const hasUpdate = !isScript && report.installed && report.update_available;
                  const actionLabel = !report.installed ? "安装" : hasUpdate ? "更新" : "重新安装";
                  const actionEnabled = report.can_install;
                  const actionTitle = !report.can_install
                    ? (isScript ? "当前平台没有可用的 PowerShell，请按官方文档手动安装" : "未检测到 npm，请先安装 Node.js 后再一键安装")
                    : `将执行：${report.install_command}`;
                  return (
                  <tr key={report.id}>
                    <td>
                      <strong>{report.label}</strong>
                      <div className="muted mono" style={{ fontSize: 10, overflowWrap: "anywhere" }}>
                        {report.path ?? report.install_target}
                      </div>
                    </td>
                    <td>
                      {report.installed
                        ? <span className="tag ok">已安装</span>
                        : <span className="tag warn">未检测到</span>}
                      <span className="tag" style={{ marginLeft: 4 }}>{isScript ? "官方脚本" : "npm"}</span>
                    </td>
                    <td className="mono">{report.version ?? "-"}</td>
                    <td className="mono">
                      {report.latest_version ?? (isScript ? "以脚本为准" : "-")}
                      {report.update_available && !isScript && <span className="tag warn" style={{ marginLeft: 6 }}>可更新</span>}
                      {report.check_error && (
                        <div className="muted" style={{ fontSize: 10 }}>{report.check_error}</div>
                      )}
                    </td>
                    <td>
                      <button
                        className="ghost"
                        disabled={busy !== null || !actionEnabled}
                        title={actionTitle}
                        onClick={() => {
                          const lead = isScript
                            ? `将执行官方安装脚本：\n${report.install_command}\n\n这会从官方地址下载并执行 ${report.label} 的安装脚本（始终安装最新版）。确定继续吗？`
                            : `将执行：\n${report.install_command}\n\n这会通过 npm 全局${actionLabel} ${report.label}。确定继续吗？`;
                          if (!window.confirm(lead)) return;
                          void (async () => {
                            setBusy(`cli-install-${report.id}`);
                            try {
                              const output = await api.installCliTool(report.id);
                              const refreshed = await api.detectCliTools();
                              setCliTools(refreshed);
                              const stillMissing = !refreshed.find((item) => item.id === report.id)?.installed;
                              setMsg({
                                kind: stillMissing ? "err" : "ok",
                                text: `${report.label} 安装命令已执行${stillMissing ? "，但本机仍未检测到该命令；请按下方输出或官方文档检查 PATH" : ""}。输出：${output.slice(0, 300) || "（无输出）"}`,
                              });
                            } catch (error) {
                              setMsg({ kind: "err", text: `${report.label} 安装失败：${errorText(error)}` });
                            } finally {
                              setBusy(null);
                            }
                          })();
                        }}
                      >
                        {installing ? "执行中…" : actionLabel}
                      </button>
                    </td>
                  </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <PetCard />

      <div className="card">
        <strong>CLI 工具接管</strong>
        <div className="sub">
          已有配置会先创建同目录的唯一备份并逐字节校验，备份失败不会改写原文件。Gemini CLI 暂不支持接管；已有旧选项可取消。写入后请重新启动对应 CLI。
        </div>
        <div className="row">
          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              checked={cfg.takeover.claude_code}
              disabled={busy !== null}
              onChange={(event) => void patch({ takeover: { ...cfg.takeover, claude_code: event.target.checked } })}
            />
            Claude Code
          </label>
          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              checked={cfg.takeover.codex}
              disabled={busy !== null}
              onChange={(event) => void patch({ takeover: { ...cfg.takeover, codex: event.target.checked } })}
            />
            Codex CLI
          </label>
          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              checked={cfg.takeover.gemini_cli}
              disabled={busy !== null || !cfg.takeover.gemini_cli}
              onChange={(event) => void patch({ takeover: { ...cfg.takeover, gemini_cli: event.target.checked } })}
            />
            Gemini CLI（暂不支持）
          </label>
          <button
            className="primary"
            disabled={busy !== null || cfg.takeover.gemini_cli}
            title={cfg.takeover.gemini_cli ? "请先取消 Gemini CLI 的旧接管选项" : undefined}
            onClick={() => {
              void (async () => {
                setTakeoverResults([]);
                setBusy("takeover");
                try {
                  const results = await api.applyTakeover();
                  setTakeoverResults(results);
                  setMsg({
                    kind: "ok",
                    text: results.length ? `已安全写入 ${results.length} 项 CLI 配置` : "未选择任何工具",
                  });
                } catch (error) {
                  setMsg({ kind: "err", text: `写入配置失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            备份并写入配置
          </button>
        </div>
        {takeoverResults.length > 0 && (
          <div style={{ marginTop: 12 }} aria-live="polite">
            {takeoverResults.map((result) => (
              <div key={`${result.client}:${result.path}`} className="sub" style={{ marginTop: 8 }}>
                <strong>{result.client}</strong>：{result.status === "created" ? "已新建" : "已更新"}
                <div className="mono" style={{ overflowWrap: "anywhere" }}>{result.path}</div>
                {result.backup_path ? (
                  <div style={{ overflowWrap: "anywhere" }}>
                    已验证备份：<span className="mono">{result.backup_path}</span>
                  </div>
                ) : (
                  <div>原文件不存在，本次新建，因此没有备份。</div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="card">
        <strong>网关连通性自检</strong>
        <div className="sub">
          用统一 Key 向本机网关发出一次最小请求（约 1 个 token），确认监听、鉴权与上游链路端到端真的可用，并显示实际路由到的上游。它不依赖是否已写入客户端配置。
        </div>
        <div className="row">
          <button
            disabled={busy !== null}
            onClick={() => {
              void (async () => {
                setBusy("self-check");
                try {
                  const result = await api.runGatewaySelfCheck();
                  setSelfCheck(result);
                  setMsg(result.healthy
                    ? { kind: "ok", text: `自检通过：网关 ${result.base_url} 正常，实际路由到 ${result.routed_via ?? "未知上游"}（${result.latency_ms}ms）` }
                    : { kind: "err", text: `自检未通过：${result.error ?? "未知原因"}` });
                } catch (error) {
                  setMsg({ kind: "err", text: `自检失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            {busy === "self-check" ? "自检中…" : "运行连通性自检"}
          </button>
          <span className="muted" style={{ fontSize: 11 }}>
            未配置供应商时，自检会明确报告「所有候选 Provider 均不可用」，这属于预期结果。
          </span>
        </div>
        {selfCheck && (
          <div className={selfCheck.healthy ? "msg ok" : "msg err"} style={{ marginTop: 8 }}>
            {selfCheck.healthy
              ? `健康检查通过；端到端路由到 ${selfCheck.routed_via ?? "未知上游"}，耗时 ${selfCheck.latency_ms}ms。`
              : `未通过：${selfCheck.error ?? "未知原因"}`}
          </div>
        )}
      </div>

      <div className="card">
        <strong>项目快照</strong>
        <div className="sub">
          快照保存供应商、模型映射、路由与运行配置；应用时会完整替换这些配置，并保留同 ID Provider 的本机 Key。
        </div>
        <div className="row">
          <input
            value={snapName}
            disabled={busy !== null}
            onChange={(event) => setSnapName(event.target.value)}
            placeholder="快照名称，例如 编码"
          />
          <button
            disabled={busy !== null || !snapName.trim()}
            onClick={() => {
              void (async () => {
                setBusy("snapshot-create");
                try {
                  await api.createSnapshot(snapName.trim());
                  setSnapName("");
                  await load();
                  setMsg({ kind: "ok", text: "已保存当前配置快照" });
                } catch (error) {
                  setMsg({ kind: "err", text: `保存快照失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            保存当前为快照
          </button>
        </div>
        <div style={{ marginTop: 10 }}>
          {snaps.length === 0 ? (
            <div className="muted">暂无快照</div>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>名称</th>
                  <th>Provider</th>
                  <th>主用</th>
                  <th>创建时间</th>
                  <th style={{ width: 100 }}>操作</th>
                </tr>
              </thead>
              <tbody>
                {snaps.map((snapshot) => (
                  <tr key={snapshot.id}>
                    <td>{snapshot.name}</td>
                    <td>{snapshot.provider_count}</td>
                    <td className="mono muted">{snapshot.active_provider_id ?? "自动路由"}</td>
                    <td className="muted" style={{ fontSize: 11 }}>{formatDate(snapshot.created_at)}</td>
                    <td>
                      <button
                        className="ghost"
                        disabled={busy !== null}
                        onClick={() => {
                          if (!window.confirm(`应用快照“${snapshot.name}”会替换当前 Provider、模型映射和路由配置。确定继续吗？`)) return;
                          void (async () => {
                            setBusy(`snapshot-${snapshot.id}`);
                            try {
                              const result = await api.applySnapshot(snapshot.id);
                              setCfg(result.config);
                              notifyConfigChanged(result.config);
                              await load();
                              setMsg({ kind: "ok", text: `已应用快照“${snapshot.name}”。${restartMessage(result)}` });
                            } catch (error) {
                              setMsg({ kind: "err", text: `应用快照失败：${errorText(error)}` });
                            } finally {
                              setBusy(null);
                            }
                          })();
                        }}
                      >
                        应用
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      <div className="card">
        <strong>配置包导入 / 导出</strong>
        <div className="sub">
          把本机的 config.toml 与 gateway.db 导出到指定目录，或在另一台设备上导入。
          导入时会完整替换 Provider 与模型配置，并在导入前把当前数据目录备份一份；统一访问 Key 与远程 HTTPS 模式属于本机安全边界，不会被包内值覆盖。
          从其他设备导出的包中的 API Key 由那台设备的主密钥加密，本机无法解密，导入后会列为「需要重新填写 Key」。
        </div>
        <div className="row">
          <button
            disabled={busy !== null}
            onClick={() => {
              void (async () => {
                try {
                  const { open } = await import("@tauri-apps/plugin-dialog");
                  const dir = await open({ directory: true, multiple: false, title: "选择导出目录" });
                  if (typeof dir !== "string") return;
                  setBusy("bundle-export");
                  await api.exportBundle(dir);
                  setMsg({ kind: "ok", text: `已导出到 ${dir}` });
                } catch (error) {
                  setMsg({ kind: "err", text: `导出失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            导出配置包
          </button>
          <button
            disabled={busy !== null}
            onClick={() => {
              void (async () => {
                try {
                  const { open } = await import("@tauri-apps/plugin-dialog");
                  const dir = await open({ directory: true, multiple: false, title: "选择包含 config.toml 与 gateway.db 的目录" });
                  if (typeof dir !== "string") return;
                  if (!window.confirm(`导入会用 ${dir} 中的配置替换当前 Provider 与模型配置。导入前会自动备份当前数据目录。确定继续吗？`)) return;
                  setBusy("bundle-import");
                  const outcome = await api.importBundle(dir);
                  const imported = await api.getConfig();
                  setCfg(imported);
                  notifyConfigChanged(imported);
                  await load();
                  const { result } = outcome;
                  const missing = result.providers_missing_key.length
                    ? `。以下 Provider 的 Key 需要用本机主密钥重新填写：${result.providers_missing_key.join("、")}`
                    : "";
                  setMsg({
                    kind: result.providers_missing_key.length ? "err" : "ok",
                    text: `已导入 ${result.providers_imported} 个 Provider、${result.models_imported} 个模型；配置${result.config_imported ? "已应用" : "未包含"}。导入前备份：${outcome.backup_dir}${missing}`,
                  });
                } catch (error) {
                  setMsg({ kind: "err", text: `导入失败：${errorText(error)}` });
                } finally {
                  setBusy(null);
                }
              })();
            }}
          >
            导入配置包
          </button>
        </div>
      </div>

      {oneTimeSecretLabel && (
        <div className="modal-mask">
          <div className="modal confirm-modal">
            <h3>保存新的远程访问 Key</h3>
            <p className="confirm-lead">
              “{oneTimeSecretLabel}”的原始 Key 只会在此处显示一次。关闭此窗口后，应用无法再次查看或恢复它。
            </p>
            <div className="code">{oneTimeSecretRef.current ?? "Key 已清除"}</div>
            <div className="msg err" style={{ marginTop: 12 }}>
              请立即保存到受信任的密码管理器。不要截图、写入日志或共享给他人。
            </div>
            <div className="row end" style={{ marginTop: 18 }}>
              <button
                onClick={() => {
                  const secret = oneTimeSecretRef.current;
                  if (!secret) {
                    setMsg({ kind: "err", text: "原始 Key 已清除，无法复制。" });
                    return;
                  }
                  void (async () => {
                    try {
                      await navigator.clipboard.writeText(secret);
                      setMsg({ kind: "ok", text: "已复制一次性远程访问 Key，请安全保存。" });
                    } catch (error) {
                      setMsg({ kind: "err", text: `复制 Key 失败：${errorText(error)}` });
                    }
                  })();
                }}
              >
                复制 Key
              </button>
              <button className="primary" onClick={closeOneTimeSecret}>我已安全保存</button>
            </div>
          </div>
        </div>
      )}

      {editingRemoteKey && (
        <div className="modal-mask" onClick={() => setEditingRemoteKey(null)}>
          <div className="modal" onClick={(event) => event.stopPropagation()}>
            <h3>编辑远程访问 Key</h3>
            <div className="field">
              <label>名称</label>
              <input
                value={editingRemoteKeyLabel}
                maxLength={64}
                disabled={busy !== null}
                onChange={(event) => setEditingRemoteKeyLabel(event.target.value)}
              />
            </div>
            <div className="field">
              <label>每分钟请求上限（1-100000）</label>
              <input
                type="number"
                min={1}
                max={100000}
                value={editingRemoteKeyRpmLimit}
                disabled={busy !== null}
                onChange={(event) => setEditingRemoteKeyRpmLimit(Number(event.target.value))}
              />
            </div>
            <div className="sub">
              编辑不会显示或轮换原始 Key。若需要更换凭据，请创建新 Key，确认客户端迁移后再停用旧 Key。
            </div>
            <div className="row end" style={{ marginTop: 18 }}>
              <button disabled={busy !== null} onClick={() => setEditingRemoteKey(null)}>取消</button>
              <button
                className="primary"
                disabled={busy !== null || !editingRemoteKeyLabel.trim()}
                onClick={() => void updateRemoteAccessKey(editingRemoteKey, {
                  label: editingRemoteKeyLabel,
                  enabled: editingRemoteKey.enabled,
                  rpm_limit: editingRemoteKeyRpmLimit,
                })}
              >
                保存更改
              </button>
            </div>
          </div>
        </div>
      )}

      {remoteConfirmationOpen && (
        <div className="modal-mask" onClick={() => setRemoteConfirmationOpen(false)}>
          <div className="modal confirm-modal" onClick={(event) => event.stopPropagation()}>
            <h3>确认启用远程 HTTPS 反代</h3>
            <p className="confirm-lead">
              将通过 <span className="mono">{cfg.remote_mode.public_url?.trim()}</span> 接受远程客户端请求。请确认以下边界都已满足：
            </p>
            <ul className="risk-list">
              <li>公开地址已由 Caddy、Nginx 或同类组件提供有效 HTTPS，TLS 在反向代理处终止。</li>
              <li>网关会强制仅绑定 <span className="mono">127.0.0.1</span>，不允许局域网直连，外部请求必须经过反代。</li>
              <li>至少已有一个独立远程访问 Key；没有启用 Key 时后端会拒绝启动远程模式。</li>
              <li>管理面只在本机使用，不能通过反代将设置页、系统托盘或管理接口公开。</li>
              <li>远程访问 Key 是凭据，只交给受控客户端，不能共享、截图、写入日志或公共配置。</li>
            </ul>
            <div className="msg err">
              监听边界变更只会写入配置；请从系统托盘退出并重新启动网关进程后才会生效。
            </div>
            <div className="row end" style={{ marginTop: 18 }}>
              <button onClick={() => setRemoteConfirmationOpen(false)}>取消</button>
              <button
                className="danger"
                onClick={() => {
                  setRemoteConfirmationOpen(false);
                  void patch({
                    allow_lan: false,
                    remote_mode: {
                      enabled: true,
                      public_url: cfg.remote_mode.public_url?.trim() || null,
                    },
                  });
                }}
              >
                我已确认，启用反代模式
              </button>
            </div>
          </div>
        </div>
      )}

      {lanConfirmationOpen && (
        <div className="modal-mask" onClick={() => setLanConfirmationOpen(false)}>
          <div className="modal confirm-modal" onClick={(event) => event.stopPropagation()}>
            <h3>确认开启局域网访问</h3>
            <p className="confirm-lead">
              此操作会在下次重启后将网关绑定到 <span className="mono">0.0.0.0</span>。请确认下列边界均已满足：
            </p>
            <ul className="risk-list">
              <li>默认保持关闭，仅允许你本人受控的设备访问。</li>
              <li>跨网络或远程访问必须放在 HTTPS 反向代理之后。</li>
              <li>管理面只在本机使用，不对局域网公开。</li>
              <li>不要共享统一 Key，也不要把它写入截图、日志或公共配置。</li>
              <li>确认本机防火墙仅放行所需网络范围。</li>
            </ul>
            <div className="msg err">
              监听地址变更只会写入配置；请从系统托盘退出并重新启动网关进程后才会生效。
            </div>
            <div className="row end" style={{ marginTop: 18 }}>
              <button onClick={() => setLanConfirmationOpen(false)}>取消</button>
              <button
                className="danger"
                onClick={() => {
                  setLanConfirmationOpen(false);
                  void patch({ allow_lan: true });
                }}
              >
                我已确认，开启访问
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
