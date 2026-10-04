import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { api, DetectedTask, PetAsset, PetStatus, PetWindowLayout } from "./api";
import "./pet.css";

const SLUG_STORAGE_KEY = "llm-gateway-pet-slug";
const SCALE_STORAGE_KEY = "llm-gateway-pet-scale";
const EXPANDED_STORAGE_KEY = "llm-gateway-pet-panel-v2";
const BUBBLE_HIDDEN_STORAGE_KEY = "llm-gateway-pet-bubble-hidden";
const DISMISSED_STORAGE_KEY = "llm-gateway-pet-dismissed";
const STATUS_POLL_MS = 1500;
/// 按住超过该时长视为拖动窗口，短按视为点击跳转（与系统托盘一致的直觉）。
const DRAG_HOLD_MS = 160;
/// 按住后只要移动超过这个距离就立即进入拖动，避免轻微抖动让长按失效。
const DRAG_MOVE_THRESHOLD_PX = 4;
/// 窗口停止移动后稍等一下再应用左右布局，避免拖动过程中来回翻转。
const DRAG_SETTLE_MS = 120;
/// 没有收到移动事件时的兜底解冻时间，防止指针抬起事件被系统拖动吞掉。
const DRAG_FALLBACK_MS = 1500;
const PET_BASE_WIDTH = 120;
const PET_BASE_HEIGHT = 130;
/// 与 src-tauri/src/pet_window.rs 的 MIN_SCALE / MAX_SCALE 保持一致（50% ~ 300%）。
const PET_MIN_SCALE = 0.5;
const PET_MAX_SCALE = 3;
/// 与 src-tauri/src/pet_window.rs::MAX_BUBBLES 保持一致：桌宠最多同时显示的气泡数。
const BUBBLE_LIMIT = 3;

type Action = { row: number; delays: number[] };
type Message = { kind: "ok" | "err"; text: string };
type ToolGroup = {
  toolId: string;
  label: string;
  kind: string;
  processCount: number;
  memoryKb: number;
  hasMemory: boolean;
};

type TaskGroup = {
  source: string;
  label: string;
  tasks: DetectedTask[];
  runningCount: number;
};

const STATUS_LABEL: Record<PetStatus["status"], string> = {
  idle: "空闲",
  working: "工作中",
  error: "出错",
};

const TASK_LABEL: Record<DetectedTask["status"], string> = {
  running: "进行中",
  error: "出错",
  done: "已完成",
};

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function actionOf(asset: PetAsset, name: string): Action | null {
  const entry = asset.animations[name] ?? asset.animations.idle;
  if (!entry) return null;
  return { row: entry.row, delays: entry.delays_ms.length ? entry.delays_ms : [200] };
}

function loopingAction(status: PetStatus | null): string {
  if (!status) return "idle";
  if (status.status === "error") return "failed";
  if (status.status === "working") return "running";
  return "idle";
}

function readStoredSlug() {
  try {
    return window.localStorage.getItem(SLUG_STORAGE_KEY);
  } catch {
    return null;
  }
}

function readStoredExpanded() {
  try {
    return window.localStorage.getItem(EXPANDED_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function readStoredBubbleHidden() {
  try {
    return window.localStorage.getItem(BUBBLE_HIDDEN_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

/// 单独关闭过的气泡（任务 key）：任务从列表消失后会自动清理，重新出现时再次显示。
function readStoredDismissed() {
  try {
    const raw = window.localStorage.getItem(DISMISSED_STORAGE_KEY);
    const value = raw ? JSON.parse(raw) : [];
    return Array.isArray(value)
      ? value.filter((item): item is string => typeof item === "string")
      : [];
  } catch {
    return [];
  }
}

function readStoredScale() {
  try {
    const value = Number(window.localStorage.getItem(SCALE_STORAGE_KEY));
    return Number.isFinite(value) && value >= PET_MIN_SCALE
      ? Math.min(PET_MAX_SCALE, value)
      : 1;
  } catch {
    return 1;
  }
}

function taskKey(task: DetectedTask) {
  return `${task.source}:${task.session_id}`;
}

function timeAgo(timestamp: number) {
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - timestamp));
  if (seconds < 45) return "刚刚";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}

export default function Pet() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const assetRef = useRef<PetAsset | null>(null);
  const actionRef = useRef<Action | null>(null);
  const transientRef = useRef<{ action: Action; framesLeft: number; nextAt: number } | null>(null);
  const cursorRef = useRef({ frame: 0, nextAt: 0 });
  const dragTimerRef = useRef<number | null>(null);
  const dragSettleTimerRef = useRef<number | null>(null);
  const dragPointerIdRef = useRef<number | null>(null);
  const dragOriginRef = useRef<{ x: number; y: number } | null>(null);
  const dragStartedRef = useRef(false);
  const draggingRef = useRef(false);
  const layoutRef = useRef<PetWindowLayout | null>(null);
  const previousStatusRef = useRef<string | null>(null);
  const slugRef = useRef<string | null>(null);
  const expandedRef = useRef(readStoredExpanded());

  const [asset, setAsset] = useState<PetAsset | null>(null);
  const [status, setStatus] = useState<PetStatus | null>(null);
  const [layout, setLayout] = useState<PetWindowLayout | null>(null);
  const [expanded, setExpanded] = useState(expandedRef.current);
  const [slug, setSlug] = useState<string | null>(readStoredSlug);
  const [paused, setPaused] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [message, setMessage] = useState<Message | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [selectedTaskKey, setSelectedTaskKey] = useState<string | null>(null);
  const [bubbleHidden, setBubbleHidden] = useState(readStoredBubbleHidden);
  const [dismissed, setDismissed] = useState<string[]>(readStoredDismissed);
  const [bubblesExpanded, setBubblesExpanded] = useState(false);
  /// 鼠标当前停在哪个气泡上：只有那个气泡显示自己的关闭按钮。
  const [hoveredKey, setHoveredKey] = useState<string | null>(null);
  const hoverRef = useRef(false);
  const collapseTimerRef = useRef<number | null>(null);
  const taskStatusRef = useRef<Record<string, string>>({});

  /// 拖动期间冻结左右气泡方向；拖动停止后再一次性应用，避免窗口跟随鼠标时来回翻转。
  const commitLayout = useCallback((next: PetWindowLayout) => {
    const current = layoutRef.current;
    const effective =
      draggingRef.current && current
        ? { ...next, bubble_left: current.bubble_left }
        : next;
    layoutRef.current = effective;
    setLayout(effective);
  }, []);

  const clearDragSettleTimer = useCallback(() => {
    if (dragSettleTimerRef.current !== null) {
      window.clearTimeout(dragSettleTimerRef.current);
      dragSettleTimerRef.current = null;
    }
  }, []);

  const settleDrag = useCallback(
    (delay = DRAG_SETTLE_MS) => {
      clearDragSettleTimer();
      dragSettleTimerRef.current = window.setTimeout(() => {
        dragSettleTimerRef.current = null;
        draggingRef.current = false;
        void api
          .getPetWindowLayout()
          .then(commitLayout)
          .catch(() => undefined);
      }, delay);
    },
    [clearDragSettleTimer, commitLayout],
  );

  const toggleExpanded = useCallback((next?: boolean) => {
    const value = next ?? !expandedRef.current;
    expandedRef.current = value;
    setExpanded(value);
    if (!value) {
      setBubbleHidden(false);
      try {
        window.localStorage.setItem(BUBBLE_HIDDEN_STORAGE_KEY, "false");
      } catch {
        // 受限 WebView 中仅当前会话生效。
      }
    }
    try {
      window.localStorage.setItem(EXPANDED_STORAGE_KEY, String(value));
    } catch {
      // 受限 WebView 中仅当前会话生效。
    }
    void api.setPetWindowExpanded(value).catch((error) => {
      setMessage({ kind: "err", text: `调整面板失败：${errorText(error)}` });
    });
  }, []);

  const hideBubble = useCallback(() => {
    setBubbleHidden(true);
    try {
      window.localStorage.setItem(BUBBLE_HIDDEN_STORAGE_KEY, "true");
    } catch {
      // 受限 WebView 中仅当前会话生效。
    }
    void api.setPetWindowBubbleHidden(true).catch((error) => {
      setMessage({ kind: "err", text: `隐藏任务气泡失败：${errorText(error)}` });
    });
  }, []);

  const showBubble = useCallback(() => {
    setBubbleHidden(false);
    // 任务徽标 = “把气泡放回来”：连同之前单独关闭的气泡一起恢复。
    setDismissed([]);
    try {
      window.localStorage.setItem(BUBBLE_HIDDEN_STORAGE_KEY, "false");
      window.localStorage.removeItem(DISMISSED_STORAGE_KEY);
    } catch {
      // 受限 WebView 中仅当前会话生效。
    }
    void api.setPetWindowBubbleHidden(false).catch((error) => {
      setMessage({ kind: "err", text: `恢复任务气泡失败：${errorText(error)}` });
    });
  }, []);

  // 原生菜单动作通过事件回到这里；面板展开状态同时同步给后端窗口布局。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen<{ action: string; slug?: string }>("pet-menu-action", (event) => {
      const payload = event.payload;
      if (payload.action === "toggle-panel") {
        toggleExpanded();
      }
      if (payload.action === "toggle-pause") {
        setPaused((value) => !value);
      }
      if (payload.action === "select-pet" && payload.slug) {
        setSlug(payload.slug);
        try {
          window.localStorage.setItem(SLUG_STORAGE_KEY, payload.slug);
        } catch {
          // 受限 WebView 中仅当前会话生效。
        }
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [toggleExpanded]);

  // 窗口布局由 Rust 侧统一计算；这里只在挂载和布局事件时同步，避免左右两边各算一套尺寸。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void api
      .getPetWindowLayout()
      .then((next) => {
        if (disposed) return;
        commitLayout(next);
        const stored = readStoredExpanded();
        expandedRef.current = stored;
        setExpanded(stored);
        if (stored !== next.expanded) {
          void api.setPetWindowExpanded(stored).catch((error) => {
            if (!disposed) setMessage({ kind: "err", text: `恢复面板状态失败：${errorText(error)}` });
          });
        }
        const storedBubbleHidden = stored ? false : readStoredBubbleHidden();
        setBubbleHidden(storedBubbleHidden);
        if (storedBubbleHidden !== next.bubble_hidden) {
          void api.setPetWindowBubbleHidden(storedBubbleHidden).catch((error) => {
            if (!disposed) setMessage({ kind: "err", text: `恢复任务气泡状态失败：${errorText(error)}` });
          });
        }
      })
      .catch((error) => {
        if (!disposed) setMessage({ kind: "err", text: `读取窗口布局失败：${errorText(error)}` });
      });
    void listen<PetWindowLayout>("pet-layout-changed", (event) => {
      if (disposed) return;
      commitLayout(event.payload);
      expandedRef.current = event.payload.expanded;
      setExpanded(event.payload.expanded);
      setBubbleHidden(event.payload.bubble_hidden);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [commitLayout]);

  // 窗口移动到屏幕右侧时，后端重新计算 bubble_left；移动事件做 80ms 防抖。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let timer: number | null = null;
    void getCurrentWindow()
      .onMoved(() => {
        if (draggingRef.current) {
          settleDrag();
          return;
        }
        if (timer !== null) window.clearTimeout(timer);
        timer = window.setTimeout(() => {
          timer = null;
          void api
            .getPetWindowLayout()
            .then((next) => {
              if (!disposed) commitLayout(next);
            })
            .catch(() => undefined);
        }, 80);
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      if (timer !== null) window.clearTimeout(timer);
      unlisten?.();
    };
  }, [commitLayout, settleDrag]);

  // 状态轮询：驱动工作 / 空闲 / 错误三种动画、HUD 与悬浮提示。
  // 同时以 localStorage 为准同步主窗口选择的宠物（同一 origin 的两个窗口共享）。
  useEffect(() => {
    if (paused) return;
    let disposed = false;
    const poll = async () => {
      try {
        const next = await api.getPetStatus();
        if (disposed) return;
        setStatus(next);
        try {
          const stored = window.localStorage.getItem(SLUG_STORAGE_KEY);
          if (stored && stored !== slugRef.current) {
            slugRef.current = stored;
            setSlug(stored);
          }
        } catch {
          // 受限 WebView 中忽略。
        }
      } catch (error) {
        if (!disposed) setLoadError(String(error));
      }
    };
    void poll();
    const timer = window.setInterval(poll, STATUS_POLL_MS);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [paused]);

  useEffect(() => {
    slugRef.current = slug;
  }, [slug]);

  // 未指定宠物时，自动选择第一个已安装的宠物。
  useEffect(() => {
    if (slug || !status) return;
    const first = status.installed_pets[0];
    if (!first) return;
    setSlug(first.slug);
    try {
      window.localStorage.setItem(SLUG_STORAGE_KEY, first.slug);
    } catch {
      // 受限 WebView 中仅当前会话生效。
    }
  }, [slug, status]);

  // 加载宠物资源（精灵图以 data URL 传递，不暴露文件路径）。
  useEffect(() => {
    if (!slug) return;
    let disposed = false;
    setLoadError(null);
    void api
      .getPetAsset(slug)
      .then((next) => {
        if (disposed) return;
        const image = new Image();
        image.onload = () => {
          if (disposed) return;
          imageRef.current = image;
          assetRef.current = next;
          setAsset(next);
          cursorRef.current = { frame: 0, nextAt: 0 };
        };
        image.onerror = () => {
          if (!disposed) setLoadError("宠物精灵图无法解码");
        };
        image.src = next.spritesheet_data_url;
      })
      .catch((error) => {
        if (!disposed) setLoadError(String(error));
      });
    return () => {
      disposed = true;
    };
  }, [slug]);

  // 状态切换时播放一次跳跃，让「任务联动」有可见反馈。
  useEffect(() => {
    const current = status?.status ?? null;
    if (assetRef.current && current && previousStatusRef.current && current !== previousStatusRef.current) {
      const jump = actionOf(assetRef.current, "jumping");
      if (jump) {
        transientRef.current = { action: jump, framesLeft: jump.delays.length, nextAt: 0 };
      }
    }
    previousStatusRef.current = current;
  }, [status]);

  // 帧动画：按宠物包自带的逐帧延迟推进，用时间戳计算以适配任意刷新率。
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const context = canvas.getContext("2d");
    if (!context) return;
    let raf = 0;

    const draw = (now: number) => {
      raf = window.requestAnimationFrame(draw);
      const image = imageRef.current;
      const currentAsset = assetRef.current;
      if (!image || !currentAsset) return;

      const looping = actionOf(currentAsset, loopingAction(status));
      if (looping) actionRef.current = looping;

      const transient = transientRef.current;
      const action = transient ? transient.action : actionRef.current;
      if (!action) return;

      const cursor = cursorRef.current;
      if (now >= cursor.nextAt) {
        const advancing = transient ?? null;
        if (advancing) {
          advancing.framesLeft -= 1;
          if (advancing.framesLeft <= 0) transientRef.current = null;
        }
        const frameCount = action.delays.length;
        cursor.frame = (cursor.frame + 1) % frameCount;
        cursor.nextAt = now + action.delays[Math.min(cursor.frame, frameCount - 1)];
      }

      const dpr = window.devicePixelRatio || 1;
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
        canvas.width = Math.round(width * dpr);
        canvas.height = Math.round(height * dpr);
      }
      context.setTransform(dpr, 0, 0, dpr, 0, 0);
      context.clearRect(0, 0, width, height);

      const scale = Math.min(width / currentAsset.cell_width, height / currentAsset.cell_height);
      const drawWidth = currentAsset.cell_width * scale;
      const drawHeight = currentAsset.cell_height * scale;
      const frameIndex = Math.min(cursor.frame, action.delays.length - 1);
      context.drawImage(
        image,
        frameIndex * currentAsset.cell_width,
        action.row * currentAsset.cell_height,
        currentAsset.cell_width,
        currentAsset.cell_height,
        (width - drawWidth) / 2,
        height - drawHeight,
        drawWidth,
        drawHeight,
      );
    };

    raf = window.requestAnimationFrame(draw);
    return () => window.cancelAnimationFrame(raf);
  }, [status, asset, paused]);

  const clearDragTimer = useCallback(() => {
    if (dragTimerRef.current !== null) {
      window.clearTimeout(dragTimerRef.current);
      dragTimerRef.current = null;
    }
  }, []);

  const startDrag = useCallback(() => {
    if (dragStartedRef.current) return;
    clearDragTimer();
    dragStartedRef.current = true;
    draggingRef.current = true;
    settleDrag(DRAG_FALLBACK_MS);
    // 拖动能力在极少数环境不可用时静默降级：窗口不跟随，但不能变成未捕获异常。
    void getCurrentWindow()
      .startDragging()
      .catch(() => {
        draggingRef.current = false;
        clearDragSettleTimer();
      });
  }, [clearDragSettleTimer, clearDragTimer, settleDrag]);

  const handlePointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.button !== 0 || dragPointerIdRef.current !== null) return;
      dragPointerIdRef.current = event.pointerId;
      dragOriginRef.current = { x: event.clientX, y: event.clientY };
      dragStartedRef.current = false;
      try {
        event.currentTarget.setPointerCapture(event.pointerId);
      } catch {
        // 某些 WebView 版本不提供 pointer capture；后面的拖动阈值仍然有效。
      }
      dragTimerRef.current = window.setTimeout(startDrag, DRAG_HOLD_MS);
    },
    [startDrag],
  );

  const handlePointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (dragPointerIdRef.current !== event.pointerId || dragStartedRef.current) return;
      const origin = dragOriginRef.current;
      if (!origin) return;
      if (Math.hypot(event.clientX - origin.x, event.clientY - origin.y) >= DRAG_MOVE_THRESHOLD_PX) {
        startDrag();
      }
    },
    [startDrag],
  );

  const handlePointerEnd = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (dragPointerIdRef.current !== event.pointerId) return;
      clearDragTimer();
      dragPointerIdRef.current = null;
      dragOriginRef.current = null;
      try {
        event.currentTarget.releasePointerCapture(event.pointerId);
      } catch {
        // pointer capture 可能已由系统拖动提前释放。
      }
      if (draggingRef.current) settleDrag();
    },
    [clearDragTimer, settleDrag],
  );

  const handlePointerCaptureLost = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (dragPointerIdRef.current !== event.pointerId) return;
      clearDragTimer();
      dragPointerIdRef.current = null;
      dragOriginRef.current = null;
      // 系统原生拖动可能立即接管并释放 pointer capture；此时不能让布局提前解冻。
    },
    [clearDragTimer],
  );

  const handleClick = useCallback(() => {
    // 只有未真正启动拖动才算点击；pointerup 清理定时器不影响这个标志。
    const wasShortPress = !dragStartedRef.current;
    clearDragTimer();
    if (!wasShortPress) return;
    void api.focusMainWindow("stats").catch((error) => setMessage({ kind: "err", text: errorText(error) }));
  }, [clearDragTimer]);

  const runAction = useCallback(async (key: string, action: () => Promise<string>) => {
    setBusyAction(key);
    setMessage(null);
    try {
      setMessage({ kind: "ok", text: await action() });
    } catch (error) {
      setMessage({ kind: "err", text: errorText(error) });
    } finally {
      setBusyAction(null);
    }
  }, []);

  const toolGroups = useMemo<ToolGroup[]>(() => {
    const groups = new Map<string, ToolGroup>();
    for (const process of status?.ai_processes ?? []) {
      const group = groups.get(process.tool_id) ?? {
        toolId: process.tool_id,
        label: process.tool_label,
        kind: process.kind,
        processCount: 0,
        memoryKb: 0,
        hasMemory: false,
      };
      group.processCount += 1;
      if (process.memory_kb !== null) {
        group.hasMemory = true;
        group.memoryKb += process.memory_kb;
      }
      groups.set(process.tool_id, group);
    }
    return Array.from(groups.values()).sort((a, b) => a.label.localeCompare(b.label, "zh-CN"));
  }, [status?.ai_processes]);

  const toolIds = useMemo(() => new Set(toolGroups.map((group) => group.toolId)), [toolGroups]);
  const tasks = status?.active_tasks ?? [];
  // 每个任务一个气泡：单独关闭过的先过滤掉，顺序与后端窗口高度计算保持一致（最近活动在前）。
  const bubbleTasks = tasks
    .filter((task) => !dismissed.includes(taskKey(task)))
    .slice(0, BUBBLE_LIMIT);

  // 上报可见气泡数量与堆叠状态：关闭单个气泡后窗口立刻缩回，展开时加高。
  useEffect(() => {
    if (expanded || bubbleHidden) return;
    void api
      .setPetBubbles(bubbleTasks.length, bubblesExpanded)
      .then(commitLayout)
      .catch(() => undefined);
  }, [bubbleTasks.length, bubblesExpanded, expanded, bubbleHidden, commitLayout]);

  // 任务列表变化时清理已消失任务的关闭记录：任务重新出现要能再次提示。
  useEffect(() => {
    if (dismissed.length === 0) return;
    const alive = new Set(tasks.map((task) => taskKey(task)));
    const next = dismissed.filter((key) => alive.has(key));
    if (next.length !== dismissed.length) {
      setDismissed(next);
      try {
        window.localStorage.setItem(DISMISSED_STORAGE_KEY, JSON.stringify(next));
      } catch {
        // 受限 WebView 中仅当前会话生效。
      }
    }
  }, [tasks, dismissed]);

  // 任务结束（完成 / 出错 / 被停止）时自动展开堆叠，提醒用户看一眼；10 秒后自动收起。
  useEffect(() => {
    const next: Record<string, string> = {};
    let finished = false;
    for (const task of bubbleTasks) {
      const key = taskKey(task);
      next[key] = task.status;
      const previous = taskStatusRef.current[key];
      if (previous !== undefined && previous !== task.status && task.status !== "running") {
        finished = true;
      }
    }
    taskStatusRef.current = next;
    if (!finished) return;
    setBubblesExpanded(true);
    if (collapseTimerRef.current !== null) window.clearTimeout(collapseTimerRef.current);
    collapseTimerRef.current = window.setTimeout(() => {
      collapseTimerRef.current = null;
      if (!hoverRef.current) setBubblesExpanded(false);
    }, 10000);
  }, [bubbleTasks.map((task) => `${taskKey(task)}:${task.status}`).join("|")]);

  useEffect(() => () => {
    if (collapseTimerRef.current !== null) window.clearTimeout(collapseTimerRef.current);
  }, []);

  // 所有气泡都被单独关闭后，窗口缩回宠物本体；宠物右上角的任务徽标可恢复。
  useEffect(() => {
    if (expanded || bubbleHidden) return;
    if (tasks.length > 0 && bubbleTasks.length === 0) hideBubble();
  }, [bubbleTasks.length, tasks.length, expanded, bubbleHidden, hideBubble]);
  const taskGroups = useMemo<TaskGroup[]>(() => {
    const groups = new Map<string, TaskGroup>();
    for (const task of tasks) {
      const group = groups.get(task.source) ?? {
        source: task.source,
        label: task.source_label,
        tasks: [],
        runningCount: 0,
      };
      group.tasks.push(task);
      if (task.status === "running") group.runningCount += 1;
      groups.set(task.source, group);
    }
    return Array.from(groups.values()).sort((a, b) => a.label.localeCompare(b.label, "zh-CN"));
  }, [tasks]);
  const scale = layout?.scale ?? readStoredScale();
  const bubbleLeft = Boolean(layout?.bubble_left) && !expanded && !bubbleHidden;
  const rootStyle = {
    "--pet-width": `${PET_BASE_WIDTH * scale}px`,
    "--pet-height": `${PET_BASE_HEIGHT * scale}px`,
  } as CSSProperties;

  const tooltip = [
    asset ? asset.display_name : status?.installed_pets.length ? "未选择宠物" : "还没有安装宠物",
    status ? `[${status.status === "working" ? "工作中" : status.status === "error" ? "出错" : "空闲"}] ${status.reason}` : null,
    tasks.length ? `当前任务：${tasks.map((task) => `[${task.source_label}] ${task.title || task.detail}`).join("；")}` : null,
    toolGroups.length ? `运行中：${toolGroups.map((group) => group.label).join("、")}` : null,
    "单击宠物跳转主窗口；按住拖动；右键打开菜单",
    loadError,
  ]
    .filter(Boolean)
    .join("\n");

  const showMenu = useCallback(() => {
    void api
      .showPetMenu(slugRef.current, paused, expandedRef.current)
      .catch((error) => setMessage({ kind: "err", text: `打开菜单失败：${errorText(error)}` }));
  }, [paused]);

  const stopTool = (group: ToolGroup) => {
    if (!window.confirm(`将结束 ${group.label} 当前检测到的 ${group.processCount} 个进程（含子进程）。\n\n未保存的工作可能丢失；结束 IDE 等同关闭该应用。确定继续吗？`)) return;
    void runAction(`stop-${group.toolId}`, () => api.stopAiTool(group.toolId));
  };

  /// 点气泡上的方块 = 停止该任务对应的软件进程（与 HUD「结束」同一动作，仍要确认）。
  const stopTask = (task: DetectedTask) => {
    const group = toolGroups.find((item) => item.toolId === task.tool_id);
    if (group) {
      stopTool(group);
      return;
    }
    if (!window.confirm(`将结束 ${task.source_label} 当前检测到的进程（含子进程）。\n\n未保存的工作可能丢失；确定继续吗？`)) return;
    void runAction(`stop-${task.tool_id}`, () => api.stopAiTool(task.tool_id));
  };

  const openTask = (task: DetectedTask) => {
    void runAction(`open-task-${taskKey(task)}`, () => api.openAiTask(task.tool_id, task.session_id));
  };

  /// 关闭单个气泡：记录任务 key，任务从列表消失后自动清理。
  const dismissBubble = (task: DetectedTask) => {
    const key = taskKey(task);
    setDismissed((previous) => {
      if (previous.includes(key)) return previous;
      const next = [...previous, key];
      try {
        window.localStorage.setItem(DISMISSED_STORAGE_KEY, JSON.stringify(next));
      } catch {
        // 受限 WebView 中仅当前会话生效。
      }
      return next;
    });
  };

  /// 鼠标悬浮时展开堆叠，离开后收起（单个气泡不需要收起）。
  const handleBubbleHover = (hovering: boolean) => () => {
    hoverRef.current = hovering;
    // 关闭按钮跟随“鼠标最近进入的那个气泡”；展开会让气泡移动，所以只在离开整个堆叠区时清空。
    if (!hovering) setHoveredKey(null);
    if (bubbleTasks.length <= 1) return;
    if (hovering && collapseTimerRef.current !== null) {
      window.clearTimeout(collapseTimerRef.current);
      collapseTimerRef.current = null;
    }
    setBubblesExpanded(hovering);
  };

  return (
    <div
      className={`pet-root ${expanded ? "is-expanded" : bubbleHidden ? "is-pet-only" : "is-bubble"}${bubbleLeft ? " is-bubble-left" : ""}`}
      style={rootStyle}
      data-testid="pet-root"
      title={tooltip}
      onContextMenu={(event) => {
        event.preventDefault();
        showMenu();
      }}
    >
      <div
        className="pet-stage"
        data-testid="pet-stage"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerEnd}
        onPointerCancel={handlePointerEnd}
        onLostPointerCapture={handlePointerCaptureLost}
        onClick={handleClick}
      >
        <canvas ref={canvasRef} className="pet-canvas" aria-hidden="true" />
        {!asset && (
          <div className="pet-placeholder" data-testid="pet-placeholder">
            {loadError ? "宠物不可用" : status?.installed_pets.length ? "正在加载宠物…" : "没有宠物"}
          </div>
        )}
        {!expanded && bubbleHidden && (
          <button
            className="pet-task-badge"
            data-testid="pet-task-badge"
            title="显示任务气泡"
            onPointerDown={(event) => event.stopPropagation()}
            onPointerUp={(event) => event.stopPropagation()}
            onClick={(event) => {
              event.stopPropagation();
              showBubble();
            }}
          >
            任务{tasks.length ? ` ${tasks.length}` : ""}
          </button>
        )}
        {paused && <span className="pet-paused-badge">已暂停</span>}
      </div>

      {!expanded && !bubbleHidden && (
        <aside
          className={`pet-bubble-stack${bubbleTasks.length > 1 && !bubblesExpanded ? " is-compact" : ""}`}
          data-testid="pet-bubble"
          data-bubbles-expanded={bubblesExpanded ? "true" : "false"}
          onMouseEnter={handleBubbleHover(true)}
          onMouseLeave={handleBubbleHover(false)}
        >
          <div className="pet-bubble-actions">
            <button
              type="button"
              className="pet-bubble-expand"
              data-testid="pet-bubble-expand"
              title="展开任务面板"
              onPointerDown={(event) => event.stopPropagation()}
              onClick={(event) => {
                event.stopPropagation();
                toggleExpanded(true);
              }}
            >
              {/* Windows 风格的“放大窗口”图标。 */}
              <svg viewBox="0 0 12 12" aria-hidden="true" focusable="false">
                <rect
                  x="1.6"
                  y="1.6"
                  width="8.8"
                  height="8.8"
                  rx="1.6"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.3"
                />
              </svg>
            </button>
          </div>
          {bubbleTasks.length > 0 ? (
            bubbleTasks.map((task, index) => {
              // 收起时最上面（最近）的气泡在最前，展开后尾巴挂在最下面那个。
              const tail =
                bubbleTasks.length === 1 ||
                index === (bubblesExpanded ? bubbleTasks.length - 1 : 0);
              return (
                <article
                  key={taskKey(task)}
                  className={`pet-bubble${tail ? " is-tail" : ""}${task.status === "running" ? " is-running" : ""}`}
                  data-testid="pet-bubble-item"
                  title={`${task.source_label} · ${task.title || task.detail}`}
                  onMouseEnter={() => setHoveredKey(taskKey(task))}
                  onClick={() => openTask(task)}
                >
                  <div className="pet-bubble-title">
                    <span className="pet-bubble-source">{task.source_label}</span>
                    <strong>{task.title || task.detail}</strong>
                  </div>
                  <div className="pet-bubble-preview">{task.last_message || task.detail}</div>
                  {task.status === "running" ? (
                    // 运行中显示实心方块：点它结束该软件当前检测到的进程（含子进程）。
                    <button
                      type="button"
                      className="pet-bubble-status running is-action"
                      data-testid="pet-bubble-status"
                      title={`停止任务：结束 ${task.source_label} 当前检测到的进程`}
                      onPointerDown={(event) => event.stopPropagation()}
                      onClick={(event) => {
                        event.stopPropagation();
                        stopTask(task);
                      }}
                    >
                      <span className="pet-bubble-stop-square" aria-hidden="true" />
                    </button>
                  ) : (
                    <span
                      className={`pet-bubble-status ${task.status}`}
                      data-testid="pet-bubble-status"
                      title={TASK_LABEL[task.status]}
                    >
                      {task.status === "done" ? "✓" : "!"}
                    </span>
                  )}
                  <button
                    type="button"
                    className={`pet-bubble-dismiss${hoveredKey === taskKey(task) ? " is-visible" : ""}`}
                    data-testid="pet-bubble-dismiss"
                    title="关闭这个气泡"
                    onPointerDown={(event) => event.stopPropagation()}
                    onClick={(event) => {
                      event.stopPropagation();
                      dismissBubble(task);
                    }}
                  >
                    ×
                  </button>
                </article>
              );
            })
          ) : (
            <article className="pet-bubble is-tail" data-testid="pet-bubble-item">
              <div className="pet-bubble-title">
                <strong>暂无进行中的任务</strong>
              </div>
              <div className="pet-bubble-preview">{status?.reason ?? "正在读取状态…"}</div>
            </article>
          )}
        </aside>
      )}

      {expanded && (
        <aside className="pet-hud" data-testid="pet-hud">
          <header className="hud-header" data-tauri-drag-region>
            <div className="hud-status" data-testid="pet-hud-status">
              <span className={`status-dot ${status?.status ?? "idle"}`} />
              <strong>{STATUS_LABEL[status?.status ?? "idle"]}</strong>
              <span className="muted">{toolGroups.length} 软件 · {tasks.length} 任务</span>
            </div>
            <div className="hud-header-actions">
              <button type="button" data-testid="pet-menu-button" onClick={showMenu}>菜单</button>
              <button type="button" data-testid="pet-collapse-button" onClick={() => toggleExpanded(false)}>收起</button>
            </div>
          </header>

          <div className="hud-reason">{status?.reason ?? "正在读取状态…"}</div>
          {paused && <div className="hud-banner">监控已暂停；右键或菜单可恢复。</div>}

          <section className="hud-section" data-testid="pet-task-section">
            <div className="hud-section-title">
              <span>当前任务</span>
              <span className="muted">{tasks.length} 个 · {taskGroups.length} 个来源</span>
            </div>
            {tasks.length > 0 ? (
              taskGroups.map((taskGroup) => (
                <div className="task-group" data-testid="pet-task-group" key={taskGroup.source}>
                  <div className="task-group-title">
                    <span className="source-chip" data-testid="pet-task-source">{taskGroup.label}</span>
                    <span className="muted">{taskGroup.tasks.length} 个任务</span>
                    {taskGroup.runningCount > 0 && <span className="running-count">{taskGroup.runningCount} 进行中</span>}
                  </div>
                  {taskGroup.tasks.slice(0, 5).map((task) => {
                    const toolGroup = toolGroups.find((item) => item.toolId === task.tool_id);
                    const selected = taskKey(task) === selectedTaskKey;
                    const canOpen = Boolean(task.deep_link) || task.tool_id.startsWith("qoder");
                    return (
                      <article
                        className={`task-row task-${task.status} ${selected ? "task-selected" : ""}`}
                        data-testid="pet-task-row"
                        key={taskKey(task)}
                        onClick={() => setSelectedTaskKey(selected ? null : taskKey(task))}
                      >
                        <div className="task-head">
                          <span className={`status-chip ${task.status}`}>{TASK_LABEL[task.status]}</span>
                          <span className="task-time">{timeAgo(task.updated_at)}</span>
                        </div>
                        <div className="task-title">{task.title || task.detail}</div>
                        <div className="task-detail">{task.detail}</div>
                        {selected && (
                          <div className="task-expanded" data-testid="pet-task-detail">
                            <div className="task-content-label">最近内容</div>
                            <div className="task-content">{task.last_message || task.detail}</div>
                            <div className="task-project" title={task.project}>{task.project || "（未提供项目路径）"}</div>
                            <div className="task-actions">
                              <button
                                type="button"
                                disabled={busyAction !== null || !canOpen}
                                title={task.deep_link ? "在对应 AI 软件中打开该任务" : "定位对应 AI 软件窗口"}
                                onClick={(event) => {
                                  event.stopPropagation();
                                  openTask(task);
                                }}
                              >
                                {busyAction === `open-task-${taskKey(task)}` ? "打开中…" : task.deep_link ? "打开任务" : "定位"}
                              </button>
                              <button
                                type="button"
                                disabled={busyAction !== null || !task.project}
                                onClick={(event) => {
                                  event.stopPropagation();
                                  void runAction(`project-${task.session_id}`, () => api.openTaskProject(task.project));
                                }}
                              >
                                {busyAction === `project-${task.session_id}` ? "打开中…" : "项目"}
                              </button>
                              <button
                                type="button"
                                className="danger"
                                disabled={busyAction !== null || !toolGroup}
                                onClick={(event) => {
                                  event.stopPropagation();
                                  if (toolGroup) stopTool(toolGroup);
                                }}
                              >
                                {toolGroup && busyAction === `stop-${toolGroup.toolId}` ? "结束中…" : "结束"}
                              </button>
                            </div>
                          </div>
                        )}
                      </article>
                    );
                  })}
                </div>
              ))
            ) : (
              <div className="hud-empty">未检测到进行中的任务；会持续监控 Codex 与 Qoder CLI 任务日志。</div>
            )}
          </section>

          <section className="hud-section" data-testid="pet-tools-section">
            <div className="hud-section-title">
              <span>运行中的 AI 软件</span>
              <span className="muted">{toolGroups.length}</span>
            </div>
            {toolGroups.length > 0 ? (
              toolGroups.slice(0, 6).map((group) => (
                <div className="tool-row" data-testid="pet-tool-row" key={group.toolId}>
                  <span className="source-chip">{group.label}</span>
                  <span className="muted">{group.processCount} 进程</span>
                  <div className="tool-actions">
                    <button
                      type="button"
                      disabled={busyAction !== null}
                      onClick={() => void runAction(`focus-${group.toolId}`, () => api.focusAiTool(group.toolId))}
                    >
                      定位
                    </button>
                    <button
                      type="button"
                      className="danger"
                      disabled={busyAction !== null}
                      onClick={() => stopTool(group)}
                    >
                      结束
                    </button>
                  </div>
                </div>
              ))
            ) : (
              <div className="hud-empty">未检测到运行中的 AI 软件。</div>
            )}
          </section>

          {message && (
            <div className={`hud-message ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>
              {message.text}
            </div>
          )}
          {loadError && <div className="hud-message err" role="alert">{loadError}</div>}
        </aside>
      )}
    </div>
  );
}
