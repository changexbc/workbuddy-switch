import { Check, Users } from "lucide-react";

import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

function accountLabel(account: { accountId: string; accountName?: string | null }): string {
  return account.accountName || account.accountId;
}

export function AccountFilterMenu({
  accounts,
  accountFilter,
  onAccountFilterChange,
  ariaLabel,
  allowAll = true,
}: {
  accounts: { accountId: string; accountName?: string | null }[];
  accountFilter: string | null;
  onAccountFilterChange: (accountId: string | null) => void;
  ariaLabel: string;
  /** false 时隐藏「所有账号」选项，仅允许选择具体账号 */
  allowAll?: boolean;
}) {
  const activeFilterAccount =
    accountFilter && accounts.some((account) => account.accountId === accountFilter)
      ? accounts.find((account) => account.accountId === accountFilter)
      : undefined;
  const effectiveFilter = activeFilterAccount?.accountId ?? null;

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="h-8 max-w-[190px] gap-1.5 px-2.5 text-xs text-muted-foreground hover:text-foreground"
          aria-label={ariaLabel}
        >
          <Users className="size-3.5 shrink-0" />
          <span className="truncate">
            {activeFilterAccount
              ? accountLabel(activeFilterAccount)
              : allowAll
                ? "所有账号"
                : accounts[0]
                  ? accountLabel(accounts[0])
                  : "无账号"}
          </span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-80 w-56 overflow-y-auto">
        {allowAll && (
          <>
            <DropdownMenuItem onSelect={() => onAccountFilterChange(null)}>
              <Users className="size-3.5 shrink-0" />
              所有账号
              {!effectiveFilter && <Check className="ml-auto size-3.5 shrink-0" />}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        )}
        {accounts.map((account) => (
          <DropdownMenuItem key={account.accountId} onSelect={() => onAccountFilterChange(account.accountId)}>
            <span className="min-w-0 flex-1 truncate">{accountLabel(account)}</span>
            {effectiveFilter === account.accountId && <Check className="ml-auto size-3.5 shrink-0" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
