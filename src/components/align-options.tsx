import { Switch } from "@/components/ui/switch";

/** 切号时的会话共享开关（本版本只含「共享会话」，后续按功能增补）。 */
export type AlignOptionsValue = {
  autoLink: boolean;
};

type Props = {
  value: AlignOptionsValue;
  onChange: (value: AlignOptionsValue) => void;
};

export function AlignOptionsPanel({ value, onChange }: Props) {
  return (
    <div className="space-y-2">
      <Row
        title="共享会话"
        hint="把当前账号的会话共享给目标账号：不占额外空间，手机端也能看到，目标已有的不会重复"
        checked={value.autoLink}
        onCheckedChange={(v) => onChange({ ...value, autoLink: v })}
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
