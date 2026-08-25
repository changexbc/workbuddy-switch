import {
  AlertTriangle,
  CheckCircle2,
  Info,
  Loader2,
  XCircle,
} from "lucide-react";
import { Toaster as SonnerToaster, type ToasterProps } from "sonner";

export function Toaster(props: ToasterProps) {
  return (
    <SonnerToaster
      position="bottom-right"
      closeButton
      gap={8}
      visibleToasts={4}
      icons={{
        success: <CheckCircle2 className="size-4 text-brand" />,
        info: <Info className="size-4 text-brand" />,
        warning: <AlertTriangle className="size-4 text-amber-600" />,
        error: <XCircle className="size-4 text-destructive" />,
        loading: <Loader2 className="size-4 animate-spin text-brand" />,
      }}
      toastOptions={{
        classNames: {
          toast:
            "!gap-2.5 !rounded-lg !border-slate-200/90 !bg-popover !px-3 !py-2.5 !text-popover-foreground !shadow-[0_8px_24px_rgba(15,23,42,.09)]",
          content: "!gap-0.5",
          title: "!text-[13px] !font-medium !leading-5",
          description: "!text-xs !leading-5 !text-muted-foreground",
          icon: "!mr-0 !self-start !pt-0.5",
          closeButton:
            "!size-5 !border-slate-200 !bg-popover !text-muted-foreground !shadow-sm hover:!bg-accent hover:!text-foreground focus-visible:!ring-2 focus-visible:!ring-brand/30",
          actionButton: "!h-7 !rounded-md !bg-brand !px-2.5 !text-xs !text-brand-foreground",
          cancelButton: "!h-7 !rounded-md !bg-muted !px-2.5 !text-xs !text-muted-foreground",
        },
      }}
      {...props}
    />
  );
}
