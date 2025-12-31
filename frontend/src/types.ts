export type Classification = 'unclassified' | 'notify' | 'ignored' | 'suppressed';

export interface Event {
  id: string;
  source: string;
  event_type: string;
  severity: string | null;
  summary: string;
  timestamp: number;
  classification: Classification;
  notified: boolean;
  created_at: number;
}

export interface EventTypeSummary {
  event_type: string;
  count: number;
  latest_timestamp: number;
  classification: Classification;
}

export interface Rule {
  event_type: string;
  classification: Classification;
}

export interface Stats {
  total_events: number;
  unclassified_types: number;
  notify_types: number;
  ignored_types: number;
}

export interface Filters {
  classifications: Set<Classification>;
  eventTypes: Set<string>;
  search: string;
}

// Auth types
export interface AuthStatus {
  authenticated: boolean;
  has_passkeys: boolean;
  needs_setup: boolean;
}

export interface PasskeyInfo {
  id: string;
  name: string | null;
  created_at: number;
}

export interface InviteToken {
  token: string;
  expires_in_secs: number;
}
