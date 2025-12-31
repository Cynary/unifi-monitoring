import { useState } from 'react';
import type { Event } from '../types';
import { fetchEventPayload } from '../api';

interface EventRowProps {
  event: Event;
}

function formatTimestamp(timestamp: number): string {
  const date = new Date(timestamp * 1000);
  const now = new Date();
  const diff = now.getTime() - date.getTime();

  if (diff < 60000) {
    return 'just now';
  } else if (diff < 3600000) {
    const mins = Math.floor(diff / 60000);
    return `${mins} min ago`;
  } else if (diff < 86400000) {
    const hours = Math.floor(diff / 3600000);
    return `${hours}h ago`;
  } else {
    return date.toLocaleDateString() + ' ' + date.toLocaleTimeString();
  }
}

function getClassificationIcon(classification: string): string {
  switch (classification) {
    case 'notify':
      return '🔔';
    case 'ignored':
      return '🔇';
    default:
      return '⚠️';
  }
}

export function EventRow({ event }: EventRowProps) {
  const [expanded, setExpanded] = useState(false);
  const [payload, setPayload] = useState<unknown>(null);
  const [loadingPayload, setLoadingPayload] = useState(false);

  const handleExpand = async () => {
    if (!expanded && payload === null) {
      setLoadingPayload(true);
      try {
        const data = await fetchEventPayload(event.id);
        setPayload(data);
      } catch (err) {
        console.error('Failed to fetch payload:', err);
        setPayload({ error: 'Failed to load payload' });
      } finally {
        setLoadingPayload(false);
      }
    }
    setExpanded(!expanded);
  };

  return (
    <div className={`event-row ${expanded ? 'expanded' : ''}`}>
      <div className="event-row-header" onClick={handleExpand}>
        <span className="event-icon">{getClassificationIcon(event.classification)}</span>
        <span className="event-type">{event.event_type}</span>
        <span className="event-separator">|</span>
        <span className="event-source">{event.source}</span>
        <span className="event-separator">|</span>
        <span className="event-time">{formatTimestamp(event.timestamp)}</span>
      </div>
      <div className="event-summary">{event.summary}</div>
      {expanded && (
        <div className="event-details">
          {loadingPayload ? (
            <div className="payload-loading">Loading...</div>
          ) : (
            <pre className="payload-json">
              {JSON.stringify(payload, null, 2)}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
