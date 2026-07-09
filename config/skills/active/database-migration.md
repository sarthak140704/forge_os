---
name: database-migration
version: 1.0.0
description: Author and apply reversible, zero-downtime schema migrations.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - migration
    - schema
    - alter table
    - ddl
    - flyway
    - alembic
  file_globs:
    - "**/migrations/**/*.sql"
    - "**/migrations/**/*.py"
    - "**/db/migrate/**/*"
---
# Database Migration Playbook

Use this playbook when the mission changes a database schema. Pair it with the
`postgres` (or equivalent) skill for connection safety.

## Principles
- Every migration is **forward + reversible**: write both the `up` and the
  `down`. If a change is genuinely irreversible (dropping a column with data),
  call it out and require approval.
- Migrations are immutable once merged/applied. Never edit an applied
  migration — write a new one.

## Zero-downtime, expand-then-contract
1. **Expand**: add the new column/table/index as nullable or with a default;
   deploy code that writes both old and new.
2. **Backfill**: migrate data in batches to avoid long locks.
3. **Contract**: once all readers use the new shape, drop the old column in a
   *later* migration.
- Add indexes concurrently (`CREATE INDEX CONCURRENTLY` in Postgres) to avoid
  table locks.

## Applying
- Run against a disposable/staging DB first. Verify with a `--dry-run` or by
  inspecting the generated SQL before touching shared data.
- Take a backup (see `postgres` skill) immediately before applying to a
  production-like database.

## Rollback
- Run the migration's `down` step. If the tool tracks a version table
  (Flyway/Alembic/`schema_migrations`), roll back exactly one version and
  verify the schema matches the prior baseline.
