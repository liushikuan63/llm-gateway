import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { api, PetAsset, PetStatus } from "./api";
import "./pet.css";

const SLUG_STORAGE_KEY = "llm-gateway-pet-slug";
const STATUS_POLL_MS = 1500;
/// 按住超过该时长视为拖动窗口，短按视为点击跳转（与系统托盘一致的直觉）。
const DRAG_HOLD_MS = 160;

type Action = { row: number; delays: number[] };

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

export default function Pet() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const assetRef = useRef<PetAsset | null>(null);
  const actionRef = useRef<Action | null>(null);
  const transientRef = useRef<{ action: Action; framesLeft: number; nextAt: number } | null>(null);
  const cursorRef = useRef({ frame: 0, nextAt: 0 });
  const dragTimerRef = useRef<number | null>(null);
  const previousStatusRef = useRef<string | null>(null);
  const slugRef = useRef<string | null>(null);

  const [asset, setAsset] = useState<PetAsset | null>(null);
  const [status, setStatus] = useState<PetStatus | null>(null);
  const [slug, setSlug] = useState<string | null>(() => {
    try {
      return window.localStorage.getItem(SLUG_STORAGE_KEY);
    } catch {
      return null;
    }
  });
  const [paused, setPaused] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  // 原生菜单的动作通过事件回到这里（菜单本身由 Rust 侧弹出）。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen<{ action: string; slug?: string }>("pet-menu-action", (event) => {
      const payload = event.payload;
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
  }, []);

  // 状态轮询：驱动工作 / 空闲 / 错误三种动画与悬浮提示。
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

  const handlePointerDown = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    dragTimerRef.current = window.setTimeout(() => {
      dragTimerRef.current = null;
      // 拖动能力在极少数环境不可用时静默降级：窗口不跟随，但不能变成未捕获异常。
      void getCurrentWindow().startDragging().catch(() => undefined);
    }, DRAG_HOLD_MS);
  }, []);

  const clearDragTimer = useCallback(() => {
    if (dragTimerRef.current !== null) {
      window.clearTimeout(dragTimerRef.current);
      dragTimerRef.current = null;
    }
  }, []);

  const handleClick = useCallback(() => {
    // 只有短按（拖动计时器尚未触发）才算点击；长按会启动窗口拖动，计时器已被清空。
    const wasShortPress = dragTimerRef.current !== null;
    clearDragTimer();
    if (!wasShortPress) return;
    void api.focusMainWindow("stats").catch((error) => setLoadError(String(error)));
  }, [clearDragTimer]);

  const installed = status?.installed_pets ?? [];
  const tooltip = [
    asset ? `${asset.display_name}` : installed.length ? "未选择宠物" : "还没有安装宠物",
    status ? `${status.status === "working" ? "工作中" : status.status === "error" ? "出错" : "空闲"} · ${status.reason}` : null,
    status && status.active_tasks.length ? `任务：${status.active_tasks[0].source_label} ${status.active_tasks[0].detail}` : null,
    status && status.ai_processes.length
      ? `运行中：${Array.from(new Set(status.ai_processes.map((process) => process.tool_label))).join("、")}`
      : null,
    "单击跳转主窗口；按住拖动；右键打开菜单",
    loadError,
  ]
    .filter(Boolean)
    .join("\n");

  return (
    <div
      className="pet-root"
      data-testid="pet-root"
      title={tooltip}
      onPointerDown={handlePointerDown}
      onPointerLeave={clearDragTimer}
      onClick={handleClick}
      onContextMenu={(event) => {
        event.preventDefault();
        // 原生菜单：桌宠窗口可能很小，页面内菜单会显示不全。
        void api.showPetMenu(slugRef.current, paused).catch((error) => setLoadError(String(error)));
      }}
    >
      <canvas ref={canvasRef} className="pet-canvas" aria-hidden="true" />
      {!asset && (
        <div className="pet-placeholder" data-testid="pet-placeholder">
          {loadError ? "宠物不可用" : installed.length ? "正在加载宠物…" : "没有宠物"}
        </div>
      )}
    </div>
  );
}
