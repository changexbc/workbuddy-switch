import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { useT } from "@/lib/i18n";
import { cn } from "@/lib/utils";

const HOUR_OPTIONS = Array.from({ length: 24 }, (_, i) => String(i).padStart(2, "0"));
const MINUTE_OPTIONS = Array.from({ length: 60 }, (_, i) => String(i).padStart(2, "0"));

interface TimePickerProps {
  /** "HH:MM"；空串表示未设置。 */
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  className?: string;
  hourLabel?: string;
  minuteLabel?: string;
}

/** "HH:MM" → 零填充时分；空串或非法值返回 null。 */
function parseClock(value: string): { hour: string; minute: string } | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  if (hour > 23 || minute > 59) return null;
  return { hour: match[1].padStart(2, "0"), minute: match[2] };
}

/**
 * 时、分两个 Select 组合的时间选择器，代替原生 `input[type=time]`
 * （桌面 WKWebView 与浏览器呈现不一致，且 macOS 原生控件没有清空入口）。
 * 未设置时两端显示 `--`；未选小时时分钟不可用，避免出现只有分钟的状态。
 */
function TimePicker({
  value,
  onChange,
  disabled,
  className,
  hourLabel,
  minuteLabel,
}: TimePickerProps) {
  const t = useT();
  const parsed = parseClock(value);
  const hour = parsed?.hour ?? "";
  const minute = parsed?.minute ?? "";

  return (
    <div data-slot="time-picker" className={cn("flex min-w-0 items-center gap-1", className)}>
      <Select
        value={hour}
        disabled={disabled}
        onValueChange={(next) => onChange(`${next}:${minute || "00"}`)}
      >
        <SelectTrigger size="sm" aria-label={hourLabel ?? t("common.hourLabel")} className="w-[4.5rem] min-w-0">
          <SelectValue placeholder="--" />
        </SelectTrigger>
        <SelectContent className="max-h-64">
          {HOUR_OPTIONS.map((option) => (
            <SelectItem key={option} value={option}>
              {option}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <span className="shrink-0 text-xs text-muted-foreground">:</span>
      <Select
        value={minute}
        disabled={disabled || !hour}
        onValueChange={(next) => onChange(`${hour}:${next}`)}
      >
        <SelectTrigger size="sm" aria-label={minuteLabel ?? t("common.minuteLabel")} className="w-[4.5rem] min-w-0">
          <SelectValue placeholder="--" />
        </SelectTrigger>
        <SelectContent className="max-h-64">
          {MINUTE_OPTIONS.map((option) => (
            <SelectItem key={option} value={option}>
              {option}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}

export { TimePicker };
