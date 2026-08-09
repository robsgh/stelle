export type Theme = 'light' | 'dark' | 'system';

export interface LinkWidget {
  type: 'link';
  label: string;
  description: string;
  url: string;
  accent: string | null;
}

export interface LuaWidget {
  type: 'lua';
  id: string;
}

export type Widget = LinkWidget | LuaWidget;

export interface Dashboard {
  title: string;
  subtitle: string;
  theme: Theme;
  accent: string;
  widgets: Widget[];
}

export interface StatsContent {
  title: string;
  subtitle: string;
  href?: string;
  metrics: Array<{ label: string; value: string | number | boolean | null }>;
  fetched_at: string;
}

export type WidgetState =
  | { status: 'loading' }
  | { status: 'ready'; content: StatsContent }
  | { status: 'error'; message: string };
