import sqlite3, os, sys
from collections import Counter

db = os.path.join(os.environ["APPDATA"], "com.sarthak.forgeos", "forge.sqlite")
c = sqlite3.connect(db).cursor()

c.execute("SELECT name FROM sqlite_master WHERE type='table'")
print("tables:", [r[0] for r in c.fetchall()])

c.execute("PRAGMA table_info(events)")
cols = [r[1] for r in c.fetchall()]
print("events cols:", cols)

c.execute("SELECT COUNT(*) FROM events")
print("\ntotal events:", c.fetchone()[0])

c.execute("SELECT aggregate_id, COUNT(*) FROM events GROUP BY aggregate_id ORDER BY 2 DESC LIMIT 20")
print("\ntop aggregates:")
for aid, n in c.fetchall():
    print(f"  {n:>4}  {aid}")

c.execute("SELECT event_type, COUNT(*) FROM events GROUP BY event_type ORDER BY 2 DESC")
print("\nevent types:")
for t, n in c.fetchall():
    print(f"  {n:>4}  {t}")
