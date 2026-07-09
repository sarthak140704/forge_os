---
name: postgres
version: 1.0.0
description: Query, inspect, and safely mutate a PostgreSQL database with psql.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - postgres
    - postgresql
    - psql
    - sql
    - database
    - query
  file_globs:
    - "**/*.sql"
---
# PostgreSQL Playbook

Use this playbook whenever the mission reads or changes a Postgres database.

## Preflight
1. Confirm the connection target (host/db/user) via `\conninfo` in `psql` or the
   `PG*`/`DATABASE_URL` env. **Never** run mutations against a production DSN
   without explicit mission approval.
2. Inspect schema before writing: `\dt`, `\d <table>`, and
   `SELECT count(*) FROM <table>` to understand size and shape.

## Querying
- Run read-only exploration inside an explicit transaction you can abandon:
  `BEGIN; ... ROLLBACK;` while iterating.
- Always add a `LIMIT` to exploratory `SELECT`s on large tables.

## Mutating safely
- Wrap writes in a transaction: `BEGIN; UPDATE ...; -- verify row count
  SELECT ...; COMMIT;`. Verify the affected-row count matches expectations
  **before** `COMMIT`.
- Never issue `DELETE`/`UPDATE` without a `WHERE` clause. Never `DROP TABLE`
  or `TRUNCATE` on shared data without approval and a backup.

## Backups
- Snapshot before risky changes: `pg_dump <db> > backup.sql` (or a targeted
  `--table` dump). Confirm the file is non-empty.

## Rollback
- Uncommitted transaction: `ROLLBACK;`.
- Committed mistake: restore from the `pg_dump` taken in Preflight, or use a
  point-in-time restore if the platform supports it.
