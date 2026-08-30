# UI Component Guidelines

> Keep the desktop and WebUI surfaces visually and behaviorally consistent.

## Shadcn First

For frontend UI work, use this decision order:

1. Reuse an existing component from `src/components/ui/`.
2. Compose existing shadcn components without changing their interaction contracts.
3. If the component is missing, add the corresponding shadcn/Radix implementation and wrap it in `src/components/ui/`.
4. Create a custom component only when the shadcn component set and its composition APIs cannot meet the requirement.

Before choosing step 4, document the missing capability or concrete incompatibility. Visual preference alone is not sufficient reason to bypass shadcn.

## Project Theme

- Use the configured `radix-rhea` style: compact controls, rounded surfaces, restrained shadows, and high information density.
- Reuse semantic theme tokens (`primary`, `ring`, `border`, `popover`, `muted`, `destructive`, and `brand`) instead of introducing isolated colors.
- The project brand accent is the emerald color used by the account-card credit-package link. Product-specific actions may keep an explicitly approved color, such as the blue OAuth CTA.
- Shared interactive components belong in `src/components/ui/`; feature components should consume the wrapper rather than import a primitive directly.

## Statistics Page Layout Parity

Keep sibling statistics pages structurally consistent so users can transfer the same visual model between Token and Credit views:

- Put the page title, update timestamp, source selector, and refresh action in the page header, outside cards.
- Put each section heading in an external `h2`, then use the card only for that section's content.
- Keep section-specific filters in the corresponding `CardHeader`; date ranges should use the shared order `近 30 天 / 今天 / 近 7 天 / 本月`.
- A date-range change may filter the trend series and its range summary, but must not silently change full-period overview, composition, heatmap, or ranking data.

This prevents mixed card hierarchy and keeps the Token page aligned with `CreditStatsPage` while preserving source-specific data semantics.

## Interaction Requirements

Prefer component primitives that already provide the complete interaction contract:

- outside-click dismissal;
- Escape dismissal;
- keyboard navigation;
- focus trapping or focus return where appropriate;
- disabled and loading states;
- accessible names and state attributes;
- portal behavior for overlays inside clipped cards or panels.

Do not use native shortcuts such as `details/summary` as substitutes for menus, popovers, dialogs, selects, or tooltips when a matching shadcn component exists.

### External links across WebUI and Tauri

- Branch on the existing host capability check (`api.isWebui()`) before loading
  a Tauri-only plugin. Ordinary browser pages do not have Tauri's injected
  `invoke` object.
- In WebUI, use `window.open` only as a best-effort convenience and keep a
  normal `<a href target="_blank">` available as the user-gesture fallback;
  popup blocking must not be surfaced as a domain-operation failure.
- In Tauri, prevent the anchor's default navigation and delegate to
  `@tauri-apps/plugin-opener` so the system browser behavior remains unchanged.

## Review Checklist

- [ ] Searched `src/components/ui/` before creating a component.
- [ ] Used or added the matching shadcn wrapper where possible.
- [ ] Kept Rhea density and project theme tokens.
- [ ] Verified outside click, Escape, keyboard, focus, disabled, and loading behavior.
- [ ] Documented why custom UI was necessary if shadcn was insufficient.
