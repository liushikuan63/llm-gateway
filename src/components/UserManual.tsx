import { useEffect, useMemo, useRef, useState } from "react";
import manualSource from "../content/user-manual.json";
import "./user-manual.css";

type ManualCode = {
  language: string;
  content: string;
};

type ManualSection = {
  id: string;
  anchorId: string;
  title: string;
  paragraphs: string[];
  steps: string[];
  notes: string[];
  code: ManualCode | null;
};

type Manual = {
  title: string;
  version: string;
  introduction: string;
  sections: ManualSection[];
};

export type UserManualProps = {
  open: boolean;
  onClose: () => void;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function text(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function textList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function readCode(value: unknown): ManualCode | null {
  if (!isRecord(value)) return null;
  const content = text(value.content);
  if (!content) return null;
  return { language: text(value.language, "text"), content };
}

function normalizeManual(value: unknown): Manual {
  const source: Record<string, unknown> = isRecord(value) ? value : {};
  const usedIds = new Set<string>();
  const sections = (Array.isArray(source.sections) ? source.sections : []).flatMap((item, index) => {
    if (!isRecord(item)) return [];

    const title = text(item.title).trim() || `第 ${index + 1} 节`;
    const candidate = text(item.id).trim() || `section-${index + 1}`;
    let id = candidate;
    let duplicate = 2;
    while (usedIds.has(id)) {
      id = `${candidate}-${duplicate}`;
      duplicate += 1;
    }
    usedIds.add(id);

    return [{
      id,
      anchorId: `user-manual-section-${index + 1}`,
      title,
      paragraphs: textList(item.paragraphs),
      steps: textList(item.steps),
      notes: textList(item.notes),
      code: readCode(item.code),
    }];
  });

  return {
    title: text(source.title).trim() || "使用手册",
    version: text(source.version).trim(),
    introduction: text(source.introduction).trim(),
    sections,
  };
}

function matchesQuery(section: ManualSection, query: string): boolean {
  if (!query) return true;
  return [
    section.title,
    ...section.paragraphs,
    ...section.steps,
    ...section.notes,
    section.code?.language ?? "",
    section.code?.content ?? "",
  ].some((value) => value.toLocaleLowerCase().includes(query));
}

const manual = normalizeManual(manualSource);
const FOCUSABLE_SELECTOR = [
  "button:not(:disabled)",
  "[href]",
  "input:not(:disabled)",
  "select:not(:disabled)",
  "textarea:not(:disabled)",
  "[tabindex]:not([tabindex=\"-1\"])",
].join(", ");

export default function UserManual({ open, onClose }: UserManualProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const lastFocusedRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  const sectionRefs = useRef(new Map<string, HTMLElement>());
  const [query, setQuery] = useState("");
  const [activeSection, setActiveSection] = useState<string | null>(manual.sections[0]?.id ?? null);
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const filteredSections = useMemo(
    () => manual.sections.filter((section) => matchesQuery(section, normalizedQuery)),
    [normalizedQuery],
  );
  const introductionMatches = !normalizedQuery || manual.introduction.toLocaleLowerCase().includes(normalizedQuery);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!open) return;

    const activeElement = document.activeElement;
    lastFocusedRef.current = activeElement instanceof HTMLElement ? activeElement : null;
    const frame = window.requestAnimationFrame(() => searchRef.current?.focus());

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? [])
        .filter((element) => element.getClientRects().length > 0);
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;

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
      if (lastFocusedRef.current?.isConnected) lastFocusedRef.current.focus({ preventScroll: true });
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    if (!filteredSections.some((section) => section.id === activeSection)) {
      setActiveSection(filteredSections[0]?.id ?? null);
    }
  }, [activeSection, filteredSections, open]);

  if (!open) return null;

  const navigateTo = (section: ManualSection) => {
    setActiveSection(section.id);
    sectionRefs.current.get(section.id)?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  return (
    <div
      className="user-manual-mask"
      data-testid="user-manual"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="user-manual-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="user-manual-title"
        aria-describedby={manual.introduction ? "user-manual-introduction" : undefined}
      >
        <header className="user-manual-header">
          <div className="user-manual-heading">
            <span className="user-manual-eyebrow">内置文档</span>
            <h2 id="user-manual-title">{manual.title}</h2>
            {manual.version && <p className="user-manual-version">版本 {manual.version}</p>}
          </div>
          <button
            type="button"
            className="user-manual-close"
            data-testid="user-manual-close"
            aria-label="关闭使用手册"
            title="关闭使用手册"
            onClick={onClose}
          >
            <span aria-hidden="true">×</span>
          </button>
        </header>

        <div className="user-manual-layout">
          <aside className="user-manual-sidebar" aria-label="手册目录">
            <div className="user-manual-search-wrap">
              <label htmlFor="user-manual-search">搜索手册</label>
              <input
                ref={searchRef}
                id="user-manual-search"
                data-testid="user-manual-search"
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="搜索标题和内容"
                autoComplete="off"
              />
            </div>

            <nav className="user-manual-toc" data-testid="user-manual-toc" aria-label="手册目录导航">
              {filteredSections.map((section, index) => (
                <button
                  type="button"
                  key={section.id}
                  className={activeSection === section.id ? "active" : ""}
                  data-testid="user-manual-toc-item"
                  data-section-id={section.id}
                  aria-current={activeSection === section.id ? "location" : undefined}
                  aria-controls={section.anchorId}
                  onClick={() => navigateTo(section)}
                >
                  <span aria-hidden="true">{String(index + 1).padStart(2, "0")}</span>
                  <strong>{section.title}</strong>
                </button>
              ))}
            </nav>
          </aside>

          <div className="user-manual-content" data-testid="user-manual-results" tabIndex={-1}>
            {manual.introduction && <p id="user-manual-introduction" className="user-manual-introduction">{manual.introduction}</p>}

            {filteredSections.length === 0 && !introductionMatches ? (
              <div className="user-manual-empty" data-testid="user-manual-empty" role="status">
                没有找到与“{query.trim()}”匹配的内容。
              </div>
            ) : (
              filteredSections.map((section) => (
                <section
                  key={section.id}
                  ref={(element) => {
                    if (element) sectionRefs.current.set(section.id, element);
                    else sectionRefs.current.delete(section.id);
                  }}
                  id={section.anchorId}
                  className="user-manual-section"
                  data-testid="user-manual-section"
                  data-section-id={section.id}
                  aria-labelledby={`${section.anchorId}-title`}
                >
                  <h3 id={`${section.anchorId}-title`}>{section.title}</h3>
                  {section.paragraphs.map((paragraph, index) => <p key={`${section.id}-paragraph-${index}`}>{paragraph}</p>)}

                  {section.steps.length > 0 && (
                    <ol className="user-manual-steps">
                      {section.steps.map((step, index) => <li key={`${section.id}-step-${index}`}>{step}</li>)}
                    </ol>
                  )}

                  {section.notes.length > 0 && (
                    <ul className="user-manual-notes">
                      {section.notes.map((note, index) => <li key={`${section.id}-note-${index}`}>{note}</li>)}
                    </ul>
                  )}

                  {section.code && (
                    <div className="user-manual-code" data-testid="user-manual-code">
                      <div className="user-manual-code-label">{section.code.language}</div>
                      <pre><code>{section.code.content}</code></pre>
                    </div>
                  )}
                </section>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
