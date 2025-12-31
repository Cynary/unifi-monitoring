import { useRef, useCallback, useEffect } from 'react';
import type { Event } from '../types';
import { EventRow } from './EventRow';

interface EventListProps {
  events: Event[];
  isLoading: boolean;
  isLoadingMore: boolean;
  hasMore: boolean;
  onLoadMore: () => void;
  onRefresh: () => void;
}

export function EventList({
  events,
  isLoading,
  isLoadingMore,
  hasMore,
  onLoadMore,
  onRefresh,
}: EventListProps) {
  const scrollRef = useRef<HTMLDivElement>(null);

  // Handle scroll for infinite loading
  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;

    // Check if near bottom for loading more
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 200;
    if (nearBottom && hasMore && !isLoadingMore && !isLoading) {
      onLoadMore();
    }
  }, [hasMore, isLoadingMore, isLoading, onLoadMore]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.addEventListener('scroll', handleScroll);
    return () => el.removeEventListener('scroll', handleScroll);
  }, [handleScroll]);

  if (isLoading && events.length === 0) {
    return (
      <div className="event-list-container">
        <div className="loading-overlay">
          <div className="spinner" />
          <span>Loading events...</span>
        </div>
      </div>
    );
  }

  return (
    <div className="event-list-container">
      {/* Loading overlay for filter/search changes */}
      {isLoading && events.length > 0 && (
        <div className="loading-overlay-transparent">
          <div className="spinner" />
        </div>
      )}

      <div ref={scrollRef} className="event-list-scroll">
        <button className="refresh-btn" onClick={onRefresh} disabled={isLoading}>
          {isLoading ? 'Refreshing...' : 'Refresh for new events'}
        </button>
        {events.map((event) => (
          <EventRow key={event.id} event={event} />
        ))}

        {/* Loading more indicator at bottom */}
        {isLoadingMore && (
          <div className="loading-more">
            <div className="spinner small" />
            <span>Loading more...</span>
          </div>
        )}

        {/* No more events indicator */}
        {!hasMore && events.length > 0 && (
          <div className="no-more-events">No more events</div>
        )}

        {/* Empty state */}
        {events.length === 0 && !isLoading && (
          <div className="empty-state">No events match your filters</div>
        )}
      </div>
    </div>
  );
}
