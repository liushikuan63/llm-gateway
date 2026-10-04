import { useId, type SVGProps } from "react";

export type IconName =
  | "providers"
  | "sessions"
  | "activity"
  | "settings"
  | "copy"
  | "check"
  | "moon"
  | "sun"
  | "shield"
  | "monitor"
  | "warning";

/// 应用标识：多路上游汇聚为单一出口。与 src-tauri/icons/logo.svg 同构，
/// 修改时两处保持一致并运行 npm run icons 重新生成图标文件。
export function LogoMark({ size = 32 }: { size?: number }) {
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const bgId = `logo-bg-${uid}`;
  const flowId = `logo-flow-${uid}`;

  return (
    <svg width={size} height={size} viewBox="0 0 512 512" aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id={bgId} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#16403a" />
          <stop offset="1" stopColor="#0a1e1a" />
        </linearGradient>
        <linearGradient id={flowId} gradientUnits="userSpaceOnUse" x1="128" y1="0" x2="276" y2="0">
          <stop offset="0" stopColor="#2fb99b" />
          <stop offset="1" stopColor="#7cecc9" />
        </linearGradient>
      </defs>
      <rect x="20" y="20" width="472" height="472" rx="112" fill={`url(#${bgId})`} />
      <g fill="none" stroke={`url(#${flowId})`} strokeWidth={46} strokeLinecap="round">
        <path d="M128 152 C 208 152 212 256 276 256" />
        <path d="M128 256 L 276 256" />
        <path d="M128 360 C 208 360 212 256 276 256" />
      </g>
      <path d="M276 256 L 355 256" fill="none" stroke="#b8f5e0" strokeWidth={46} strokeLinecap="round" />
      <circle cx="355" cy="256" r="40" fill="#eafff6" />
    </svg>
  );
}

type IconProps = Omit<SVGProps<SVGSVGElement>, "children"> & {
  name: IconName;
  size?: number;
};

export function Icon({ name, size = 18, ...props }: IconProps) {
  const shared = {
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.8,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
      {...props}
    >
      {name === "providers" && (
        <>
          <rect x="4" y="4" width="16" height="6" rx="1.5" {...shared} />
          <rect x="4" y="14" width="16" height="6" rx="1.5" {...shared} />
          <path d="M8 7h.01M8 17h.01M12 7h5M12 17h5" {...shared} />
        </>
      )}
      {name === "sessions" && (
        <>
          <path d="M5 5.5A2.5 2.5 0 0 1 7.5 3h9A2.5 2.5 0 0 1 19 5.5v7A2.5 2.5 0 0 1 16.5 15H11l-4 4v-4.2A2.5 2.5 0 0 1 5 12.5z" {...shared} />
          <path d="M9 8h6M9 11h4" {...shared} />
        </>
      )}
      {name === "activity" && <path d="M4 13h3l2-7 4 12 2-5h5" {...shared} />}
      {name === "settings" && (
        <>
          <path d="M4 7h7M15 7h5M4 17h4M12 17h8" {...shared} />
          <circle cx="13" cy="7" r="2" {...shared} />
          <circle cx="10" cy="17" r="2" {...shared} />
        </>
      )}
      {name === "copy" && (
        <>
          <rect x="8" y="8" width="11" height="11" rx="2" {...shared} />
          <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" {...shared} />
        </>
      )}
      {name === "check" && <path d="m5 12 4.2 4.2L19 6.5" {...shared} />}
      {name === "moon" && <path d="M20 15.2A8.4 8.4 0 0 1 8.8 4 8.6 8.6 0 1 0 20 15.2Z" {...shared} />}
      {name === "sun" && (
        <>
          <circle cx="12" cy="12" r="3.5" {...shared} />
          <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" {...shared} />
        </>
      )}
      {name === "shield" && <path d="M12 3 5.5 5.6v5.1c0 4.4 2.6 8 6.5 10.3 3.9-2.3 6.5-5.9 6.5-10.3V5.6zM9.3 12.1l1.8 1.8 3.8-4" {...shared} />}
      {name === "monitor" && (
        <>
          <rect x="3.5" y="4" width="17" height="12" rx="2" {...shared} />
          <path d="M8 20h8M12 16v4" {...shared} />
        </>
      )}
      {name === "warning" && (
        <>
          <path d="M12 4.5 2.8 20h18.4z" {...shared} />
          <path d="M12 10v4.2M12 17.3h.01" {...shared} />
        </>
      )}
    </svg>
  );
}
