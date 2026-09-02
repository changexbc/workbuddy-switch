# Local Token Statistics Contract

## Scenario: WorkBuddy and CodeBuddy CLI token dashboard

### 1. Scope / Trigger

- Trigger: the token dashboard adds a cross-layer local JSONL aggregation API.
- Scope: decode usage records from the two local sources, aggregate safe numeric
  projections, and expose the same payload through Tauri and HTTP.
- The decoder in `crates/wb-switch-core/src/modules/token_stats.rs` is the sole
  owner of event parsing, source isolation, filtering, and aggregation.

### 2. Signatures

- Core: `token_stats::get_statistics(days: Option<i64>) -> serde_json::Value`.
- Tauri: `get_token_statistics(days: Option<i64>) -> Result<serde_json::Value, String>`;
  the command reports blocking-worker join failures through the `Err` branch.
- HTTP: `GET /api/token-stats` with optional query parameter `days`.
- Accepted range values are `7`, `30`, and `90`; all other values mean the
  complete available local history (`rangeDays: null`).

### 3. Contracts

#### Request

- `days` is an optional signed integer at the HTTP/Tauri boundary.
- Sources are fixed local roots: `~/.workbuddy/projects` and
  `~/.codebuddy/projects`.

#### Response

```json
{
  "generatedAt": 0,
  "rangeDays": 7,
  "sources": [{
    "source": "workbuddy",
    "summary": {
      "total": 0,
      "input": 0,
      "output": 0,
      "cacheRead": 0,
      "cacheWrite": 0,
      "uncachedInput": 0,
      "records": 0,
      "cacheHitRate": null
    },
    "models": [],
    "projects": [],
    "sessions": [{
      "key": "project · session-id",
      "title": "Readable session title",
      "project": "project",
      "sessionId": "session-id",
      "input": 0,
      "output": 0,
      "cacheRead": 0,
      "cacheWrite": 0,
      "uncachedInput": 0,
      "records": 0,
      "cacheHitRate": null
    }],
    "daily": [],
    "dailyByModel": {
      "model-id": []
    },
    "hours": [],
    "filesScanned": 0,
    "parseErrors": 0,
    "coverageStartAt": null,
    "coverageEndAt": null
  }]
}
```

- `summary.input` includes the provider-reported input total, including cached
  input. `summary.uncachedInput = input - cacheRead` (saturating at zero).
- `cacheWrite` accepts only explicit provider write aliases:
  `cache_write_input_tokens`, `cacheWriteInputTokens`,
  `cache_creation_input_tokens`, and `prompt_cache_write_tokens`. When the
  selected usage object has no positive write value, the decoder may inspect
  the other usage objects and `providerData.rawUsage` for those aliases only.
  `prompt_cache_miss_tokens` is not a write alias; it represents uncached input
  and must remain part of `uncachedInput`.
- `summary.cacheHitRate` is `cacheRead / input` when input is positive;
  otherwise it is `null`.
- Dashboard total is derived from `input + output + cacheWrite`; composition
  uses `cacheRead`, `uncachedInput`, `output`, and `cacheWrite`.
- `models`, `projects`, `sessions`, `daily`, and `hours` contain `{ key, total,
  input, output, cacheRead, cacheWrite, uncachedInput, records }` projections,
  sorted by descending `total`.
- `dailyByModel` is an object keyed by the same model labels exposed in
  `models`; each value is a daily projection array using the same totals shape
  as `daily`. It is additive and may be absent in older responses. Consumers
  must keep the aggregate `daily` series as the default and only expose a model
  filter when `dailyByModel` is available; they must never fake a model trend
  by reusing the aggregate series.
- Session projections additionally expose optional `title` plus safe `project`
  and `sessionId` labels. `aiTitle` is the preferred title; the latest non-empty
  `summary` in the same JSONL file is the fallback. Title metadata is collected
  before applying the usage time cutoff, so an older title can label in-range
  usage. The JSONL file remains the session identity boundary: equal titles
  never merge two files or change their token totals.
- Project labels are reduced to a safe basename. Absolute path-like encoded
  project keys are not returned. Message bodies, arguments, raw paths, and
  authentication data are never included.
- Records under a directory named exactly `subagents` are excluded. A record is
  counted once using usage precedence `message.usage`, then
  `providerData.usage`, then top-level `usage`; `providerData.rawUsage` only
  supplements cache-write fields absent from the selected usage object.

### 4. Validation & Error Matrix

| Condition | Required behavior |
|---|---|
| `days=7`, `30`, or `90` | Apply one shared millisecond cutoff to both sources. |
| Missing/invalid `days` | Scan complete history and return `rangeDays: null`. |
| Missing source directory | Return an empty source, not an API error. |
| Invalid JSONL line | Skip the line and increment `parseErrors`. |
| Missing timestamp or outside cutoff | Do not aggregate the record. |
| Missing input usage field | Ignore the record as non-usage content. |
| Zero input with a valid usage field | Count the record; hit rate remains `null`. |
| Explicit cache-write aliases are missing or zero | Return `cacheWrite: 0`; the UI explains that this source has no explicit write quantity. |
| Positive write alias exists only in another usage object or `rawUsage` | Use that positive explicit value without counting input/output twice. |
| Only `prompt_cache_miss_tokens` is positive | Keep `cacheWrite: 0`; derive uncached input from `input - cacheRead`. |
| `subagents` directory | Do not scan files below that directory. |
| Title event before/after usage | Associate the latest non-empty title in the same file. |
| Title timestamp before cutoff | Keep the title when that file has in-range usage. |
| Missing/blank title | Return `title: null`; the UI uses a redacted session fallback. |
| Equal titles in different files | Return separate session groups with stable unique keys. |
| Older response without `dailyByModel` | Keep the aggregate daily trend and do not expose model-specific options. |
| Selected model has no dated usage | Return an empty model trend; never fall back to the aggregate series under that model label. |

### 5. Good / Base / Bad Cases

- Good: a record with `message.usage.input_tokens` and aliases for output/cache
  fields is normalized and appears once in all applicable groups.
- Good cache write: `message.usage` supplies canonical input/output while
  `providerData.rawUsage.prompt_cache_write_tokens` supplies a positive write;
  the record still counts once and preserves that explicit write.
- Base: an empty local source returns zero totals, empty groups, and no error.
- Base cache write: write aliases are present but all zero, so the response
  keeps `cacheWrite: 0` and never invents a write amount.
- Bad: a record containing only provider metadata or message text contributes no
  tokens; malformed JSON contributes only to `parseErrors`.
- Bad cache write: mapping `prompt_cache_miss_tokens` to `cacheWrite` falsely
  labels newly computed input as cache creation.
- Good title: a file containing `aiTitle`, `summary`, and usage returns the
  latest non-empty `aiTitle` without changing usage totals.
- Base title: a session without title metadata returns `title: null` and safe
  project/session labels.
- Bad title: never derive a title from message content, tool arguments, output,
  raw paths, or authentication data.

### 6. Tests Required

- Unit tests assert usage precedence, field aliases, raw cache-write fallback,
  cross-object positive cache-write fallback, miss/write separation,
  zero-input handling, saturating uncached input, and
  `total = input + output + cacheWrite`.
- Fixture tests assert `subagents` exclusion, one-record counting, malformed
  line accounting, project basename redaction, and cutoff filtering.
- Fixture tests assert that each dated usage record contributes once to
  `daily` and once to its matching `dailyByModel[model]` series, with no
  cross-model leakage.
- Session fixture tests assert file-scoped title association, `aiTitle` over
  `summary`, title events before and after usage, titles older than the usage
  cutoff, untitled fallback, and equal-title sessions remaining distinct.
- HTTP/Tauri compile tests must continue to pass after the optional `days`
  signature change.
- Frontend/browser checks must assert source tabs, range selection, summary,
  trend, composition, heatmap, rankings, empty/error states, and no horizontal
  overflow at narrow viewport widths.

### 7. Wrong vs Correct

#### Wrong

```rust
// Counts providerData and message usage as two independent records.
total.add(usage(provider_data));
total.add(usage(message));
```

#### Correct

```rust
// Select one canonical usage object, then aggregate it once.
let selected = message_usage.or(provider_usage).or(top_level_usage)?;
total.add(normalize(selected));
```

#### Wrong

```rust
// Cache misses are newly computed input, not cache creation.
write = field(raw_usage, &["prompt_cache_miss_tokens"]).unwrap_or(0);
```

#### Correct

```rust
// Only explicit cache-write aliases may populate cacheWrite.
write = positive_field(raw_usage, CACHE_WRITE_KEYS).unwrap_or(0);
```

#### Wrong

```rust
// Applying cutoff first drops older title-only events, and keying by title
// merges unrelated sessions that happen to share a label.
if timestamp(&event)? < cutoff { continue; }
sessions.entry(event["aiTitle"].to_string()).or_default();
```

#### Correct

```rust
// Collect file metadata before cutoff; only usage is time-filtered. Preserve
// file identity and attach the preferred title to that file's aggregate.
collect_title_metadata(&event);
if usage_is_in_range(&event, cutoff) {
    session_totals.add(normalize_usage(&event)?);
}
```
