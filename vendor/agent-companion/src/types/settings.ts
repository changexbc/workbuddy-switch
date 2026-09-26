/**
 * Settings contract. `src/settings-config.js` owns the runtime defaults and
 * validation and is shared with the Node collector, so this file only describes
 * the shape it must produce; the JS file is checked against it with `@ts-check`.
 */

export type SourceId = 'codex' | 'workbuddy' | 'codebuddy-ide' | 'codeg';

export type AvatarStyle = 'animal' | 'bot';
export type RailSize = 'small' | 'medium' | 'standard';
export type RailVisibleCount = 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16;
export type RetentionHours = 0 | 0.5 | 24 | 168;
export type AssignmentMode = 'auto' | 'fixed';
/** Seat assignment is `'auto'` or a source pinned to that seat. */
export type SeatAssignment = 'auto' | SourceId;

export interface SourceConfig {
  enabled: boolean;
  path: string;
  /** WorkBuddy only: watch run logs for sandbox approvals (they emit no hook events). */
  logWatch?: boolean;
}

export interface MonitorSettings {
  avatarStyle: AvatarStyle;
  railVisibleCount: RailVisibleCount;
  autoDiscover: boolean;
  retentionHours: RetentionHours;
  assignment: AssignmentMode;
  /** Always eight entries; index is the seat. */
  seats: SeatAssignment[];
}

export interface SceneSettings {
  light: 'day' | 'night' | 'auto';
  weather: 'clear' | 'overcast' | 'rain' | 'downpour' | 'thunderstorm' | 'wind' | 'auto';
  lightning: boolean;
  door: boolean;
  ceiling: boolean;
  playing: boolean;
  speed: 1 | 2 | 4;
  maxFps: 30 | 60;
  renderResolution: 'native' | 'balanced' | 'low';
  showPerformance: boolean;
  reducedMotion: boolean;
  defaultView: 'all' | 'program' | 'device';
}

export interface NotificationSettings {
  desktop: boolean;
  wait: boolean;
  error: boolean;
  done: boolean;
  sound: boolean;
}

export interface GeneralSettings {
  mode: 'live' | 'demo';
  rememberView: boolean;
}

export interface ScheduleSettings {
  enabled: boolean;
  /** `HH:MM`, validated to sort before `end`. */
  start: string;
  end: string;
  deferBusy: boolean;
}

export interface Settings {
  version: 1;
  sources: Record<SourceId, SourceConfig>;
  monitor: MonitorSettings;
  scene: SceneSettings;
  notifications: NotificationSettings;
  general: GeneralSettings;
  schedule: ScheduleSettings;
}

/**
 * Rail preferences live in their own file and command
 * (`crates/agent-studio-desktop/src/rail_settings.rs`), separate from `Settings`.
 */
export interface RailPreferences {
  avatarStyle: AvatarStyle;
  visibleCount: number;
  animation: boolean;
  size: RailSize;
}

export interface RailPreferencesState extends RailPreferences {
  autostartSupported: boolean;
  /** False when the embedding host owns the login item; the settings page hides the row. */
  autostartManaged: boolean;
  autostart: boolean;
}
