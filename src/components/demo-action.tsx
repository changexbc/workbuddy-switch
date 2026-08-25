import { cloneElement, isValidElement, useState, type ReactElement, type ReactNode } from "react";
import { toast } from "sonner";

import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { DEMO_UNAVAILABLE_MESSAGE, demoModeEnabled } from "@/lib/demo-mode";
import { cn } from "@/lib/utils";

interface DemoActionProps {
  children: ReactElement;
  className?: string;
}

/**
 * Keeps operational controls visible in the public demo while preventing activation.
 * The wrapper remains hoverable and keyboard focusable because native disabled controls
 * cannot trigger a tooltip by themselves.
 */
export function DemoAction({ children, className }: DemoActionProps) {
  const [open, setOpen] = useState(false);

  if (!demoModeEnabled) return children;

  const child = isValidElement(children)
    ? cloneElement(children as ReactElement<Record<string, unknown>>, {
        disabled: true,
        "aria-disabled": true,
        tabIndex: -1,
      })
    : children;

  function explain() {
    setOpen(true);
    toast.info(DEMO_UNAVAILABLE_MESSAGE);
  }

  return (
    <Tooltip open={open} onOpenChange={setOpen}>
      <TooltipTrigger asChild>
        <span
          className={cn("inline-flex max-w-full", className)}
          role="button"
          tabIndex={0}
          aria-disabled="true"
          aria-label={DEMO_UNAVAILABLE_MESSAGE}
          onPointerDownCapture={(event) => {
            // Disabled form controls do not reliably dispatch click events to ancestors.
            // Pointer-down capture still reports the attempted activation without
            // allowing the child control to run its own handler.
            event.preventDefault();
            explain();
          }}
          onKeyDown={(event) => {
            if (event.key !== "Enter" && event.key !== " ") return;
            event.preventDefault();
            explain();
          }}
        >
          {child as ReactNode}
        </span>
      </TooltipTrigger>
      <TooltipContent>{DEMO_UNAVAILABLE_MESSAGE}</TooltipContent>
    </Tooltip>
  );
}
