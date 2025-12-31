import { useState } from 'react';
import type { EventTypeSummary, Classification, Rule } from '../types';
import { setRule, deleteRule } from '../api';

interface SidebarProps {
  eventTypes: EventTypeSummary[];
  rules: Rule[];
  selectedEventTypes: Set<string>;
  onToggleEventType: (eventType: string) => void;
  onRuleChange: () => void;
}

interface SectionProps {
  title: string;
  icon: string;
  classification: Classification;
  eventTypes: EventTypeSummary[];
  selectedEventTypes: Set<string>;
  onToggleEventType: (eventType: string) => void;
  onRuleChange: () => void;
}

function EventTypeSection({
  title,
  icon,
  classification,
  eventTypes,
  selectedEventTypes,
  onToggleEventType,
  onRuleChange,
}: SectionProps) {
  const [expanded, setExpanded] = useState(classification === 'unclassified');
  const filtered = eventTypes
    .filter((et) => et.classification === classification)
    .sort((a, b) => a.event_type.localeCompare(b.event_type));

  if (filtered.length === 0) return null;

  const handleAction = async (eventType: string, newClassification: Classification) => {
    try {
      await setRule(eventType, newClassification);
      onRuleChange();
    } catch (err) {
      console.error('Failed to set rule:', err);
    }
  };

  return (
    <div className="sidebar-section">
      <button
        className="sidebar-section-header"
        onClick={() => setExpanded(!expanded)}
      >
        <span>
          {icon} {title} ({filtered.length})
        </span>
        <span className="chevron">{expanded ? '▼' : '▶'}</span>
      </button>
      {expanded && (
        <div className="sidebar-section-content">
          {filtered.map((et) => (
            <div
              key={et.event_type}
              className={`event-type-card ${selectedEventTypes.has(et.event_type) ? 'selected' : ''}`}
            >
              <div
                className="event-type-info"
                onClick={() => onToggleEventType(et.event_type)}
              >
                <span className="event-type-name">{et.event_type}</span>
                <span className="event-type-count">{et.count}</span>
              </div>
              <div className="event-type-actions">
                {classification === 'unclassified' && (
                  <>
                    <button
                      className="action-btn suppress"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'suppressed');
                      }}
                      title="Stop storing this event type"
                    >
                      Suppress
                    </button>
                    <button
                      className="action-btn ignore"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'ignored');
                      }}
                    >
                      Ignore
                    </button>
                    <button
                      className="action-btn notify"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'notify');
                      }}
                    >
                      Notify
                    </button>
                  </>
                )}
                {classification === 'notify' && (
                  <>
                    <button
                      className="action-btn suppress"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'suppressed');
                      }}
                      title="Stop storing this event type"
                    >
                      Suppress
                    </button>
                    <button
                      className="action-btn ignore"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'ignored');
                      }}
                    >
                      Ignore
                    </button>
                  </>
                )}
                {classification === 'ignored' && (
                  <>
                    <button
                      className="action-btn suppress"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'suppressed');
                      }}
                      title="Stop storing this event type"
                    >
                      Suppress
                    </button>
                    <button
                      className="action-btn notify"
                      onClick={(e) => {
                        e.stopPropagation();
                        handleAction(et.event_type, 'notify');
                      }}
                    >
                      Notify
                    </button>
                  </>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

interface SuppressedSectionProps {
  rules: Rule[];
  onRuleChange: () => void;
}

function SuppressedSection({ rules, onRuleChange }: SuppressedSectionProps) {
  const [expanded, setExpanded] = useState(false);
  const suppressed = rules
    .filter((r) => r.classification === 'suppressed')
    .sort((a, b) => a.event_type.localeCompare(b.event_type));

  if (suppressed.length === 0) return null;

  const handleUnsuppress = async (eventType: string) => {
    try {
      await deleteRule(eventType);
      onRuleChange();
    } catch (err) {
      console.error('Failed to delete rule:', err);
    }
  };

  return (
    <div className="sidebar-section">
      <button
        className="sidebar-section-header"
        onClick={() => setExpanded(!expanded)}
      >
        <span>
          🚫 Suppressed ({suppressed.length})
        </span>
        <span className="chevron">{expanded ? '▼' : '▶'}</span>
      </button>
      {expanded && (
        <div className="sidebar-section-content">
          {suppressed.map((rule) => (
            <div key={rule.event_type} className="event-type-card">
              <div className="event-type-info">
                <span className="event-type-name">{rule.event_type}</span>
                <span className="event-type-count suppressed">not stored</span>
              </div>
              <div className="event-type-actions">
                <button
                  className="action-btn unsuppress"
                  onClick={() => handleUnsuppress(rule.event_type)}
                  title="Start storing this event type again"
                >
                  Unsuppress
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function Sidebar({
  eventTypes,
  rules,
  selectedEventTypes,
  onToggleEventType,
  onRuleChange,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <EventTypeSection
        title="Unclassified"
        icon="⚠️"
        classification="unclassified"
        eventTypes={eventTypes}
        selectedEventTypes={selectedEventTypes}
        onToggleEventType={onToggleEventType}
        onRuleChange={onRuleChange}
      />
      <EventTypeSection
        title="Notify"
        icon="🔔"
        classification="notify"
        eventTypes={eventTypes}
        selectedEventTypes={selectedEventTypes}
        onToggleEventType={onToggleEventType}
        onRuleChange={onRuleChange}
      />
      <EventTypeSection
        title="Ignored"
        icon="🔇"
        classification="ignored"
        eventTypes={eventTypes}
        selectedEventTypes={selectedEventTypes}
        onToggleEventType={onToggleEventType}
        onRuleChange={onRuleChange}
      />
      <SuppressedSection
        rules={rules}
        onRuleChange={onRuleChange}
      />
    </aside>
  );
}
