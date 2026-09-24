import * as React from 'react';
import { Slot } from '@radix-ui/react-slot';
import { cva, type VariantProps } from 'class-variance-authority';
import { cn } from '@/lib/utils';

/**
 * Variants reproduce the two buttons that `settings.css` used to own, so the
 * stylesheet no longer needs control-level rules.
 *
 * No `outline-none` here. It sets `--tw-outline-style: none` on the element
 * unconditionally, and `outline-2` resolves `outline-style` from that variable,
 * so the two together produce a 2px outline whose style is `none` — i.e. no
 * ring at all. Measured: without it, `focus-visible` yields
 * `2px solid rgb(71,125,102)` at `4px`, matching the rule this replaced
 * (`button:focus-visible{outline:2px solid #477d66;outline-offset:4px}`).
 */
const buttonVariants = cva(
  'inline-flex items-center justify-center whitespace-nowrap transition-colors focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-50 disabled:cursor-default cursor-pointer',
  {
    variants: {
      variant: {
        /**
         * Was `.save`: 10px radius, solid #355c48, 10px/16px padding, 12px text.
         * `text-[12px]` rather than `text-xs` because the scale utilities also
         * set a line-height, which made the button 1px shorter than before.
         */
        default: 'rounded-md bg-primary px-4 py-2.5 text-[12px] text-primary-foreground hover:bg-primary-hover',
        /** Was `.retry`: 8px radius, 1px #abc4b3 border, 8px padding. */
        outline: 'rounded-sm border border-outline bg-secondary p-2 text-foreground hover:bg-accent',
      },
    },
    defaultVariants: { variant: 'default' },
  },
);

export type ButtonProps = React.ComponentProps<'button'> & VariantProps<typeof buttonVariants> & {asChild?: boolean};

export function Button({className, variant, asChild = false, ...props}: ButtonProps) {
  const Component = asChild ? Slot : 'button';
  return <Component data-slot="button" className={cn(buttonVariants({variant}), className)} {...props} />;
}

export { buttonVariants };
