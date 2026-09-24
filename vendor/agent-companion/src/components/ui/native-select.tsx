import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * A native `<select>` with the project's own metrics. It deliberately keeps the
 * platform's own appearance (no `appearance: none`, no synthetic chevron) so the
 * control renders exactly as it did before the migration.
 *
 * No `outline-none`: it would set `--tw-outline-style: none`, which `outline-2`
 * then reads back as `outline-style`, cancelling the ring. See `button.tsx`.
 */
export function NativeSelect({className, ...props}: React.ComponentProps<'select'>) {
  return (
    <select
      data-slot="native-select"
      className={cn(
        'cursor-pointer rounded-sm border border-input bg-secondary px-[10px] py-[7px] text-[11px] text-strong focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring disabled:cursor-default disabled:opacity-40',
        className,
      )}
      {...props}
    />
  );
}
