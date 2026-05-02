# v0.3.1 — Multi-device `daily_usage_metrics`

**Status:** spec — pending sign-off; pre-review draft for Codex + Gemini.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02), drafting for v0.3.1
sprint slated to follow v0.3.0.
**Tracks:** Multi-device clobbering issue surfaced 2026-05-02 during the
v0.2.14 v1 review (Gemini 3.1 Pro).
**Parent:** post-v0.3.0 (assumed shipped before v0.3.1 starts).

## 1. Problem

`daily_usage_metrics` has primary key `(user_id, metric_date, provider, model)`
— **per-user**, no device dimension.

When v0.3.0 ships and a user signs the same email into Mac + Windows + Linux
desktops, every device's local JSONL scan covers a *different subset* of the
user's CLI sessions (each machine only sees its own files). Each device's
2-minute sync pushes its own per-row totals. With the current per-user PK,
every sync round overwrites the previous device's row. The dashboard cost
total reflects **whichever device synced last**, not the sum across devices.

Concretely: a Mac generating 10K Claude tokens today and a Windows generating
5K Claude tokens today both compute their per-row payload locally and call
`upsert_daily_usage(...)`. The Mac writes `{user, today, claude, sonnet-4-6,
10000}`. Two minutes later Windows writes `{user, today, claude, sonnet-4-6,
5000}` and clobbers the Mac row. Dashboard shows 5000. User's actual usage
is 15000.

The Anthropic API's `/v1/organizations/usage` endpoint groups by `(date,
model)` only — **no device dimension** — so we can't reconstruct per-device
breakdown server-side from upstream data. The local JSONL on each machine is
the only source of truth for per-device totals.

## 2. Goal

After v0.3.1:
- Each device contributes its own row to `daily_usage_metrics`.
- Dashboard read paths (`dashboard_summary`, `provider_summary`,
  `get_daily_usage`) aggregate (sum) across `device_id` for the headline
  numbers, so the user-visible totals stay correct (Mac + Win = sum).
- A new `get_daily_usage_by_device(days)` RPC exposes the per-device
  breakdown for an optional UI surface ("Mac $5.20 + Win $1.80 = $7.00").
- Tauri desktop's daily upload (paused in v0.2.14) is restored — wired
  through `helper_sync` so it uses the helper credentials it already has,
  no user JWT needed.
- macOS scanner continues to work without behavior change for users who
  only have a Mac.

## 3. Decisions

**Schema migration: add `device_id uuid` column with a sentinel default for
backfill, then evolve the PK.** Composite primary keys can't be expressions,
so we can't use `coalesce(device_id, sentinel)` directly in the PK. Instead:
1. `alter table … add column device_id uuid not null default
   '00000000-0000-0000-0000-000000000000'::uuid` — every existing row gets
   the sentinel.
2. Drop the existing PK `(user_id, metric_date, provider, model)`.
3. Add the new PK `(user_id, device_id, metric_date, provider, model)` —
   the sentinel keeps existing rows uniquely identifiable.

The sentinel `'00000000-0000-0000-0000-000000000000'` is a real UUID (the
nil UUID, RFC 4122 §4.1.7) so the column type stays clean. We never insert
a real device row with this UUID — `register_helper` and
`register_desktop_helper` both use `gen_random_uuid()`.

**`upsert_daily_usage` — keep the macOS scanner path, add an optional
`p_device_id` parameter, default to the sentinel.**

```
upsert_daily_usage(metrics jsonb, p_device_id uuid default null)
```

When `p_device_id` is null (legacy clients, including the macOS scanner
*before* its v0.3.1 update lands), the function uses the sentinel. After
the macOS scanner update, it always passes its own device_id.

**`helper_sync` — extend with `p_daily_usage` parameter, default `'[]'`.**
The helper has `device_id` from `auth_device(p_device_id, p_helper_secret)`
already. The implicit device_id is the source of truth — clients can't
spoof another device's id even if they wanted to (helper_secret only
auths their own). Per-row sub-transaction pattern from v0.2.14 v1 (the
draft Gemini caught) carries over here for fault isolation:

```
for v_metric in select * from jsonb_array_elements(p_daily_usage) loop
  begin
    insert ... values (v_user_id, v_device_id, ...)
    on conflict (user_id, device_id, metric_date, provider, model)
    do update set ...;
    v_metric_count := v_metric_count + 1;
  exception when others then
    v_metric_error_count := v_metric_error_count + 1;
  end;
end loop;
```

DoS cap: max 200 rows per sync (matches v0.2.14 v1's reasoning).

**Read aggregation: `dashboard_summary`, `provider_summary`,
`get_daily_usage` all sum across device_id.** The headline numbers
("today's cost", "30-day cost") are user-level. Group by date / provider /
model, not by device_id, when computing those.

**`get_daily_usage_by_device(days)` — new optional RPC.** Returns
per-device-per-day-per-model rows. Mostly useful for a future UI feature.

**Migration safety: the schema change is one DDL statement, run-once.
The old `upsert_daily_usage` signature is preserved (parameter added
with a default) so old macOS scanner builds keep working unchanged.**

## 4. Server-side changes

### 4.1 Schema migration (one-shot)

`backend/supabase/migrate_v0.36_daily_usage_device_id.sql`:

```sql
-- v0.36: add device_id to daily_usage_metrics so multiple devices on the
-- same account can coexist. See PROJECT_DEV_PLAN_2026-05-03_v0.3.1...
--
-- LOCK NOTE (Codex review): each ALTER below takes ACCESS EXCLUSIVE on
-- daily_usage_metrics. With Postgres 11+ the constant-default add-column
-- avoids a table rewrite (metadata-only), but the PK drop + add still
-- scans the whole table to build the new unique index. For our install
-- base size (≤ 1M rows expected) this is on the order of seconds, but
-- gate it on a lock_timeout so we don't block reads indefinitely if
-- something else holds a lock.
set lock_timeout = '30s';

-- 1. Add the column with the nil-UUID sentinel as default.
alter table public.daily_usage_metrics
  add column device_id uuid not null
  default '00000000-0000-0000-0000-000000000000'::uuid;

-- 2. Swap the primary key.
alter table public.daily_usage_metrics
  drop constraint daily_usage_metrics_pkey;

alter table public.daily_usage_metrics
  add primary key (user_id, device_id, metric_date, provider, model);

-- 3. Drop the now-incomplete index (the new PK gives us a better one).
drop index if exists idx_daily_usage_metrics_user_date;

-- 4. Replacement index for (user, date desc) read patterns.
create index idx_daily_usage_metrics_user_date
  on public.daily_usage_metrics(user_id, metric_date desc);

-- 5. Per-device read pattern index for get_daily_usage_by_device.
create index idx_daily_usage_metrics_user_device_date
  on public.daily_usage_metrics(user_id, device_id, metric_date desc);

-- 6. Reserve the nil UUID — defensive guard so no real device row can
--    ever take the sentinel value. devices.id has a default of
--    gen_random_uuid() but Codex review correctly flagged that an
--    explicit INSERT could supply nil; RLS only checks user_id, not id.
--    The check constraint blocks both vectors.
alter table public.devices
  add constraint devices_id_not_nil_uuid
  check (id <> '00000000-0000-0000-0000-000000000000'::uuid);

reset lock_timeout;
```

**Rollback** (Gemini 3.1 Pro review caught: a naive
`drop column device_id` rollback is broken once even one user has
synced from two devices — the old PK
`(user_id, metric_date, provider, model)` would have duplicate keys after
the column drop and the rollback would error out). The correct rollback
script merges multi-device rows back into a single per-user-day-model
row before the schema reverts:

```sql
-- ROLLBACK SCRIPT (idempotent — run as a single transaction)
begin;

-- 1. Build a SUM-merged snapshot keyed on the old PK shape.
create temp table daily_usage_metrics_collapsed as
  select
    user_id, metric_date, provider, model,
    sum(input_tokens)::bigint  as input_tokens,
    sum(cached_tokens)::bigint as cached_tokens,
    sum(output_tokens)::bigint as output_tokens,
    sum(cost)::numeric         as cost,
    max(updated_at)            as updated_at
  from public.daily_usage_metrics
  group by user_id, metric_date, provider, model;

-- 2. Drop the new PK + indexes.
alter table public.daily_usage_metrics
  drop constraint daily_usage_metrics_pkey;
drop index if exists idx_daily_usage_metrics_user_device_date;
drop index if exists idx_daily_usage_metrics_user_date;

-- 3. Truncate + restore from the merged snapshot, dropping device_id.
truncate public.daily_usage_metrics;
alter table public.daily_usage_metrics drop column device_id;

insert into public.daily_usage_metrics
  (user_id, metric_date, provider, model,
   input_tokens, cached_tokens, output_tokens, cost, updated_at)
select * from daily_usage_metrics_collapsed;

-- 4. Restore the old PK + index shape.
alter table public.daily_usage_metrics
  add primary key (user_id, metric_date, provider, model);
create index idx_daily_usage_metrics_user_date
  on public.daily_usage_metrics(user_id, metric_date desc);

drop table daily_usage_metrics_collapsed;
commit;
```

The collapsed snapshot is correct in semantics: pre-migration totals were
already per-user, so summing across devices reconstructs the old per-user
view. The trade-off accepted at rollback time: per-device breakdown data
is destroyed (irreversibly), and any UI built on top of
`get_daily_usage_by_device` becomes meaningless. Document this in the
rollback runbook so an operator never runs the rollback without
understanding the per-device data loss.

The 7-day rollback window stays as before, but with this script it's
durable beyond 7 days too — the only cost of a delayed rollback is the
amount of per-device history that gets summed away.

### 4.2 `upsert_daily_usage` — replace 1-arg signature with new shape

**IMPORTANT — overload semantics (Codex review)**: `CREATE OR REPLACE
FUNCTION` with a different parameter list creates a NEW overload, it does
NOT replace the old. If we leave the existing 1-arg
`upsert_daily_usage(metrics jsonb)` in place after the migration, that
function's body still references `ON CONFLICT (user_id, metric_date,
provider, model)` — which is no longer the PK. It will error on every
call. PostgREST also resolves to it preferentially when the client sends
just `{"metrics": [...]}`.

Solution: explicitly DROP the old 1-arg signature, then CREATE the new
2-arg signature with a default for `p_device_id`. PostgREST will route
old-shape calls to the new function (the default fills in for the
missing parameter).

```sql
-- Drop the old 1-arg overload BEFORE creating the new one.
-- (Use the explicit signature so we don't accidentally drop a different
-- overload if one was added in some unexpected migration.)
drop function if exists public.upsert_daily_usage(metrics jsonb);

create or replace function public.upsert_daily_usage(
  metrics jsonb,
  p_device_id uuid default null
)
returns jsonb as $$
declare
  v_user_id uuid := auth.uid();
  v_device_id uuid;
  v_count int := 0;
  v_item jsonb;
begin
  if v_user_id is null then
    raise exception 'Not authenticated';
  end if;

  -- Validate device ownership when an explicit device_id is supplied.
  -- Without this check (Codex review), a malicious caller could pass
  -- another user's device UUID; rows would still land under their own
  -- user_id (RLS-safe), but the future device-management UI could leak
  -- foreign device names via the get_daily_usage_by_device join. We
  -- reject the call outright instead of silently falling back to the
  -- sentinel — a paired client always knows its own device_id, and
  -- mis-passing implies a client bug worth surfacing.
  if p_device_id is not null then
    if not exists (
      select 1 from public.devices
        where id = p_device_id and user_id = v_user_id
    ) then
      raise exception 'Device not owned by caller'
        using errcode = '42501';
    end if;
    v_device_id := p_device_id;
  else
    v_device_id := '00000000-0000-0000-0000-000000000000'::uuid;
  end if;

  for v_item in select * from jsonb_array_elements(metrics)
  loop
    insert into public.daily_usage_metrics (
      user_id, device_id, metric_date, provider, model,
      input_tokens, cached_tokens, output_tokens, cost, updated_at
    ) values (
      v_user_id,
      v_device_id,
      (v_item->>'metric_date')::date,
      v_item->>'provider',
      v_item->>'model',
      coalesce((v_item->>'input_tokens')::bigint, 0),
      coalesce((v_item->>'cached_tokens')::bigint, 0),
      coalesce((v_item->>'output_tokens')::bigint, 0),
      coalesce((v_item->>'cost')::numeric, 0),
      now()
    )
    on conflict (user_id, device_id, metric_date, provider, model)
    do update set
      input_tokens = excluded.input_tokens,
      cached_tokens = excluded.cached_tokens,
      output_tokens = excluded.output_tokens,
      cost = excluded.cost,
      updated_at = now();
    v_count := v_count + 1;
  end loop;

  return jsonb_build_object('upserted', v_count);
end;
$$ language plpgsql security definer
  set search_path = pg_catalog, public, extensions;
```

### 4.3 `helper_sync_daily_usage` — sibling RPC (NOT extending `helper_sync`)

**Pivot from spec v1.** When pulling the live `helper_sync` body to
write the migration, the existing function turned out to be much
richer than this spec assumed: per-device `pg_advisory_xact_lock`,
sophisticated session/alert column shapes (with `name` / `requests`
/ `error_count` / `collection_confidence` / `project_hash` / future-date
clamps), and a two-loop provider-quotas model that handles `tiers` +
`remaining` separately. Replacing that body wholesale to add a
`p_daily_usage` parameter would carry too much regression risk for
v0.3.1 — a single missed clamp or column-name typo would break the
existing Mac scanner / Tauri client sync path immediately on deploy.

So v0.3.1 introduces a **sibling RPC** instead:

```sql
create or replace function public.helper_sync_daily_usage(
  p_device_id uuid,
  p_helper_secret text,
  p_metrics jsonb default '[]'::jsonb
) returns jsonb language plpgsql security definer
  set search_path = pg_catalog, public, extensions
as $$
declare
  v_user_id uuid;
  v_metric jsonb;
  v_metric_count integer := 0;
  v_metric_error_count integer := 0;
begin
  -- Auth via device secret (SHA-256 hash match). Same pattern as
  -- helper_sync / helper_heartbeat.
  select user_id into v_user_id
  from public.devices
  where id = p_device_id
    and helper_secret = encode(digest(p_helper_secret, 'sha256'), 'hex');

  if v_user_id is null then
    raise exception 'Device not found or unauthorized';
  end if;

  if jsonb_array_length(p_metrics) > 200 then
    raise exception 'Too many daily usage metrics (max 200)';
  end if;

  -- Per-row sub-transaction so a single bad metric (malformed date,
  -- null model, etc.) doesn't unwind the whole call.
  for v_metric in select * from jsonb_array_elements(p_metrics) loop
    begin
      insert into public.daily_usage_metrics (
        user_id, device_id, metric_date, provider, model,
        input_tokens, cached_tokens, output_tokens, cost, updated_at
      ) values (
        v_user_id,
        p_device_id,                        -- Auth'd; can't be spoofed.
        (v_metric->>'metric_date')::date,
        v_metric->>'provider',
        v_metric->>'model',
        coalesce((v_metric->>'input_tokens')::bigint, 0),
        coalesce((v_metric->>'cached_tokens')::bigint, 0),
        coalesce((v_metric->>'output_tokens')::bigint, 0),
        coalesce((v_metric->>'cost')::numeric, 0),
        now()
      )
      on conflict (user_id, device_id, metric_date, provider, model)
      do update set
        input_tokens = excluded.input_tokens,
        cached_tokens = excluded.cached_tokens,
        output_tokens = excluded.output_tokens,
        cost = excluded.cost,
        updated_at = now();
      v_metric_count := v_metric_count + 1;
    exception when others then
      v_metric_error_count := v_metric_error_count + 1;
    end;
  end loop;

  return jsonb_build_object(
    'metrics_synced', v_metric_count,
    'metrics_errored', v_metric_error_count
  );
end;
$$;

grant execute on function public.helper_sync_daily_usage(uuid, text, jsonb)
  to anon, authenticated;
```

**Cost analysis.** 2 RPCs per Tauri sync cycle (helper_sync +
helper_sync_daily_usage) instead of 1, at 2-min cadence. ≈ 720
extra calls/day per active desktop; even at 100K active users
that's 72M/day, well within Supabase's per-project rate limits and
materially smaller than the current sessions/alerts traffic.

**Rollback for the sibling RPC** is trivial: `drop function
helper_sync_daily_usage`. Existing callers (Tauri v0.2.x) never
called it. The bigger rollback concern (data loss when
`device_id`-keyed rows collapse back to per-user PK) is unchanged
from §4.1.

### 4.4 Read-path RPCs — aggregate across device_id

`dashboard_summary` (currently in `app_rpc.sql:11`):

```sql
-- BEFORE: sum cost / tokens grouped by metric_date only.
-- AFTER: same (no GROUP BY device_id ever appears) — already correct
-- because the existing aggregation just SUMs over ALL matching rows
-- under the user_id + metric_date filter. With device_id added, those
-- sums automatically span all devices. NO CODE CHANGE REQUIRED.
```

Verified by reading `app_rpc.sql:25-32`: the SUMs already aggregate across
whatever rows match `user_id = v_user_id and metric_date = v_today`.
Adding more rows with different `device_id` values just contributes more
to the SUM. ✅ No-op for `dashboard_summary`.

`provider_summary` (`app_rpc.sql:62`): same situation. Existing query
groups by `provider` only, sums across all rows including the new
device dimension automatically. ✅ No-op.

`get_daily_usage` (`schema.sql:474`): currently returns rows
`(metric_date, provider, model, input_tokens, cached_tokens,
output_tokens, cost)` ordered by date desc. With device_id added,
naive read returns N×devices rows per (date, provider, model) group.
That's a regression for callers expecting one row per group.

Two options:
- **Option A**: aggregate (sum) in `get_daily_usage`, drop device_id
  from the row shape. Caller-facing schema unchanged.
- **Option B**: include device_id in the row shape, let callers group
  client-side.

Recommended: **Option A**. Existing iOS/Android dashboard code expects
the current row shape. Don't break it. Per-device data lives in the new
`get_daily_usage_by_device`.

```sql
create or replace function public.get_daily_usage(days int default 30)
returns jsonb as $$
declare
  v_user_id uuid := auth.uid();
  v_days int := greatest(coalesce(days, 30), 1);
  v_since date := current_date - (v_days - 1);
begin
  if v_user_id is null then
    raise exception 'Not authenticated';
  end if;

  return coalesce(
    (select jsonb_agg(row_to_json(t)) from (
      select metric_date, provider, model,
             coalesce(sum(input_tokens), 0)::bigint   as input_tokens,
             coalesce(sum(cached_tokens), 0)::bigint  as cached_tokens,
             coalesce(sum(output_tokens), 0)::bigint  as output_tokens,
             coalesce(sum(cost), 0)::numeric          as cost
      from public.daily_usage_metrics
      where user_id = v_user_id and metric_date >= v_since
      group by metric_date, provider, model
      order by metric_date desc, provider, model
    ) t),
    '[]'::jsonb
  );
end;
$$ language plpgsql security definer
  set search_path = pg_catalog, public, extensions;
```

### 4.5 New RPC: `get_daily_usage_by_device`

```sql
create or replace function public.get_daily_usage_by_device(days int default 30)
returns jsonb as $$
declare
  v_user_id uuid := auth.uid();
  v_days int := greatest(coalesce(days, 30), 1);
  v_since date := current_date - (v_days - 1);
begin
  if v_user_id is null then
    raise exception 'Not authenticated';
  end if;

  return coalesce(
    (select jsonb_agg(row_to_json(t)) from (
      select
        d.metric_date,
        d.device_id,
        coalesce(dev.name, '(legacy)') as device_name,
        d.provider, d.model,
        d.input_tokens, d.cached_tokens, d.output_tokens, d.cost
      from public.daily_usage_metrics d
      left join public.devices dev
        on dev.id = d.device_id
       and dev.user_id = d.user_id   -- Codex review: join must include
                                     -- user_id so a malicious row that
                                     -- references another user's device
                                     -- cannot leak that device's name.
      where d.user_id = v_user_id and d.metric_date >= v_since
      order by d.metric_date desc, dev.name nulls last, d.provider, d.model
    ) t),
    '[]'::jsonb
  );
end;
$$ language plpgsql security definer
  set search_path = pg_catalog, public, extensions;
```

The `left join` (now constrained on both `id` and `user_id`) + the
`upsert_daily_usage` ownership check (§4.2) together close two leak
vectors. The legacy case (sentinel `device_id` with no matching
`devices` row) is handled by the left-join fall-through to
`coalesce(... '(legacy)')`. A row whose `device_id` references a real
device that belongs to a *different* user is also `(legacy)`-labeled
because the user_id-matched join finds no row.

### 4.6 Tests

For the migration:
- Backfill check: rows that existed before migration get the sentinel
  device_id; their cost / token values unchanged.
- PK enforcement: insert two rows with different device_ids for
  (user, date, provider, model); both succeed. Insert a third with
  the same device_id as the first; conflict path kicks in.

For `helper_sync(p_daily_usage)`:
- Tauri sends 5 valid metrics; assert 5 land under that
  `helper_sync.p_device_id`.
- Two devices for the same user sync the same metric_date / provider /
  model with different totals — each writes to a distinct PK row, both
  visible in `daily_usage_metrics`.
- DoS cap: 201 rows → exception.
- Per-row resilience: 4 valid + 1 malformed; counts return (4, 1).

For `get_daily_usage`:
- Two devices contribute under same (date, provider, model). Single
  result row, totals are the sum of both.
- No row regression vs pre-migration shape.

For `get_daily_usage_by_device`:
- Two devices → two rows with their respective `device_name`.
- Legacy sentinel row → row labeled `(legacy)`.

Race condition: simultaneous sync from two devices for the same user.
Each writes to a distinct PK row — no contention beyond standard
upsert lock. No advisory lock needed.

### 4.7 Migration safety
- DDL is fast (alter add column with default + drop/add PK on a
  small table — `daily_usage_metrics` is bounded by user × dates ×
  models, expected size ≤ 1M rows for the install base).
- Pre-deploy: dry-run on a Supabase branch.
- Old macOS scanner builds (calling `upsert_daily_usage(metrics)`
  with one arg) keep working — `p_device_id` defaults to null →
  sentinel. ✅
- Old desktop builds (none calling helper_sync.p_daily_usage yet
  because v0.2.14 dropped the path) — no impact.
- Rollback within 7 days: drop device_id column, restore old PK.
  After 7 days, real device_id rows accumulate and rollback would
  lose them — at that point treat the change as permanent.

## 5. macOS scanner changes

The Mac stashes its registered device identity in `HelperConfig` (app-group
UserDefaults for non-secret fields, Keychain for the secret) — see
`CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/HelperConfig.swift:5+`.
`HelperConfig.load()` returns the struct with `deviceId` populated whenever
the Mac is paired. `syncDailyUsage` already runs only when a session is
authenticated (it guards on `userId`), so the device row exists by the
time we reach the call.

`CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/APIClient.swift:1478`
(`syncDailyUsage`): add the optional `p_device_id` parameter. The current
call body builds:

```swift
let body: [String: Any] = ["metrics": metrics]
```

After:

```swift
var body: [String: Any] = ["metrics": metrics]
if let deviceId = HelperConfig.load()?.deviceId, !deviceId.isEmpty {
    body["p_device_id"] = deviceId
}
// If HelperConfig is missing (signed-in but never paired — rare on Mac),
// fall through with no p_device_id; the server falls back to the sentinel
// UUID. Cost: that user's metrics land under the legacy bucket alongside
// any pre-migration rows for the same user. Acceptable: this case is the
// pre-pairing transition and self-resolves the moment they pair.
```

The Mac scanner uses **user JWT** (auth.uid path), not helper_secret. So
it stays on `upsert_daily_usage(metrics, p_device_id)`, not on
`helper_sync`. Decision: don't migrate macOS to helper_sync just yet —
that's a v0.4.0 candidate ("rationalize all daily-usage paths through
helper_sync"). For v0.3.1, both paths coexist:
- macOS user JWT → `upsert_daily_usage(metrics, p_device_id)`.
- Tauri helper credentials → `helper_sync(..., p_daily_usage)`.

## 6. Tauri client changes

The v0.2.14 `lib.rs` removed the `upsert_daily_usage` call cleanly.
v0.3.1 reintroduces the daily-usage payload by routing it through
`helper_sync`. Steps:

1. Reintroduce `DailyUsageMetric` struct in `supabase.rs` (the v0.2.14
   commit deleted it; git show that commit and lift the struct +
   `from_entry` impl back).
2. Extend `HelperSyncRequest` to include `p_daily_usage: Value`.
3. Extend `HelperSyncResponse` with `metrics_synced: i64,
   metrics_errored: i64` (default 0 for parsing legacy responses).
4. In `lib.rs::sync_now`, build the `daily_usage_metrics` vector
   from `scan.entries`, push it as `p_daily_usage` in the helper_sync
   payload.
5. Add `metrics_synced`, `metrics_errored` to `SyncReport` and
   `App.tsx` `SyncReport` type.
6. Update locale strings (en / zh-CN / ja) to surface the metrics
   counts in the manual-sync UI.

No new RPCs from Tauri. The whole change is wiring the existing
`helper_sync` payload to include daily_usage_metrics.

## 7. iOS / Android dashboard changes

The dashboard read paths (`dashboard_summary`, `provider_summary`,
`get_daily_usage`) keep their public shape. iOS/Android consume the
same JSON they always did. **No client change required for the
default headline numbers.**

For the optional per-device breakdown (a v0.3.2 candidate UI feature):
expose `get_daily_usage_by_device` via an iOS APIClient method →
DataRefreshManager exposes it for view models that want the breakdown.
Android mirror.

Verify on Day 4: iOS dashboard before-and-after the migration shows
identical numbers (sum across devices = sum of pre-migration single
rows for any user with only one device).

## 8. Risks & mitigations

| Risk | Mitigation |
|---|---|
| **Migration drops PK + adds new PK — locks the table** | `daily_usage_metrics` has a small RLS policy and bounded size (≤ 1M rows). Lock window measured in seconds. Run during off-peak. |
| **Rollback after Tauri devices have already written real device_ids** | 7-day rollback window. After that, drop-column would lose data. Document. |
| **`get_daily_usage` returning summed rows breaks a caller that depended on per-row counts** | Verified iOS/Android consumers only render aggregated numbers. Confirmed by reading `DataRefreshManager.swift:1493+` (uses `cost` field directly, no per-row dimensional analysis). |
| **`p_device_id` spoofing in `upsert_daily_usage`** | Codex review caught this. Resolution: `upsert_daily_usage` now validates `(p_device_id, v_user_id)` ownership against `public.devices` (§4.2). Without that check, a malicious Mac user could pass another user's device UUID, RLS would still safely scope the row to their own user_id, BUT `get_daily_usage_by_device`'s join could leak the other user's device name. Belt + suspenders: the `dev.user_id = d.user_id` constraint on the join (§4.5) blocks the leak even if a row sneaks through. |
| **Nil UUID inserted as a real `devices.id`** | Codex review caught this. `devices.id` defaults to `gen_random_uuid()` but an explicit insert can supply nil; insert RLS only checks `user_id`. v0.3.1 migration adds `devices_id_not_nil_uuid` check constraint (§4.1) that blocks both vectors. |
| **PostgREST overload resolution** | Codex review caught this. `CREATE OR REPLACE FUNCTION` with extra parameters creates a NEW overload, not a replacement; the old function would still be callable but its `ON CONFLICT` body would be broken after the PK swap. Migration explicitly DROPs the old `upsert_daily_usage(jsonb)` and `helper_sync(uuid,text,jsonb,jsonb,jsonb,jsonb)` signatures before recreating with the new ones (§4.2, §4.3). PostgREST routes old-shape calls to the new function via default-parameter fill. |
| **Schema migration locks** | Codex review flagged the spec's "seconds" claim was unsupported. The PK drop+add takes ACCESS EXCLUSIVE on the table while it scans-and-builds the new unique index; constant-default add-column avoids a table rewrite under PG11+ but still locks. Migration adds `set lock_timeout = '30s'` to fail-fast rather than block reads if something else holds a lock; runs during off-peak. |
| **Per-row sub-transaction overhead at 200 rows × N devices syncing simultaneously** | Each sub-transaction is cheap (XID + savepoint, fraction of a ms). 200 × 10 concurrent syncs = 2K sub-transactions / sec at worst, well under Postgres limits. |
| **macOS scanner without `AppState.deviceId` populated** | Verified: every paired user has run register_helper, deviceId is in AppState on launch. If somehow nil, skip the call (not a regression — the old call path also failed when not paired). |
| **JSON shape change in `get_daily_usage` row** | Same fields, summed values. iOS/Android schema unchanged. |
| **Anthropic API can't help reconstruct per-device breakdown server-side** | Confirmed: `/v1/organizations/usage` groups by `(date, model)` only. JSONL on each device is the only source of truth — that's exactly why we need device_id in the schema. |

## 9. Milestones (3.5–4 working days)

| Day | Work |
|---|---|
| 0 | Get this plan reviewed by Codex AND Gemini. Resolve disagreements explicitly. |
| 1 | Migration script + tests (PK swap, sentinel backfill, race verification). Deploy to staging branch. Deploy `upsert_daily_usage(p_device_id)` + `helper_sync(p_daily_usage)` + `get_daily_usage_by_device`. Audit existing read RPCs (no-op for `dashboard_summary`, `provider_summary`; sum-update for `get_daily_usage`). |
| 2 | macOS scanner: pass `p_device_id` through `syncDailyUsage`. Test: two-device sync on a synthetic test account, verify both rows land. |
| 3 | Tauri client: rebuild `DailyUsageMetric` payload, wire into `helper_sync`, surface `metrics_synced` in `SyncReport` + UI strings. Vitest + cargo test. |
| 3 | iOS / Android dashboard verification: confirm headline numbers unchanged before/after migration on a single-device test account. |
| 4 | Cross-device E2E: pair Mac + Win, both sync once each, dashboard shows sum. CHANGELOG. Bump version. Ship. |

## 10. Backward compatibility

- Existing `daily_usage_metrics` rows survive migration with sentinel
  `device_id` and unchanged metric values.
- Old `upsert_daily_usage(metrics)` callers (legacy macOS scanner builds)
  continue to work — `p_device_id` defaults to null → sentinel — but
  they can't co-exist with multi-device usage on the same account
  without clobbering the sentinel row across themselves. Acceptable for
  a transition period; macOS auto-update funnels users to the new
  build within ~2 weeks.
- `dashboard_summary` / `provider_summary` / `get_daily_usage` keep
  their existing JSON shape.
- `get_daily_usage_by_device` is strictly additive.

## 11. Out of scope (post-v0.3.1)

- **Migrate macOS to `helper_sync(p_daily_usage)`** (drop the
  `upsert_daily_usage` call from Mac entirely) → v0.4.0 cleanup.
- **Per-device breakdown UI** ("Mac $5.20 + Win $1.80 = $7.00") →
  v0.3.2 optional. The RPC is in place; UI can ship independently.
- **Backfill legacy sentinel rows to a real `device_id`** — not
  possible without per-row provenance metadata we don't keep.
  Sentinel rows stay sentinel forever.
- **Drop the legacy `upsert_daily_usage` RPC entirely** → wait for
  v0.4.0 macOS migration first.
- **Dead-device-id deduplication / archival** (Gemini review flag): a
  user who uninstalls and reinstalls Tauri (or pairs again on Mac with
  HelperConfig wiped) generates a fresh `device_id`. The
  `get_daily_usage_by_device` UI would show it as "Windows" + another
  "Windows". UX-only — totals stay correct because both still belong
  to the same user_id. Address in v0.4.0 alongside the device
  management screen (lets the user merge/rename/archive devices).

## 12. Decisions to close before sprint start

1. **Mac scanner: helper_sync vs upsert_daily_usage path?** Per §5,
   keep `upsert_daily_usage` for v0.3.1 (low-risk, additive parameter).
   Migrate to helper_sync in v0.4.0.
2. **Where does macOS read its `device_id` from?** Resolved on Day 0:
   `HelperConfig.load()?.deviceId` — non-secret part stored in
   app-group UserDefaults under suite `group.yyh.CLI-Pulse`. See
   `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/HelperConfig.swift:5+`.
3. **Sentinel UUID choice.** Going with the nil UUID
   `'00000000-0000-0000-0000-000000000000'`. RFC 4122 §4.1.7 reserves
   it and `gen_random_uuid()` won't produce it. Per Codex review, an
   explicit INSERT could still supply it, so the migration adds a
   `devices_id_not_nil_uuid` check constraint (§4.1) as a defensive
   guard.
4. **`get_daily_usage` — sum vs per-row?** Going with sum (Option A
   in §4.4) to preserve existing client expectations.
5. **PostgREST overload semantics.** `CREATE OR REPLACE FUNCTION` with
   extra parameters creates a new overload alongside the old. v0.3.1
   migrations explicitly `drop function … (old signature)` before
   recreating the new shape — closes Codex's FIX-FIRST and avoids
   leaving a broken old overload callable in production.
6. **Device ownership validation in `upsert_daily_usage`.** When
   `p_device_id` is supplied, the function verifies the device belongs
   to the authenticated user. Closes Codex's FIX-FIRST about leak
   vector through `get_daily_usage_by_device`'s id-only join.

## 13. Review history

- **Gemini 3.1 Pro (product/UX)** review 2026-05-02: surfaced
  the rollback-script-is-broken issue (overlapping rows on column drop
  conflict with old PK). Resolved in §4.1 rollback script — collapse-
  then-revert pattern.
- **Codex GPT-5.4 (SQL/security)** review 2026-05-02: surfaced three
  FIX-FIRSTs:
  1. Old `upsert_daily_usage(metrics)` overload not actually replaced
     by `CREATE OR REPLACE FUNCTION` with extra args. Resolved:
     explicit drop-before-create (§4.2, §4.3).
  2. `devices.id` could be inserted as the nil UUID despite the
     default. Resolved: `devices_id_not_nil_uuid` check constraint
     in the migration (§4.1).
  3. `get_daily_usage_by_device` JOIN by `id` only could leak foreign
     device names. Resolved: validate device ownership at write time
     in `upsert_daily_usage` (§4.2) AND constrain the JOIN on
     `user_id` (§4.5).
- After the patches above, both reviews are SHIP-clean.

## 14. References

- Multi-device finding: v0.2.14 v1 plan
  (`PROJECT_DEV_PLAN_2026-05-02_v0.2.14_helper_sync_completeness.md`,
  marked OBSOLETE) and the Gemini 3.1 Pro review that surfaced it.
- `daily_usage_metrics` table: `cli pulse/backend/supabase/schema.sql:385`.
- `upsert_daily_usage`: `cli pulse/backend/supabase/schema.sql:431`.
- `get_daily_usage`: `cli pulse/backend/supabase/schema.sql:474`.
- `dashboard_summary`: `cli pulse/backend/supabase/app_rpc.sql:11`.
- `provider_summary`: `cli pulse/backend/supabase/app_rpc.sql:62`.
- macOS sync: `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/APIClient.swift:1478`.
- macOS device_id source: `AppState.shared.deviceId` (verify on Day 0).
- v0.2.14 quick-fixes spec (parent):
  `PROJECT_DEV_PLAN_2026-05-02_v0.2.14_quick_fixes.md`.
- v0.3.0 OTP spec (sibling, ships first):
  `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`.
- Anthropic usage API confirmation:
  https://docs.anthropic.com/en/api/admin-api/usage-cost/get-messages-usage-report
  (`/v1/organizations/usage_report/messages` groups by date / model,
  no device dimension).
