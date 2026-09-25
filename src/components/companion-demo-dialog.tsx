import { useCallback, useEffect, useRef, useState } from "react";

import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  COMPANION_DEMO_CHANNEL,
  COMPANION_DEMO_INTRO_HOLD_MS,
  COMPANION_DEMO_ROUND,
  buildCompanionDemoSnapshot,
  postCompanionDemoSnapshot,
} from "@/lib/companion-demo-script";

/** Reported by the rail when the visitor tries something only the desktop app owns. */
interface CompanionDemoFrameMessage {
  channel: string;
  type: "ready" | "activate-client" | "drag" | "blocked";
  action?: "open-session" | "close-monitoring";
  text?: string;
}

const BLOCKED_FALLBACK = "该操作仅在桌面版可用";
const ACTIVATE_CLIENT_NOTICE = "演示不会打开真实会话";

/**
 * The upstream demo build of the rail; `BASE_URL` keeps it valid under any
 * hosting prefix. `?welcome` is the rail's own switch for its opening kitten
 * animation — the demo build has no desktop host, so without it the animation
 * is skipped.
 */
const RAIL_SRC = `${import.meta.env.BASE_URL}companion-demo/desktop.html?welcome`;

interface CompanionDemoDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * Read-only showcase of the Agent Companion rail on the public demo page.
 *
 * The overlay only embeds the upstream web demo build and drives it with
 * fictional snapshots: it never installs, starts or talks to the local
 * component, and the frame itself refuses session links and the close command.
 */
export function CompanionDemoDialog({ open, onOpenChange }: CompanionDemoDialogProps) {
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const timerRef = useRef<number | undefined>(undefined);
  const generationRef = useRef(0);
  const startedRef = useRef(false);
  const [notice, setNotice] = useState<string | null>(null);

  const stopPlayback = useCallback(() => {
    generationRef.current += 1;
    if (timerRef.current !== undefined) {
      window.clearTimeout(timerRef.current);
      timerRef.current = undefined;
    }
    startedRef.current = false;
  }, []);

  /** Publishes one round of snapshots, then restarts with a fresh round id. */
  const startPlayback = useCallback(() => {
    if (startedRef.current) return;
    startedRef.current = true;
    const generation = generationRef.current;
    let index = 0;
    let cycle = 0;
    const play = () => {
      if (generationRef.current !== generation) return;
      const step = COMPANION_DEMO_ROUND[index];
      postCompanionDemoSnapshot(frameRef.current, buildCompanionDemoSnapshot(step.phase, `demo-round-${cycle + 1}`));
      // The opening round waits out the rail's welcome animation (see
      // `COMPANION_DEMO_INTRO_HOLD_MS`); an urgent session would abort it.
      const holdMs = cycle === 0 && index === 0 ? COMPANION_DEMO_INTRO_HOLD_MS : step.holdMs;
      timerRef.current = window.setTimeout(() => {
        index += 1;
        if (index >= COMPANION_DEMO_ROUND.length) {
          index = 0;
          cycle += 1;
        }
        play();
      }, holdMs);
    };
    play();
  }, []);

  useEffect(() => {
    if (!open) return;
    setNotice(null);

    const onMessage = (event: MessageEvent<unknown>) => {
      // Same origin only, and only from the frame this dialog embeds.
      if (event.origin !== window.location.origin || event.source !== frameRef.current?.contentWindow) return;
      const message = event.data as Partial<CompanionDemoFrameMessage> | null;
      if (!message || message.channel !== COMPANION_DEMO_CHANNEL) return;
      if (message.type === "ready") {
        // The rail publishes `ready` only after it has registered its own
        // listener, so this is the first moment a snapshot can land. The frame's
        // `load` event is too early: the rail's demo bridge is a dynamic import
        // and evaluates after `load`, and a snapshot sent then is dropped.
        startPlayback();
        return;
      }
      if (message.type === "blocked") {
        setNotice(message.text || BLOCKED_FALLBACK);
        return;
      }
      if (message.type === "activate-client") setNotice(ACTIVATE_CLIENT_NOTICE);
      // `drag` is the grip of a real window; the overlay has no window to move.
    };

    window.addEventListener("message", onMessage);
    return () => {
      window.removeEventListener("message", onMessage);
      stopPlayback();
    };
  }, [open, startPlayback, stopPlayback]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="gap-3 sm:max-w-[492px]">
        <DialogHeader>
          <DialogTitle>会话悬浮窗</DialogTitle>
          <DialogDescription>
            悬浮栏的真实形态，演示数据自动轮播，不会读取或打开本机任何会话。
          </DialogDescription>
        </DialogHeader>
        <div className="mx-auto h-[520px] max-h-[70vh] w-[420px] max-w-full overflow-hidden rounded-xl border border-border/60 bg-gradient-to-br from-muted/50 to-background">
          <iframe
            ref={frameRef}
            src={RAIL_SRC}
            title="Agent Companion 会话悬浮栏演示"
            // Keep the frame out of the dialog's initial tab order: Radix would
            // otherwise autofocus it and swallow Escape, leaving the overlay
            // closable only by its close button or the backdrop.
            tabIndex={-1}
            className="h-full w-full border-0"
          />
        </div>
        <p className="min-h-5 text-xs leading-5 text-muted-foreground" role="status">
          {notice ?? "演示数据自动循环：工作中 → 待确认 → 已完成 → 出错。"}
        </p>
      </DialogContent>
    </Dialog>
  );
}
