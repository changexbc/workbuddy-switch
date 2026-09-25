import { cn } from "@/lib/utils";
import workbuddyIcon from "@/assets/workbuddy-official-icon.png";
import codebuddyCnIdeIcon from "@/assets/codebuddy-cn-ide-icon.png";

const appIconUrl = `${import.meta.env.BASE_URL}icon-transparent.png`;

interface MarkProps {
  size?: number;
  className?: string;
}

/**
 * WorkBuddy 官方应用图标（从 WorkBuddy.app 的 icon.icns 提取）。
 * 与 CodeBuddy IDE 图标同为标准 macOS app icon 风格（约 10% 透明边距），
 * 放大 118% 居中裁掉透明圈后与 CodeBuddy 系列图标视觉一致。
 */
export function WorkBuddyMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0 overflow-hidden rounded-[22%]", className)}
      style={{ width: size, height: size }}
    >
      <img
        src={workbuddyIcon}
        alt=""
        className="absolute left-1/2 top-1/2 size-[118%] max-w-none -translate-x-1/2 -translate-y-1/2 object-cover"
      />
    </span>
  );
}

/**
 * 档位角标：复用官方图标 + 角标区分档位，不新画图形，
 * 保持与既有 WorkBuddy / CodeBuddy 标记同一套圆角与配色。
 * 角标文案为 `INTL`（4 字符，比圆形 badge 宽），故按内容撑成胶囊并收紧字号。
 * WorkBuddy 与 CodeBuddy IDE 的档位标记共用这一份实现。
 */
function IntlBadge({ size }: { size: number }) {
  const badge = Math.max(11, Math.round(size * 0.46));
  return (
    <span
      className="absolute -bottom-0.5 -right-0.5 inline-flex items-center justify-center rounded-full border border-card bg-foreground px-[3px] font-semibold leading-none text-background"
      style={{ minWidth: badge, height: badge, fontSize: Math.max(6, Math.round(badge * 0.5)) }}
    >
      INTL
    </span>
  );
}

/** WorkBuddy 国际版标记。 */
export function WorkBuddyAiMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <WorkBuddyMark size={size} />
      <IntlBadge size={size} />
    </span>
  );
}

/** 应用自身的透明角色图标；桌面安装图标仍使用 public/icon.png。 */
export function AppIconMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <img src={appIconUrl} alt="" className="size-full object-contain" />
    </span>
  );
}

export function CodeBuddyMark({ size = 32, className }: MarkProps) {
  const icon = Math.max(10, Math.round(size));
  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-[22%] border border-white/10 bg-zinc-950 text-zinc-50 shadow-sm",
        className,
      )}
      style={{ width: size, height: size, fontSize: icon }}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="size-[1em]"
      >
        <path d="M4.4 7.4 10.4 12 4.4 16.6" />
        <path d="M13 16.6h7" />
      </svg>
    </span>
  );
}

/**
 * CodeBuddy IDE（桌面客户端）官方应用图标。
 * 源图四周自带约 9% 透明边距：正方形图 + object-cover 不会触发任何缩放，
 * 必须先把图放大到 122% 再居中裁剪，才能把透明圈裁掉并与 WorkBuddy 的
 * 全幅 logo 达到同样的视觉大小（裁剪仅落在透明边距上，几乎不伤画面）。
 */
export function CodeBuddyCnIdeMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0 overflow-hidden", className)}
      style={{ width: size, height: size }}
    >
      <img
        src={codebuddyCnIdeIcon}
        alt=""
        className="absolute left-1/2 top-1/2 size-[122%] max-w-none -translate-x-1/2 -translate-y-1/2 object-cover"
      />
    </span>
  );
}

/**
 * VS Code 内 CodeBuddy 扩展标记：**无底块，直接画扩展官方字形**。
 *
 * - 字形取自扩展自带的 `resources/copilot.svg`（24×24 六边形 + S 形镂空，`fill-rule: evenodd`）；
 * - viewBox 裁到字形包围盒 `2.1 0.63 19.8 22.74`（去掉原图四周约 8% 空白）后 `size-full` 铺满：
 *   字形高度等于调用方给的 `size`，与相邻满幅 app 图标（WorkBuddy / CodeBuddy IDE）视觉等大；
 * - `currentColor` 着色：浅色主题下深色字形、深色主题下浅色字形，颜色随所在按钮 / 文字。
 */
export function VscodeExtMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("inline-flex shrink-0 items-center justify-center", className)}
      style={{ width: size, height: size }}
    >
      <svg viewBox="2.1 0.63 19.8 22.74" fill="none" className="size-full">
        <path
          fillRule="evenodd"
          clipRule="evenodd"
          fill="currentColor"
          d="M12.8186 0.927341C12.3127 0.632261 11.6872 0.632262 11.1814 0.927343L2.90588 5.75472C2.40677 6.04588 2.09985 6.58022 2.09985 7.15805V16.8419C2.09985 17.4198 2.40677 17.9541 2.90588 18.2453L11.1814 23.0727C11.6872 23.3677 12.3127 23.3677 12.8186 23.0727L21.0941 18.2453C21.5932 17.9541 21.9002 17.4198 21.9002 16.8419V7.15806C21.9002 6.58022 21.5932 6.04588 21.0941 5.75473L12.8186 0.927341ZM13.7284 15.7031C13.1528 14.8905 13.4369 13.7562 14.3275 13.3109L16.0465 12.4514C17.173 11.8881 17.2579 10.3127 16.1985 9.63167L8.81052 4.88228C8.43485 4.64078 8.00131 5.09426 8.25945 5.4587L10.2699 8.29701C10.8455 9.10954 10.5613 10.2439 9.67074 10.6892L7.95179 11.5487C6.82527 12.1119 6.74036 13.6873 7.79981 14.3684L15.1877 19.1178C15.5634 19.3593 15.997 18.9058 15.7388 18.5414L13.7284 15.7031Z"
        />
      </svg>
    </span>
  );
}

/**
 * CodeBuddy IDE 国际版标记：同一官方图标 + INTL 角标。
 * 依据 `variantUsesIntlCodebuddyIde()` —— 国际版档位下 IDE 切的是 CodeBuddy.app，
 * 与国内版的 CodeBuddy CN 是两个客户端，故用同一套角标区分。
 */
export function CodeBuddyAiIdeMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <CodeBuddyCnIdeMark size={size} />
      <IntlBadge size={size} />
    </span>
  );
}

/**
 * JetBrains IDE（IntelliJ IDEA / PyCharm）标记：官方 2021 品牌字块的简化版。
 *
 * 黑色圆角方块 + 右下角品牌渐变斜条（粉→黄→绿），不用真实 logo 资产，
 * 与相邻的深色 CodeBuddy / VS Code 字块视觉密度一致。
 */
export function JetbrainsMark({ size = 32, className }: MarkProps) {
  const id = `jb-gradient-${size}`;
  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center overflow-hidden rounded-[22%] border border-white/10 bg-zinc-950 shadow-sm",
        className,
      )}
      style={{ width: size, height: size }}
    >
      <svg viewBox="0 0 24 24" className="size-[72%]" fill="none">
        <defs>
          <linearGradient id={id} x1="12" y1="20" x2="20" y2="12" gradientUnits="userSpaceOnUse">
            <stop offset="0" stopColor="#EE318C" />
            <stop offset="0.5" stopColor="#FDB60D" />
            <stop offset="1" stopColor="#21D789" />
          </linearGradient>
        </defs>
        <rect x="12" y="12" width="10" height="10" fill={`url(#${id})`} />
        <rect x="2" y="2" width="7" height="3" fill="#EEEDFF" />
      </svg>
    </span>
  );
}

export function StatusDot({ on, className }: { on: boolean; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn("size-1.5 shrink-0 rounded-full", on ? "bg-primary" : "bg-muted-foreground/35", className)}
    />
  );
}
