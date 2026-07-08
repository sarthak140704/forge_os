"""
forge-inspect.py — quick backend inspector for Forge OS acceptance tests.

Usage:
    python scripts/forge-inspect.py <command> [args]

Commands (see docs/TEST_SUITE.md for full context):

    events                    Show last 30 events, most recent first.
    events-for <mission_id>   Show every event for one mission (chronological).
    events-type <type>        Show last 20 events of a given type
                              (e.g. skill_promoted, llm_responded, replan_requested).
    missions                  List missions with status + goal counts.
    mission <mission_id>      Full detail: mission + goals + tasks + reflections.
    skills                    Every active skill (one per row).
    skills-history [name]     History for one skill, or all if omitted.
    reflections [mission_id]  Reflections (all or by mission).
    counts                    One-line count of each table.
    tail <n>                  Follow the last <n> events (poll every 2 s).
    query "<SQL>"             Escape hatch — run any read-only SQL.

The DB path is read from --db, else the env FORGE_DB, else the default
Windows Tauri path: %APPDATA%\\com.sarthak.forgeos\\forge.sqlite
"""
from __future__ import annotations
import argparse
import json
import os
import sqlite3
import sys
import time
from pathlib import Path

DEFAULT_DB = Path(os.environ.get("APPDATA", "")) / "com.sarthak.forgeos" / "forge.sqlite"


def connect(db: Path) -> sqlite3.Connection:
    if not db.exists():
        sys.exit(f"[ERR] DB not found at {db}. Boot the app once, or pass --db.")
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    return con


def _fmt_event(row: sqlite3.Row) -> str:
    try:
        payload = json.loads(row["payload"])
    except Exception:
        payload = row["payload"]
    return f"[{row['seq']:>5}] {row['created_at']}  {row['event_type']:<32}  agg={row['aggregate_id']}\n         {json.dumps(payload, ensure_ascii=False)[:180]}"


def cmd_events(con, limit=30):
    for row in con.execute("SELECT * FROM events ORDER BY seq DESC LIMIT ?", (limit,)).fetchall():
        print(_fmt_event(row))


def cmd_events_for(con, agg_id):
    rows = con.execute(
        "SELECT * FROM events WHERE aggregate_id = ? ORDER BY seq ASC", (agg_id,)
    ).fetchall()
    if not rows:
        # try mission_id lookup via events whose payload references it
        rows = con.execute(
            "SELECT * FROM events WHERE payload LIKE ? ORDER BY seq ASC", (f"%{agg_id}%",)
        ).fetchall()
    for row in rows:
        print(_fmt_event(row))
    print(f"\n[{len(rows)} events]")


def cmd_events_type(con, ev_type, limit=20):
    for row in con.execute(
        "SELECT * FROM events WHERE event_type = ? ORDER BY seq DESC LIMIT ?",
        (ev_type, limit),
    ).fetchall():
        print(_fmt_event(row))


def cmd_missions(con):
    rows = con.execute(
        """
        SELECT m.id, m.title, m.status, m.created_at,
               (SELECT COUNT(*) FROM goals g WHERE g.mission_id = m.id) AS goals,
               (SELECT COUNT(*) FROM goals g WHERE g.mission_id = m.id AND g.status='completed') AS done
        FROM missions m ORDER BY m.created_at DESC
        """
    ).fetchall()
    print(f"{'STATUS':<11} {'GOALS':<9} {'ID':<40} TITLE")
    for r in rows:
        print(f"{r['status']:<11} {r['done']}/{r['goals']:<7} {r['id']:<40} {r['title']}")


def cmd_mission(con, mid):
    m = con.execute("SELECT * FROM missions WHERE id = ?", (mid,)).fetchone()
    if not m:
        # allow prefix
        m = con.execute("SELECT * FROM missions WHERE id LIKE ?", (f"{mid}%",)).fetchone()
    if not m:
        sys.exit(f"[ERR] no mission with id or prefix {mid}")
    print(f"=== mission {m['id']} ===")
    print(f"  title:  {m['title']}")
    print(f"  status: {m['status']}   created: {m['created_at']}   updated: {m['updated_at']}")
    print(f"  desc:   {m['description'][:200]}")

    goals = con.execute(
        "SELECT * FROM goals WHERE mission_id = ? ORDER BY rowid", (m["id"],)
    ).fetchall()
    print(f"\n--- {len(goals)} goals ---")
    for g in goals:
        deps = json.loads(g["depends_on_json"])
        print(f"  [{g['status']:<10}] {g['id']}  {g['title']}  (deps={len(deps)})")

    tasks = con.execute(
        """SELECT t.* FROM tasks t JOIN goals g ON g.id = t.goal_id
           WHERE g.mission_id = ? ORDER BY t.rowid""",
        (m["id"],),
    ).fetchall()
    print(f"\n--- {len(tasks)} tasks ---")
    for t in tasks:
        print(f"  [{t['status']:<15}] tool={t['tool']:<22} attempts={t['attempts']} id={t['id']}")

    refls = con.execute(
        "SELECT * FROM reflections WHERE mission_id = ? ORDER BY created_at", (m["id"],)
    ).fetchall()
    print(f"\n--- {len(refls)} reflections ---")
    for r in refls:
        print(f"  {r['created_at']}  outcome={r['outcome']}")

    evs = con.execute(
        """SELECT DISTINCT event_type, COUNT(*) as n FROM events
           WHERE aggregate_id = ? OR payload LIKE ? GROUP BY event_type ORDER BY n DESC""",
        (m["id"], f"%{m['id']}%"),
    ).fetchall()
    print(f"\n--- event summary ---")
    for e in evs:
        print(f"  {e['n']:>4}  {e['event_type']}")


def cmd_skills(con):
    rows = con.execute(
        """SELECT * FROM skills_history WHERE retired_at IS NULL
           ORDER BY promoted_at DESC"""
    ).fetchall()
    print(f"{'ORIGIN':<12} {'VERSION':<9} {'PROMOTED_AT':<27} {'SHA[0:12]':<14} NAME")
    for r in rows:
        print(f"{r['origin']:<12} {r['version']:<9} {r['promoted_at']:<27} {r['sha'][:12]:<14} {r['name']}")
    print(f"\n[{len(rows)} active skills]")


def cmd_skills_history(con, name=None):
    if name:
        rows = con.execute(
            "SELECT * FROM skills_history WHERE name = ? ORDER BY id DESC", (name,)
        ).fetchall()
    else:
        rows = con.execute("SELECT * FROM skills_history ORDER BY id DESC").fetchall()
    print(f"{'ID':<5} {'NAME':<20} {'V':<7} {'ORIGIN':<12} {'RETIRED':<8} SHA[0:12]  parent[0:12]")
    for r in rows:
        retired = "yes" if r["retired_at"] else "no"
        parent = (r["parent_sha"] or "")[:12] or "-"
        print(f"{r['id']:<5} {r['name'][:20]:<20} {r['version']:<7} {r['origin']:<12} "
              f"{retired:<8} {r['sha'][:12]}  {parent}")


def cmd_reflections(con, mid=None):
    if mid:
        rows = con.execute(
            "SELECT * FROM reflections WHERE mission_id = ? ORDER BY created_at DESC", (mid,)
        ).fetchall()
    else:
        rows = con.execute("SELECT * FROM reflections ORDER BY created_at DESC").fetchall()
    for r in rows:
        print(f"=== {r['mission_id']}  outcome={r['outcome']}  at={r['created_at']} ===")
        try:
            payload = json.loads(r["payload"])
            print(json.dumps(payload, indent=2)[:600])
        except Exception:
            print(r["payload"][:600])
        print()


def cmd_counts(con):
    for tbl in ("missions", "goals", "tasks", "events", "reflections", "skills_history"):
        n = con.execute(f"SELECT COUNT(*) FROM {tbl}").fetchone()[0]
        print(f"  {tbl:<18} {n}")


def cmd_tail(con, n=5):
    seen = con.execute("SELECT MAX(seq) FROM events").fetchone()[0] or 0
    print(f"[tail from seq={seen}; Ctrl-C to stop]")
    while True:
        rows = con.execute(
            "SELECT * FROM events WHERE seq > ? ORDER BY seq ASC LIMIT ?", (seen, n)
        ).fetchall()
        for r in rows:
            print(_fmt_event(r))
            seen = r["seq"]
        time.sleep(2)


def cmd_query(con, sql):
    if any(bad in sql.lower() for bad in ("insert", "update", "delete", "drop", "attach")):
        sys.exit("[ERR] read-only queries only")
    for row in con.execute(sql).fetchall():
        print(dict(row))


def main():
    ap = argparse.ArgumentParser(description="Forge OS backend inspector")
    ap.add_argument("--db", default=os.environ.get("FORGE_DB", str(DEFAULT_DB)))
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("events").add_argument("-n", type=int, default=30)
    p = sub.add_parser("events-for")
    p.add_argument("id")
    p = sub.add_parser("events-type")
    p.add_argument("type")
    p.add_argument("-n", type=int, default=20)
    sub.add_parser("missions")
    p = sub.add_parser("mission")
    p.add_argument("id")
    sub.add_parser("skills")
    p = sub.add_parser("skills-history")
    p.add_argument("name", nargs="?")
    p = sub.add_parser("reflections")
    p.add_argument("mission_id", nargs="?")
    sub.add_parser("counts")
    p = sub.add_parser("tail")
    p.add_argument("-n", type=int, default=5)
    p = sub.add_parser("query")
    p.add_argument("sql")

    args = ap.parse_args()
    con = connect(Path(args.db))
    match args.cmd:
        case "events":         cmd_events(con, args.n)
        case "events-for":     cmd_events_for(con, args.id)
        case "events-type":    cmd_events_type(con, args.type, args.n)
        case "missions":       cmd_missions(con)
        case "mission":        cmd_mission(con, args.id)
        case "skills":         cmd_skills(con)
        case "skills-history": cmd_skills_history(con, args.name)
        case "reflections":    cmd_reflections(con, args.mission_id)
        case "counts":         cmd_counts(con)
        case "tail":           cmd_tail(con, args.n)
        case "query":          cmd_query(con, args.sql)


if __name__ == "__main__":
    main()
