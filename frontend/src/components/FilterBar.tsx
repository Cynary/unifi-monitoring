import { useState } from 'react';
import type { Classification } from '../types';

interface FilterBarProps {
  classifications: Set<Classification>;
  selectedEventTypes: Set<string>;
  search: string;
  onClassificationToggle: (classification: Classification) => void;
  onEventTypeRemove: (eventType: string) => void;
  onSearchSubmit: (search: string) => void;
}

export function FilterBar({
  classifications,
  selectedEventTypes,
  search,
  onClassificationToggle,
  onEventTypeRemove,
  onSearchSubmit,
}: FilterBarProps) {
  const [searchInput, setSearchInput] = useState(search);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      onSearchSubmit(searchInput);
    }
  };

  return (
    <div className="filter-bar">
      <div className="filter-row">
        <div className="classification-filters">
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={classifications.has('unclassified')}
              onChange={() => onClassificationToggle('unclassified')}
            />
            Unclassified
          </label>
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={classifications.has('notify')}
              onChange={() => onClassificationToggle('notify')}
            />
            Notify
          </label>
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={classifications.has('ignored')}
              onChange={() => onClassificationToggle('ignored')}
            />
            Ignored
          </label>
        </div>
        <div className="search-container">
          <input
            type="text"
            className="search-input"
            placeholder="Search... (press Enter)"
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            onKeyDown={handleKeyDown}
          />
        </div>
      </div>
      {selectedEventTypes.size > 0 && (
        <div className="selected-event-types">
          <span className="filter-label">Filtered:</span>
          {Array.from(selectedEventTypes).map((et) => (
            <span key={et} className="event-type-chip">
              {et}
              <button
                className="chip-remove"
                onClick={() => onEventTypeRemove(et)}
              >
                x
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
