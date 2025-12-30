# UI Redesign Plan

## Overview

Complete rewrite of the frontend to fix fundamental architecture issues. The UI is a **window into server-side filtered data**, not a client-side cache.

## Core Concept

```
Server DB: [All Events]
              │
              ▼ filters (classifications[], event_types[], search)
Filtered Results: [Matching Events ordered by timestamp DESC]
              │
              ▼ window (offset based on scroll position)
UI Window: [200 events max]
```

- **All filtering/search happens server-side**
- **UI shows a sliding 200-event window** into filtered results
- **SSE streams new events**, client filters them to match current criteria
- **Optimistic search**: immediately filter visible events client-side while fetching from server

---

## Backend Changes

### 1. Reclassify Events on Rule Change

When a rule is created/updated, update ALL existing events of that type:

```rust
// In set_rule():
pub fn set_rule(&self, event_type: &str, classification: Classification) -> rusqlite::Result<()> {
    let conn = self.conn.lock().unwrap();
    let now = chrono::Utc::now().timestamp();

    // Update or insert rule
    conn.execute(
        r#"INSERT INTO event_type_rules (event_type, classification, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?3)
           ON CONFLICT(event_type) DO UPDATE SET
               classification = excluded.classification,
               updated_at = excluded.updated_at"#,
        params![event_type, classification.as_str(), now],
    )?;

    // UPDATE ALL EXISTING EVENTS OF THIS TYPE
    conn.execute(
        "UPDATE events SET classification = ?1 WHERE event_type = ?2",
        params![classification.as_str(), event_type],
    )?;

    Ok(())
}

// In delete_rule() - revert to unclassified:
pub fn delete_rule(&self, event_type: &str) -> rusqlite::Result<bool> {
    let conn = self.conn.lock().unwrap();
    let rows = conn.execute(
        "DELETE FROM event_type_rules WHERE event_type = ?1",
        params![event_type],
    )?;

    if rows > 0 {
        // Revert events to unclassified
        conn.execute(
            "UPDATE events SET classification = 'unclassified' WHERE event_type = ?1",
            params![event_type],
        )?;
    }

    Ok(rows > 0)
}
```

### 2. Multi-Filter API

Update `GET /api/events` to accept multiple classifications and event types:

```
GET /api/events?classification=notify&classification=ignored&event_type=motion&event_type=ring&search=camera&limit=200&offset=0
```

```rust
#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
    #[serde(default)]
    classification: Vec<String>,  // Multiple allowed
    #[serde(default)]
    event_type: Vec<String>,      // Multiple allowed
    search: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}
```

SQL query building:
```rust
let mut sql = "SELECT ... FROM events WHERE 1=1".to_string();

// Classifications filter (OR within, AND with others)
if !query.classification.is_empty() {
    let placeholders: Vec<_> = query.classification.iter().map(|_| "?").collect();
    sql.push_str(&format!(" AND classification IN ({})", placeholders.join(",")));
}

// Event types filter (OR within, AND with others)
if !query.event_type.is_empty() {
    let placeholders: Vec<_> = query.event_type.iter().map(|_| "?").collect();
    sql.push_str(&format!(" AND event_type IN ({})", placeholders.join(",")));
}

// Search filter
if let Some(q) = &query.search {
    sql.push_str(" AND (event_type LIKE ? OR summary LIKE ? OR source LIKE ? OR payload LIKE ?)");
}

sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ? OFFSET ?");
```

### 3. Total Count Endpoint

Add endpoint to get total matching events (for scroll calculations):

```
GET /api/events/count?classification=notify&event_type=motion&search=camera
```

Returns: `{ "count": 5432 }`

---

## Frontend Architecture

### State Model

```typescript
interface AppState {
  // Filters (all multi-select)
  filters: {
    classifications: Set<'unclassified' | 'notify' | 'ignored'>;  // Default: all 3
    eventTypes: Set<string>;  // Default: empty (means all)
    search: string;           // Default: ''
  };

  // Event window
  events: Event[];           // Current window, max 200
  totalCount: number;        // Total matching events on server
  windowOffset: number;      // Current offset into filtered results

  // UI state
  expandedEvent: string | null;
  isAtTop: boolean;
  eventsAbove: number;       // Count of events added above (max ~100)

  // Event types sidebar
  eventTypeSummaries: EventTypeSummary[];  // Sorted alphabetically within classification

  // Loading states
  isLoading: boolean;
  isLoadingMore: boolean;
}
```

### Components Structure

```
App
├── Header
│   └── Stats (total events, unclassified count, etc.)
├── Main
│   ├── Sidebar (event types by classification)
│   │   ├── UnclassifiedSection
│   │   │   └── EventTypeCard[] (sorted alphabetically)
│   │   │       └── Actions: [Ignore] [Notify]
│   │   ├── NotifySection
│   │   │   └── EventTypeCard[] (sorted alphabetically)
│   │   │       └── Actions: [Ignore]
│   │   └── IgnoredSection
│   │       └── EventTypeCard[] (sorted alphabetically)
│   │           └── Actions: [Notify]
│   └── EventsPanel
│       ├── FiltersBar
│       │   ├── ClassificationCheckboxes (multi-select)
│       │   ├── EventTypeChips (multi-select)
│       │   └── SearchInput
│       └── VirtualEventList
│           └── EventRow[] (virtualized, 200 max loaded)
│               └── EventDetails (expandable JSON)
```

---

## Key Behaviors

### 1. Search/Filter Change

```
User changes filter/search
        │
        ├──► Immediate (sync):
        │    - Filter current events[] client-side
        │    - Show filtered results instantly
        │
        └──► Concurrent (async):
             - Fetch GET /api/events with new filters
             - Fetch GET /api/events/count with new filters
             - On response: replace events[], update totalCount
             - Reset windowOffset to 0 (top)
```

### 2. Scroll Behavior

```
User scrolls
        │
        ├──► Near top (within 200px):
        │    - If windowOffset > 0: fetch newer events
        │    - Prepend to events[], increment windowOffset
        │
        ├──► Near bottom (within 200px):
        │    - If windowOffset + events.length < totalCount: fetch older events
        │    - Append to events[]
        │
        └──► Maintain window size:
             - If events.length > 200: trim from opposite end
             - Track isAtTop (scrollTop < 10)
```

### 3. SSE Event Handling

```
New event arrives via SSE
        │
        ├──► Check if matches current filters:
        │    - Classification in filters.classifications?
        │    - Event type in filters.eventTypes (or empty)?
        │    - Matches search query?
        │
        ├──► If doesn't match: ignore
        │
        └──► If matches:
             ├──► If isAtTop AND expandedEvent is null:
             │    - Prepend to events[]
             │    - Trim from bottom if > 200
             │
             └──► If NOT at top OR event expanded:
                  - If eventsAbove < 100:
                      - Prepend to events[]
                      - Increment eventsAbove
                      - Scroll position stays (user sees scrollbar move)
                  - If eventsAbove >= 100:
                      - Don't add (server has it, load on scroll up)
                  - Increment totalCount
```

### 4. Sidebar Stability

```
On SSE event or periodic refresh:
        │
        ├──► Update counts in existing EventTypeSummary
        │
        ├──► Add new event types in sorted position
        │
        └──► NEVER re-sort or re-render entire list
             (React key by event_type ensures stability)
```

Sort order: Alphabetical by event_type within each classification section.

### 5. Classification Actions

| Current | Available Actions |
|---------|-------------------|
| Unclassified | `[Ignore]` `[Notify]` |
| Notify | `[Ignore]` |
| Ignored | `[Notify]` |

On action:
1. POST /api/rules with new classification
2. Backend updates rule AND all existing events
3. Refresh event types (counts/classifications change)
4. If current filter excludes new classification, events disappear from view

---

## Virtual Scrolling

Use `@tanstack/react-virtual` for efficient rendering:

```typescript
const rowVirtualizer = useVirtualizer({
  count: totalCount,  // Total matching events on server
  getScrollElement: () => scrollRef.current,
  estimateSize: () => 48,  // Estimated row height
  overscan: 10,
});

// Only render visible rows
{rowVirtualizer.getVirtualItems().map((virtualRow) => {
  const event = events[virtualRow.index - windowOffset];
  if (!event) {
    // Event not loaded yet - trigger fetch
    return <EventRowSkeleton key={virtualRow.index} />;
  }
  return <EventRow key={event.id} event={event} />;
})}
```

---

## API Summary

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/api/events` | GET | Fetch events with filters |
| `/api/events/count` | GET | Get total count for filters |
| `/api/events/types` | GET | Get event type summaries |
| `/api/events/stream` | GET (SSE) | Live event stream |
| `/api/rules` | GET | List all rules |
| `/api/rules` | POST | Create/update rule (+ update events) |
| `/api/rules/{type}` | DELETE | Delete rule (+ revert events) |
| `/api/stats` | GET | Dashboard stats |

---

## File Structure

```
frontend/src/
├── App.tsx              # Main layout, SSE connection
├── App.css              # Global styles
├── api.ts               # API client functions
├── types.ts             # TypeScript interfaces
├── hooks/
│   ├── useEvents.ts     # Event fetching, filtering, pagination
│   ├── useSSE.ts        # SSE connection management
│   └── useFilters.ts    # Filter state management
├── components/
│   ├── Header.tsx
│   ├── Sidebar/
│   │   ├── Sidebar.tsx
│   │   ├── EventTypeSection.tsx
│   │   └── EventTypeCard.tsx
│   ├── Events/
│   │   ├── EventsPanel.tsx
│   │   ├── FiltersBar.tsx
│   │   ├── VirtualEventList.tsx
│   │   ├── EventRow.tsx
│   │   └── EventDetails.tsx
│   └── ui/
│       ├── Checkbox.tsx
│       ├── MultiSelect.tsx
│       └── SearchInput.tsx
└── utils/
    └── filterEvents.ts  # Client-side filtering for optimistic updates
```

---

## Implementation Order

1. **Backend changes**
   - Update `set_rule` to reclassify existing events
   - Update `delete_rule` to revert events to unclassified
   - Update `query_events` for multi-value classification/event_type filters
   - Add `/api/events/count` endpoint

2. **Scrap frontend**
   - Delete current App.tsx, App.css, api.ts contents

3. **Rebuild frontend**
   - Types and API client
   - Hooks (useEvents, useSSE, useFilters)
   - Components bottom-up (EventRow → VirtualEventList → EventsPanel → Sidebar → App)
   - Styling

4. **Testing**
   - Filter combinations
   - Search + filter
   - SSE with filters
   - Scroll loading
   - Classification changes
