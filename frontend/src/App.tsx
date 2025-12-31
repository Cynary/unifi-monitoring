import { useState, useEffect, useCallback } from 'react';
import type { Event, EventTypeSummary, Classification, Filters, Rule, AuthStatus } from './types';
import { fetchEvents, fetchEventTypes, fetchStats, fetchRules, fetchEventCount, fetchAuthStatus } from './api';
import { Sidebar } from './components/Sidebar';
import { FilterBar } from './components/FilterBar';
import { EventList } from './components/EventList';
import { Login } from './components/Login';
import { AccountMenu } from './components/AccountMenu';
import { useTheme } from './hooks/useTheme';
import './App.css';

const EVENTS_PER_PAGE = 200;

function App() {
  // Theme
  const { theme, toggleTheme } = useTheme();

  // Auth state
  const [authStatus, setAuthStatus] = useState<AuthStatus | null>(null);
  const [authLoading, setAuthLoading] = useState(true);
  // Event data
  const [events, setEvents] = useState<Event[]>([]);
  const [eventTypes, setEventTypes] = useState<EventTypeSummary[]>([]);
  const [rules, setRules] = useState<Rule[]>([]);
  const [stats, setStats] = useState({ total: 0, unclassified: 0, filtered: 0 });

  // Filters
  const [filters, setFilters] = useState<Filters>({
    classifications: new Set<Classification>(['unclassified', 'notify', 'ignored']),
    eventTypes: new Set<string>(),
    search: '',
  });

  // Loading states
  const [isLoading, setIsLoading] = useState(true);
  const [isLoadingMore, setIsLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(true);

  // Load events with current filters
  const loadEvents = useCallback(async (offset: number = 0, append: boolean = false) => {
    try {
      const data = await fetchEvents(filters, EVENTS_PER_PAGE, offset);
      if (append) {
        setEvents((prev) => [...prev, ...data]);
      } else {
        setEvents(data);
      }
      setHasMore(data.length === EVENTS_PER_PAGE);
    } catch (err) {
      console.error('Failed to load events:', err);
    }
  }, [filters]);

  // Load event types for sidebar
  const loadEventTypes = useCallback(async () => {
    try {
      const data = await fetchEventTypes();
      setEventTypes(data);
    } catch (err) {
      console.error('Failed to load event types:', err);
    }
  }, []);

  // Load stats
  const loadStats = useCallback(async () => {
    try {
      const [data, filteredCount] = await Promise.all([
        fetchStats(),
        fetchEventCount(filters),
      ]);
      setStats({
        total: data.total_events,
        unclassified: data.unclassified_types,
        filtered: filteredCount,
      });
    } catch (err) {
      console.error('Failed to load stats:', err);
    }
  }, [filters]);

  // Load rules (for suppressed section)
  const loadRules = useCallback(async () => {
    try {
      const data = await fetchRules();
      setRules(data);
    } catch (err) {
      console.error('Failed to load rules:', err);
    }
  }, []);

  // Check auth status on mount
  useEffect(() => {
    const checkAuth = async () => {
      try {
        const status = await fetchAuthStatus();
        setAuthStatus(status);
      } catch (err) {
        console.error('Failed to check auth status:', err);
        // If we can't check auth, set error state (not needs_setup)
        setAuthStatus({ authenticated: false, has_passkeys: true, needs_setup: false });
      } finally {
        setAuthLoading(false);
      }
    };
    checkAuth();
  }, []);

  // Handle successful auth
  const handleAuthSuccess = useCallback(async () => {
    // Refresh auth status
    const status = await fetchAuthStatus();
    setAuthStatus(status);
  }, []);

  // Handle logout
  const handleLogout = useCallback(() => {
    setAuthStatus({ authenticated: false, has_passkeys: true, needs_setup: false });
  }, []);

  // Initial load (only when authenticated)
  useEffect(() => {
    if (!authStatus?.authenticated) return;

    const init = async () => {
      setIsLoading(true);
      await Promise.all([loadEvents(), loadEventTypes(), loadStats(), loadRules()]);
      setIsLoading(false);
    };
    init();
  }, [authStatus?.authenticated]); // eslint-disable-line react-hooks/exhaustive-deps

  // Reload when filters change (except initial load)
  const reloadWithFilters = useCallback(async () => {
    setIsLoading(true);
    await Promise.all([loadEvents(), loadStats()]);
    setIsLoading(false);
  }, [loadEvents, loadStats]);

  // Handle classification toggle
  const handleClassificationToggle = useCallback((classification: Classification) => {
    setFilters((prev) => {
      const newClassifications = new Set(prev.classifications);
      if (newClassifications.has(classification)) {
        newClassifications.delete(classification);
      } else {
        newClassifications.add(classification);
      }
      return { ...prev, classifications: newClassifications };
    });
  }, []);

  // Trigger reload when classifications change
  useEffect(() => {
    if (!isLoading) {
      reloadWithFilters();
    }
  }, [filters.classifications]); // eslint-disable-line react-hooks/exhaustive-deps

  // Handle event type toggle from sidebar
  const handleEventTypeToggle = useCallback((eventType: string) => {
    setFilters((prev) => {
      const newEventTypes = new Set(prev.eventTypes);
      if (newEventTypes.has(eventType)) {
        newEventTypes.delete(eventType);
      } else {
        newEventTypes.add(eventType);
      }
      return { ...prev, eventTypes: newEventTypes };
    });
  }, []);

  // Trigger reload when event types filter changes
  useEffect(() => {
    if (!isLoading) {
      reloadWithFilters();
    }
  }, [filters.eventTypes]); // eslint-disable-line react-hooks/exhaustive-deps

  // Handle event type remove from filter bar
  const handleEventTypeRemove = useCallback((eventType: string) => {
    handleEventTypeToggle(eventType);
  }, [handleEventTypeToggle]);

  // Handle search submit
  const handleSearchSubmit = useCallback((search: string) => {
    setFilters((prev) => ({ ...prev, search }));
  }, []);

  // Trigger reload when search changes
  useEffect(() => {
    if (!isLoading) {
      reloadWithFilters();
    }
  }, [filters.search]); // eslint-disable-line react-hooks/exhaustive-deps

  // Handle load more (infinite scroll)
  const handleLoadMore = useCallback(async () => {
    if (isLoadingMore || !hasMore) return;
    setIsLoadingMore(true);
    await loadEvents(events.length, true);
    setIsLoadingMore(false);
  }, [isLoadingMore, hasMore, events.length, loadEvents]);

  // Handle refresh button
  const handleRefresh = useCallback(async () => {
    setIsLoading(true);
    await Promise.all([loadEvents(), loadEventTypes(), loadStats(), loadRules()]);
    setIsLoading(false);
  }, [loadEvents, loadEventTypes, loadStats, loadRules]);

  // Handle rule change from sidebar
  const handleRuleChange = useCallback(async () => {
    await Promise.all([loadEvents(), loadEventTypes(), loadStats(), loadRules()]);
  }, [loadEvents, loadEventTypes, loadStats, loadRules]);

  // Check if filters are active (not default state)
  const hasActiveFilters =
    filters.classifications.size !== 3 ||
    filters.eventTypes.size > 0 ||
    filters.search.trim() !== '';

  // Show loading spinner while checking auth
  if (authLoading) {
    return (
      <div className="login-container">
        <div className="loading-overlay">
          <div className="spinner" />
          <span>Loading...</span>
        </div>
      </div>
    );
  }

  // Show login if not authenticated
  if (!authStatus?.authenticated) {
    return (
      <Login
        authStatus={authStatus || { authenticated: false, has_passkeys: false, needs_setup: true }}
        onAuthSuccess={handleAuthSuccess}
      />
    );
  }

  return (
    <div className="app">
      <header className="header">
        <h1>UniFi Event Monitor</h1>
        <div className="header-stats">
          <span>
            {hasActiveFilters
              ? `${stats.filtered.toLocaleString()} / ${stats.total.toLocaleString()} events`
              : `${stats.total.toLocaleString()} events`}
          </span>
          {stats.unclassified > 0 && (
            <span className="unclassified-badge">
              {stats.unclassified} unclassified types
            </span>
          )}
          <AccountMenu onLogout={handleLogout} theme={theme} onToggleTheme={toggleTheme} />
        </div>
      </header>

      <div className="main">
        <Sidebar
          eventTypes={eventTypes}
          rules={rules}
          selectedEventTypes={filters.eventTypes}
          onToggleEventType={handleEventTypeToggle}
          onRuleChange={handleRuleChange}
        />

        <div className="events-panel">
          <FilterBar
            classifications={filters.classifications}
            selectedEventTypes={filters.eventTypes}
            search={filters.search}
            onClassificationToggle={handleClassificationToggle}
            onEventTypeRemove={handleEventTypeRemove}
            onSearchSubmit={handleSearchSubmit}
          />

          <EventList
            events={events}
            isLoading={isLoading}
            isLoadingMore={isLoadingMore}
            hasMore={hasMore}
            onLoadMore={handleLoadMore}
            onRefresh={handleRefresh}
          />
        </div>
      </div>
    </div>
  );
}

export default App;
