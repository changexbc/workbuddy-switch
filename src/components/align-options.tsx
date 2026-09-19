import { Switch } from "@/components/ui/switch";

/** 切号时的数据对齐开关（本版本只含「带走定时任务」，后续按功能增补）。 */
export type AlignOptionsValue = {
  alignAutomations: boolean;
};

type Props = {
  value: AlignOptionsValue;
  onChange: (value: AlignOptionsValue) => void;
};

export function AlignOptionsPanel({ value, onChange }: Props) {
  return (
    <div className="space-y-2">
      <Row
        title="带走定时任务"
        hint="让目标账号也能看到、继续管你现在这些定时任务（只改本机记录，云端不受影响）"
        checked={value.alignAutomations}
        onCheckedChange={(v) => onChange({ ...value, alignAutomations: v })}
      />
    </div>
  );
}

function Row({
  title,
  hint,
  checked,
  onCheckedChange,
}: {
  title: string;
  hint: string;
  checked: boolean;
  onCheckedChange: (v: boolean) => void;
}) {
  return (
    <label className="flex items-start justify-between gap-3 rounded-md border p-3">
      <span className="space-y-1">
        <span className="block text-sm font-medium">{title}</span>
        <span className="block text-xs text-muted-foreground">{hint}</span>
      </span>
      <Switch checked={checked} onCheckedChange={onCheckedChange} />
    </label>
  );
}
