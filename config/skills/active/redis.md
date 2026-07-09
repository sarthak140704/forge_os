---
name: redis
version: 1.0.0
description: Inspect and operate a Redis instance with redis-cli, non-destructively.
tools:
  - fs.read
  - shell.run
triggers:
  keywords:
    - redis
    - cache
    - key-value
    - redis-cli
    - pubsub
---
# Redis Playbook

Use this playbook whenever the mission inspects or operates a Redis instance.

## Preflight
1. `redis-cli -h <host> -p <port> ping` should return `PONG`.
2. `redis-cli info server` and `info keyspace` to see version and db sizes.
3. Confirm you are on the intended instance — a shared cache in production must
   not be flushed or scanned aggressively without approval.

## Inspecting keys safely
- **Never** run `KEYS *` on a production instance — it blocks the event loop.
  Use `SCAN 0 MATCH <pattern> COUNT 100` and iterate the cursor instead.
- `TYPE <key>`, `TTL <key>`, and the type-appropriate read (`GET`, `HGETALL`,
  `LRANGE`, `SMEMBERS`, `ZRANGE`) to inspect values.

## Mutations
- Prefer setting an explicit TTL on new keys: `SET k v EX <seconds>`.
- Scope deletes precisely with `SCAN` + `DEL`; never `FLUSHALL`/`FLUSHDB` on a
  shared instance.

## Monitoring
- `redis-cli --stat` for live throughput; `MONITOR` only briefly for debugging
  (it has a real performance cost).

## Rollback
- Redis has no transaction undo after `EXEC`. Recovery is restoring from an RDB
  snapshot / AOF, so take or confirm a backup before bulk changes.
