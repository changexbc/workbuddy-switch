import { cn } from "@/lib/utils";

interface MarkProps {
  size?: number;
  className?: string;
}

export function WorkBuddyMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("inline-flex shrink-0 overflow-hidden rounded-[22%]", className)}
      style={{ width: size, height: size }}
    >
      <img src="/icon.png" alt="" className="size-full object-cover" />
    </span>
  );
}

export function CodeBuddyMark({ size = 32, className }: MarkProps) {
  const icon = Math.max(10, Math.round(size * 0.56));
  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-[22%] bg-teal-100 text-teal-700",
        className,
      )}
      style={{ width: size, height: size, fontSize: icon }}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="size-[1em]"
      >
        <path d="M12 3v2" />
        <circle cx="12" cy="2.5" r=".7" fill="currentColor" stroke="none" />
        <rect x="4.5" y="6.5" width="15" height="12" rx="4" />
        <path d="M4.5 11H3.3M20.5 11h1.2" />
        <circle cx="9" cy="12" r="1" fill="currentColor" stroke="none" />
        <circle cx="15" cy="12" r="1" fill="currentColor" stroke="none" />
        <path d="M9 15.5c1.7 1 4.3 1 6 0" />
      </svg>
    </span>
  );
}

export function StatusDot({ on }: { on: boolean }) {
  return (
    <span
      aria-hidden
      className={cn("size-1.5 shrink-0 rounded-full", on ? "bg-emerald-500" : "bg-muted-foreground/35")}
    />
  );
}
