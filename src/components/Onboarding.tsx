import { useEffect, useId, useRef, useState } from "react";
import "./onboarding.css";

export type OnboardingTarget = "providers" | "sessions" | "stats" | "settings";

export type OnboardingProps = {
  open: boolean;
  onSkip: () => void;
  onComplete: () => void;
  onNavigate: (page: OnboardingTarget) => void;
};

type OnboardingStep = {
  id: "provider" | "models" | "test" | "backup" | "review";
  eyebrow: string;
  title: string;
  description: string;
  detail: string;
  target?: OnboardingTarget;
  actionLabel?: string;
};

const STEPS: OnboardingStep[] = [
  {
    id: "provider",
    eyebrow: "第一步：建立上游连接",
    title: "添加一个供应商",
    description: "从供应商页新建兼容协议、服务地址和 API Key，先让网关知道请求可以交给谁。",
    detail: "此引导不会保存供应商、填写凭据或发起网络请求。密钥仅在你主动保存后由桌面应用处理。",
    target: "providers",
    actionLabel: "前往供应商页面",
  },
  {
    id: "models",
    eyebrow: "第二步：确定可用模型",
    title: "获取并选择模型",
    description: "在供应商页面找到对应卡片后点击“配置”，获取模型目录，再选择需要对外提供的模型映射和别名。",
    detail: "上游未提供上下文长度时，保留可编辑的默认值；已保存的模型不会因重新获取目录而被自动改写。",
    target: "providers",
    actionLabel: "前往供应商页面",
  },
  {
    id: "test",
    eyebrow: "第三步：验证可达性",
    title: "保存后测试连接",
    description: "保存供应商后，使用供应商卡片中的“测试连接”确认地址、凭据和上游响应均正常。",
    detail: "模型目录可见不代表账户一定可调用。连接通过后，再根据需要设为主用或调整优先级。",
    target: "providers",
    actionLabel: "查看供应商状态",
  },
  {
    id: "backup",
    eyebrow: "第四步：接入前留好回退点",
    title: "客户端接入前先备份",
    description: "在设置页核对统一接入地址和访问 Key；接管本地客户端配置前，先使用“备份并写入配置”。",
    detail: "供应商 Key 仅用于网关访问上游；本地统一 Key 仅供客户端连接此网关。接管已有配置时，备份失败会停止写入并保留原文件。",
    target: "settings",
    actionLabel: "前往设置页",
  },
  {
    id: "review",
    eyebrow: "第五步：持续观察运行状态",
    title: "查看会话与用量",
    description: "会话上下文用于查看持久化消息、摘要和路由续接；用量与审计用于核对请求量、实际路由和降级记录。",
    detail: "供应商额度和有效期可从供应商卡片的更多操作中查询。界面展示不会替你修改路由或凭据。",
  },
];

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function focusableNodes(container: HTMLElement) {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter((node) => {
    const style = window.getComputedStyle(node);
    return node.tabIndex >= 0 && style.visibility !== "hidden" && style.display !== "none";
  });
}

export default function Onboarding({ open, onSkip, onComplete, onNavigate }: OnboardingProps) {
  const [stepIndex, setStepIndex] = useState(0);
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstFocusableRef = useRef<HTMLButtonElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const wasOpenRef = useRef(false);
  const onSkipRef = useRef(onSkip);
  const titleId = useId();
  const descriptionId = useId();
  const currentStep = STEPS[stepIndex];
  const isFirst = stepIndex === 0;
  const isLast = stepIndex === STEPS.length - 1;

  useEffect(() => {
    if (open && !wasOpenRef.current) setStepIndex(0);
    wasOpenRef.current = open;
  }, [open]);

  useEffect(() => {
    onSkipRef.current = onSkip;
  }, [onSkip]);

  useEffect(() => {
    if (!open) return;

    const previous = document.activeElement;
    restoreFocusRef.current = previous instanceof HTMLElement ? previous : null;
    const frame = window.requestAnimationFrame(() => firstFocusableRef.current?.focus());

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onSkipRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const dialog = dialogRef.current;
      if (!dialog) return;
      const nodes = focusableNodes(dialog);
      if (!nodes.length) {
        event.preventDefault();
        dialog.focus();
        return;
      }

      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKeyDown);
    return () => {
      window.cancelAnimationFrame(frame);
      document.removeEventListener("keydown", onKeyDown);
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
    };
  }, [open]);

  if (!open) return null;

  const navigate = (page: OnboardingTarget) => {
    onNavigate(page);
  };

  const move = (direction: -1 | 1) => {
    setStepIndex((current) => Math.min(STEPS.length - 1, Math.max(0, current + direction)));
  };

  return (
    <div className="onboarding-mask" data-testid="onboarding-overlay">
      <div
        ref={dialogRef}
        className="onboarding-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        aria-label="首次配置引导"
        tabIndex={-1}
        data-testid="onboarding-dialog"
      >
        <header className="onboarding-header">
          <div>
            <span className="onboarding-overline">LLM Gateway</span>
            <h2 id={titleId}>首次配置引导</h2>
          </div>
          <button
            ref={firstFocusableRef}
            type="button"
            className="onboarding-close"
            onClick={onSkip}
            aria-label="跳过并关闭首次配置引导"
            title="跳过引导"
            data-testid="onboarding-close"
          >
            <span aria-hidden="true">×</span>
          </button>
        </header>

        <ol className="onboarding-progress" aria-label="引导进度" data-testid="onboarding-progress">
          {STEPS.map((step, index) => {
            const active = index === stepIndex;
            const complete = index < stepIndex;
            return (
              <li
                key={step.id}
                className={active ? "active" : complete ? "complete" : ""}
                aria-current={active ? "step" : undefined}
                aria-label={`第 ${index + 1} 步，共 ${STEPS.length} 步：${step.title}${complete ? "，已浏览" : active ? "，当前" : ""}`}
              >
                <span aria-hidden="true">{index + 1}</span>
              </li>
            );
          })}
        </ol>

        <section className="onboarding-content" aria-live="polite" data-testid={`onboarding-step-${currentStep.id}`}>
          <span className="onboarding-step-label">{currentStep.eyebrow}</span>
          <h3>{currentStep.title}</h3>
          <p id={descriptionId} className="onboarding-description">{currentStep.description}</p>
          <p className="onboarding-detail">{currentStep.detail}</p>

          {currentStep.target && currentStep.actionLabel && (
            <button
              type="button"
              className="onboarding-page-link"
              onClick={() => navigate(currentStep.target!)}
              data-testid={`onboarding-navigate-${currentStep.target}`}
            >
              {currentStep.actionLabel}
            </button>
          )}

          {currentStep.id === "review" && (
            <div className="onboarding-review-actions" aria-label="运行状态页面">
              <button
                type="button"
                className="onboarding-page-link"
                onClick={() => navigate("sessions")}
                data-testid="onboarding-navigate-sessions"
              >
                查看会话上下文
              </button>
              <button
                type="button"
                className="onboarding-page-link"
                onClick={() => navigate("stats")}
                data-testid="onboarding-navigate-stats"
              >
                查看用量与审计
              </button>
            </div>
          )}
        </section>

        <footer className="onboarding-footer">
          <button
            type="button"
            className="ghost"
            onClick={onSkip}
            data-testid="onboarding-skip"
          >
            跳过引导
          </button>
          <div className="onboarding-footer-actions">
            <button
              type="button"
              onClick={() => move(-1)}
              disabled={isFirst}
              data-testid="onboarding-previous"
            >
              上一步
            </button>
            {isLast ? (
              <button
                type="button"
                className="primary"
                onClick={onComplete}
                data-testid="onboarding-complete"
              >
                完成
              </button>
            ) : (
              <button
                type="button"
                className="primary"
                onClick={() => move(1)}
                data-testid="onboarding-next"
              >
                下一步
              </button>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}
