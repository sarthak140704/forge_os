import os, sqlite3, json, sys

db = os.path.join(os.environ["APPDATA"], "com.sarthak.forgeos", "forge.sqlite")
c = sqlite3.connect(db)
c.row_factory = sqlite3.Row

mission = "msn_8d87852d-d2d7-4802-bc9a-1e9a17e2ba53"
print(f"=== mission {mission} tool events ===")
rows = c.execute(
    "SELECT seq, event_type, payload FROM events WHERE payload LIKE ? ORDER BY seq",
    (f'%{mission}%',),
).fetchall()
tools = set()
for r in rows:
    p = json.loads(r["payload"])
    et = r["event_type"]
    if et in ("TaskStarted", "TaskCompleted", "TaskFailed", "task_started", "task_completed"):
        print(f"  {et:22s} {json.dumps(p)[:150]}")
    if et in ("ToolStarted", "ToolCompleted", "tool_started", "tool_completed"):
        print(f"  {et:22s} {json.dumps(p)[:200]}")
        # try to extract tool name
        for k in ("tool", "tool_name", "name"):
            if k in p:
                tools.add(p[k]); break

print("\nDistinct tool names seen:", sorted(tools))

# Sample a raw ToolCompleted payload to see its shape
print("\n=== sample ToolCompleted payloads (last 5 globally) ===")
rows2 = c.execute("SELECT seq, event_type, payload FROM events WHERE event_type LIKE '%Tool%' ORDER BY seq DESC LIMIT 5").fetchall()
for r in rows2:
    print(f"  #{r['seq']} {r['event_type']}: {r['payload'][:300]}")

# Distinct event types
print("\n=== distinct event_types ===")
for r in c.execute("SELECT DISTINCT event_type FROM events ORDER BY event_type").fetchall():
    print(f"  {r[0]}")

