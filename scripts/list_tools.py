import os, sqlite3
db = os.path.join(os.environ['APPDATA'],'com.sarthak.forgeos','forge.sqlite')
c = sqlite3.connect(db)
rows = c.execute("SELECT DISTINCT json_extract(payload,'$.tool') FROM events WHERE event_type='tool_invoked' AND json_extract(payload,'$.tool') IS NOT NULL").fetchall()
print("distinct tool names in event stream:")
for r in rows:
    print(" ", r[0])
