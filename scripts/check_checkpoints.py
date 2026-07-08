import os, sqlite3
db = os.path.join(os.environ['APPDATA'],'com.sarthak.forgeos','forge.sqlite')
c = sqlite3.connect(db)
print("=== checkpoint_created events in DB ===")
rows = c.execute("SELECT seq, created_at, payload FROM events WHERE event_type='checkpoint_created' ORDER BY seq DESC LIMIT 10").fetchall()
if not rows:
    print("  (none yet)")
for r in rows:
    print(f"  #{r[0]} {r[1]}: {r[2][:200]}")

print("\n=== last 5 tool_invoked events (any mission) ===")
rows = c.execute("SELECT seq, created_at, payload FROM events WHERE event_type='tool_invoked' ORDER BY seq DESC LIMIT 5").fetchall()
for r in rows:
    print(f"  #{r[0]} {r[1]}: {r[2][:150]}")

print("\n=== task_completed events after 04:20Z (post-fix boot) ===")
rows = c.execute("SELECT seq, created_at, payload FROM events WHERE event_type='task_completed' AND created_at > '2026-07-08T04:20:00Z' ORDER BY seq DESC LIMIT 10").fetchall()
if not rows:
    print("  (none — no missions have run since the fix compiled)")
for r in rows:
    print(f"  #{r[0]} {r[1]}: {r[2][:150]}")
