import * as React from 'react';
import * as Primitive from '@radix-ui/react-alert-dialog';
import { cn } from '@/lib/utils';
import { buttonVariants } from './button';

export const AlertDialog = Primitive.Root;
export const AlertDialogTrigger = Primitive.Trigger;
export const AlertDialogTitle = Primitive.Title;
export const AlertDialogDescription = Primitive.Description;
export function AlertDialogContent({className, ...props}: React.ComponentProps<typeof Primitive.Content>) {
  return <Primitive.Portal>
    <Primitive.Overlay className="settings-dialog-overlay" />
    <Primitive.Content className={cn('settings-dialog', className)} {...props} />
  </Primitive.Portal>;
}
export function AlertDialogCancel({className, ...props}: React.ComponentProps<typeof Primitive.Cancel>) {
  return <Primitive.Cancel className={cn(buttonVariants({variant: 'outline'}), className)} {...props} />;
}
export function AlertDialogAction({className, ...props}: React.ComponentProps<typeof Primitive.Action>) {
  return <Primitive.Action className={cn(buttonVariants(), className)} {...props} />;
}
