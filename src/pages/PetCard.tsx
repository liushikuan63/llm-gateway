import { useEffect, useState } from "react";
import { api, DetectedTask, PetStatus } from "../api";
import "./pet-card.css";

type Message = { kind: "ok" | "err"; text: string };

export const PET_SLUG_STORAGE_KEY = "llm-gateway-pet-slug";
export const PET_SCALE_STORAGE_KEY = "llm-gateway-pet-scale";

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

const STATUS_LABEL: Record<PetStatus["status"], string> = {
  idle: "空闲",
  working: "工作中",
  error: "出错",
};

const KIND_LABEL: Record<string, string> = { cli: "CLI", app: "桌面应用" };
const TASK_LABEL: Record<string, string> = { running: "进行中", error: "出错", done: "已完成" };

function formatMemory(kb: number | null) {
  if (kb === null) return "-";
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)} MB`;
  return `${kb} KB`;
}

function readStoredScale() {
  try {
    const stored = Number(window.localStorage.getItem(PET_SCALE_STORAGE_KEY));
    if (!Number.isFinite(stored)) return 1;
    // 与 src-tauri/src/pet_window.rs 的 MIN_SCALE / MAX_SCALE 保持一致：最小 0.50×。
    return Math.min(3, Math.max(0.5, stored));
  } catch {
    return 1;
  }
}

function readStoredSlug() {
  try {
    return window.localStorage.getItem(PET_SLUG_STORAGE_KEY);
  } catch {
    return null;
  }
}

/// 设置页的「桌宠与 AI 监控」卡片：状态、AI 工具进程、宠物选择、大小与 Petdex 安装。
export default function PetCard() {
  const [status, setStatus] = useState<PetStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<Message | null>(null);
  const [catalog, setCatalog] = useState<string | null>(null);
  const [slug, setSlug] = useState("");
  const [selectedSlug, setSelectedSlug] = useState<string | null>(readStoredSlug);
  const [scale, setScale] = useState(readStoredScale);

  const refresh = async () => {
    try {
      setStatus(await api.getPetStatus());
    } catch (error) {
      setMsg({ kind: "err", text: `读取桌宠状态失败：${errorText(error)}` });
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const persistSlug = (next: string) => {
    setSelectedSlug(next);
    try {
      window.localStorage.setItem(PET_SLUG_STORAGE_KEY, next);
    } catch {
      // 受限 WebView 中仅当前会话生效。
    }
  };

  const openPet = async () => {
    setBusy("open");
    try {
      await api.openPetWindow();
      // 开启后立即应用已保存的大小，保持与上次一致。
      await api.setPetWindowSize(scale);
      await refresh();
      setMsg({ kind: "ok", text: "桌宠已开启：宠物本体 100% 为 120×130（可缩到 50% = 60×65），展开面板可查看任务并定位 AI 软件。" });
    } catch (error) {
      setMsg({ kind: "err", text: `开启桌宠失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const applyScale = async (value: number) => {
    setScale(value);
    try {
      window.localStorage.setItem(PET_SCALE_STORAGE_KEY, String(value));
    } catch {
      // 仅当前会话生效。
    }
    try {
      await api.setPetWindowSize(value);
      setMsg({ kind: "ok", text: `桌宠大小已调整为 ${Math.round(value * 100)}%。` });
    } catch (error) {
      setMsg({ kind: "err", text: `${errorText(error)}` });
    }
  };

  const browseCatalog = async () => {
    setBusy("catalog");
    try {
      setCatalog(await api.petdexCatalog());
      setMsg({ kind: "ok", text: "已获取 Petdex 商店列表（下方为 CLI 原始输出，安装时填写其中的 slug）。" });
    } catch (error) {
      setCatalog(null);
      setMsg({ kind: "err", text: `获取 Petdex 商店失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const installPet = async () => {
    const trimmed = slug.trim();
    if (!trimmed) {
      setMsg({ kind: "err", text: "请先填写宠物 slug（可先浏览 Petdex 商店查看）。" });
      return;
    }
    if (!window.confirm(`将执行：\n${"npx --yes petdex@latest install"} ${trimmed}\n\n这会从 Petdex（第三方）下载并安装宠物到 ~/.petdex/pets。确定继续吗？`)) return;
    setBusy("install");
    try {
      await api.petdexInstallPet(trimmed);
      setSlug("");
      await refresh();
      // 安装后直接选中新宠物，省去再点一次。
      persistSlug(trimmed);
      setMsg({ kind: "ok", text: `宠物 ${trimmed} 已安装并设为当前宠物。` });
    } catch (error) {
      setMsg({ kind: "err", text: `安装宠物失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const stopTool = async (toolId: string, label: string, processCount: number) => {
    if (!window.confirm(`将结束 ${label} 当前检测到的 ${processCount} 个进程（含子进程）。\n\n未保存的工作可能丢失；结束 IDE 等同关闭该应用。确定继续吗？`)) return;
    setBusy(`stop-${toolId}`);
    try {
      const output = await api.stopAiTool(toolId);
      await refresh();
      setMsg({ kind: "ok", text: output });
    } catch (error) {
      setMsg({ kind: "err", text: `结束进程失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const openAiTask = async (task: DetectedTask) => {
    const key = "task-" + task.source + ":" + task.session_id;
    setBusy(key);
    try {
      setMsg({ kind: "ok", text: await api.openAiTask(task.tool_id, task.session_id) });
    } catch (error) {
      setMsg({ kind: "err", text: "打开任务失败：" + errorText(error) });
    } finally {
      setBusy(null);
    }
  };

  const openTaskProject = async (path: string) => {
    setBusy("project-" + path);
    try {
      setMsg({ kind: "ok", text: await api.openTaskProject(path) });
    } catch (error) {
      setMsg({ kind: "err", text: "打开项目目录失败：" + errorText(error) });
    } finally {
      setBusy(null);
    }
  };

  const installedPets = status?.installed_pets ?? [];
  // 同一软件常有多个进程（如 Electron 的主/渲染/GPU 进程），按软件聚合展示。
  const monitoredTools = Array.from(
    (status?.ai_processes ?? []).reduce((groups, process) => {
      const group = groups.get(process.tool_id) ?? {
        toolId: process.tool_id,
        label: process.tool_label,
        kind: process.kind,
        processes: [] as PetStatus["ai_processes"],
      };
      group.processes.push(process);
      groups.set(process.tool_id, group);
      return groups;
    }, new Map<string, { toolId: string; label: string; kind: string; processes: PetStatus["ai_processes"] }>()),
  ).map(([, group]) => group);

  return (
    <div className="card" data-testid="pet-card">
      <strong>桌宠与 AI 监控</strong>
      <div className="sub">
        桌宠是独立置顶小窗口，用宠物包动画呈现网关与 AI 工具的实时状态（工作中 / 空闲 / 出错）：100% = 120×130，最小 50% = 60×65，展开信息面板后窗口会自动加大并显示当前任务。单击跳转到用量记录，按住可拖动，右键切换宠物、收起面板、暂停监控或隐藏。
        任务行会前置显示 AI 软件名，并可定位窗口、打开项目目录或结束该软件；宠物来自第三方 Petdex，本应用只读取 <code>~/.petdex/pets</code> 下的宠物包。
      </div>
      <div className="row">
        <button className="primary" disabled={busy !== null} onClick={() => void openPet()}>
          {busy === "open" ? "开启中…" : status?.pet_window_open ? "显示桌宠" : "开启桌宠"}
        </button>
        <label htmlFor="pet-select" style={{ fontSize: 12 }}>宠物</label>
        <select
          id="pet-select"
          data-testid="pet-select"
          value={selectedSlug ?? ""}
          disabled={busy !== null || installedPets.length === 0}
          onChange={(event) => persistSlug(event.target.value)}
        >
          {installedPets.length === 0 && <option value="">（未安装宠物）</option>}
          {installedPets.map((pet) => (
            <option key={pet.slug} value={pet.slug}>
              {pet.display_name}{pet.version ? ` · v${pet.version}` : ""}
            </option>
          ))}
        </select>
        <label htmlFor="pet-scale" style={{ fontSize: 12 }}>大小</label>
        <input
          id="pet-scale"
          type="range"
          min={50}
          max={300}
          step={25}
          value={Math.round(scale * 100)}
          disabled={busy !== null || !status?.pet_window_open}
          onChange={(event) => void applyScale(Number(event.target.value) / 100)}
          style={{ width: 140 }}
        />
        <span className="muted mono" style={{ fontSize: 12 }}>{Math.round(scale * 100)}%</span>
        <button disabled={busy !== null} onClick={() => void refresh()}>刷新监控</button>
        {status && (
          <span className={`tag ${status.status === "error" ? "err" : status.status === "working" ? "ok" : ""}`}>
            {STATUS_LABEL[status.status]}
          </span>
        )}
      </div>
      {status && <div className="muted" style={{ fontSize: 11, marginTop: 6 }}>{status.reason}</div>}
      {!status?.pet_window_open && <div className="muted" style={{ fontSize: 11 }}>大小在开启桌宠后可调整；50%–300%，其中 100% 对应宠物本体 120×130（最小 50% = 60×65），信息面板会额外扩宽窗口。</div>}

      {msg && <div className={`msg ${msg.kind}`} role={msg.kind === "err" ? "alert" : "status"} style={{ marginTop: 10 }}>{msg.text}</div>}

      {status && status.active_tasks.length > 0 && (
        <div className="pet-table-block">
          <div className="pet-table-hint">
            检测到的任务（来自工具会话日志，错误优先、其次最近活动）：宠物会按最高优先级切换动作。
          </div>
          <div className="pet-table-wrap">
            <table className="pet-table pet-task-table">
              <colgroup>
                <col style={{ width: 108 }} />
                <col />
                <col style={{ width: 92 }} />
                <col style={{ width: 150 }} />
                <col />
                <col style={{ width: 170 }} />
              </colgroup>
              <thead>
                <tr>
                  <th>来源</th>
                  <th>具体任务</th>
                  <th>状态</th>
                  <th>项目</th>
                  <th>最近事件</th>
                  <th>定向操作</th>
                </tr>
              </thead>
              <tbody>
                {status.active_tasks.map((task) => (
                  <tr key={`${task.source}-${task.session_id}`} data-testid="pet-card-task-row">
                    <td className="pet-task-source">
                      <strong>{task.source_label}</strong>
                    </td>
                    <td>
                      <div className="pet-task-title">{task.title || task.detail}</div>
                    </td>
                    <td>
                      <span
                        className={`pet-status-chip ${task.status}`}
                        data-testid="pet-card-task-status"
                        title={TASK_LABEL[task.status] ?? task.status}
                      >
                        <i className="dot" aria-hidden="true" />
                        {TASK_LABEL[task.status] ?? task.status}
                      </span>
                    </td>
                    <td className="pet-task-project" title={task.project}>
                      {task.project}
                    </td>
                    <td>
                      <div className="pet-task-event">{task.last_message || task.detail}</div>
                    </td>
                    <td className="pet-ops-cell">
                      <div className="row">
                        <button
                          className="ghost"
                          disabled={busy !== null || (!task.deep_link && !task.tool_id.startsWith("qoder"))}
                          onClick={() => void openAiTask(task)}
                        >
                          {busy === "task-" + task.source + ":" + task.session_id ? "打开中…" : task.deep_link ? "打开任务" : "定位"}
                        </button>
                        <button
                          className="ghost"
                          disabled={busy !== null || !task.project}
                          onClick={() => void openTaskProject(task.project)}
                        >
                          {busy === "project-" + task.project ? "打开中…" : "项目"}
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {status && (
        <div className="pet-table-block">
          <div className="pet-table-hint">
            最近一分钟：{status.requests_last_minute} 个请求，{status.failed_last_minute} 个失败；本机运行中的 AI 软件 {monitoredTools.length} 个（{status.ai_processes.length} 个进程）。
            「结束」会终止该软件的全部进程树（未保存的工作可能丢失），确认前请核对工具名与进程数。
          </div>
          {monitoredTools.length > 0 ? (
            <div className="pet-table-wrap">
              <table className="pet-table pet-process-table">
                <colgroup>
                  <col />
                  <col style={{ width: 104 }} />
                  <col style={{ width: 88 }} />
                  <col style={{ width: 110 }} />
                  <col style={{ width: 96 }} />
                </colgroup>
                <thead>
                  <tr>
                    <th>AI 软件</th>
                    <th>类型</th>
                    <th>进程</th>
                    <th>内存合计</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {monitoredTools.map((group) => {
                    const totalMemory = group.processes.reduce((sum, process) => sum + (process.memory_kb ?? 0), 0);
                    const hasMemory = group.processes.some((process) => process.memory_kb !== null);
                    return (
                      <tr key={group.toolId}>
                        <td>
                          <strong>{group.label}</strong>
                          <div className="muted mono pet-process-pids">
                            {group.processes.slice(0, 3).map((process) => `PID ${process.pid}`).join(" · ")}
                            {group.processes.length > 3 ? ` 等 ${group.processes.length} 个` : ""}
                          </div>
                        </td>
                        <td>
                          <span className="tag pet-kind-chip">{KIND_LABEL[group.kind] ?? group.kind}</span>
                        </td>
                        <td className="mono">{group.processes.length} 个</td>
                        <td className="mono">{hasMemory ? formatMemory(totalMemory) : "-"}</td>
                        <td>
                          <button
                            className="ghost"
                            disabled={busy !== null}
                            onClick={() => void stopTool(group.toolId, group.label, group.processes.length)}
                          >
                            {busy === `stop-${group.toolId}` ? "结束中…" : "结束"}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          ) : (
            <div className="muted" style={{ fontSize: 12 }}>未检测到运行中的 AI 软件进程（匹配编码 CLI 的可执行名与常见 AI 桌面应用）。</div>
          )}
        </div>
      )}

      <div style={{ marginTop: 14 }}>
        <div className="row">
          <input
            aria-label="宠物 slug"
            placeholder="宠物 slug，例如 snow-plum-lillia"
            value={slug}
            disabled={busy !== null}
            onChange={(event) => setSlug(event.target.value)}
          />
          <button disabled={busy !== null} onClick={() => void installPet()}>
            {busy === "install" ? "安装中…" : "从 Petdex 安装"}
          </button>
          <button disabled={busy !== null} onClick={() => void browseCatalog()}>
            {busy === "catalog" ? "查询中…" : "浏览 Petdex 商店"}
          </button>
        </div>
        {catalog !== null && (
          <div className="code" style={{ marginTop: 10, maxHeight: 220, overflow: "auto" }} data-testid="petdex-catalog">
            {catalog || "（商店没有返回内容）"}
          </div>
        )}
      </div>
    </div>
  );
}
