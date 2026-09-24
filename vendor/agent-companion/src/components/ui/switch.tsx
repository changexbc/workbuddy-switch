import * as React from 'react';
import * as SwitchPrimitive from '@radix-ui/react-switch';
import { cn } from '@/lib/utils';

/**
 * Reproduces the toggle that `settings.css` used to draw on `input[type=checkbox]`:
 * a 33x20 track, 12px radius, #ccd7cf / #719c82 fill and a 14px thumb that slides
 * from 3px to 16px. `group-data-[state=checked]` is needed because Radix puts
 * `data-state` on the root, not on the thumb.
 *
 * No `outline-none`: it would set `--tw-outline-style: none`, which `outline-2`
 * then reads back as `outline-style`, cancelling the ring. See `button.tsx`.
 */
export function Switch({className, ...props}: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      className={cn(
        'group relative inline-flex h-5 w-[33px] shrink-0 cursor-pointer items-center rounded-full bg-track transition-colors focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring disabled:cursor-not-allowed disabled:opacity-40 data-[state=checked]:bg-track-on',
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb
        data-slot="switch-thumb"
        className="pointer-events-none block size-3.5 translate-x-[3px] rounded-full bg-white shadow-[0_1px_3px_#29423925] transition-transform group-data-[state=checked]:translate-x-[16px]"
      />
    </SwitchPrimitive.Root>
  );
}
