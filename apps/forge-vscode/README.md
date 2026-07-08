# Forge OS · VS Code extension

Talks to the Forge OS HTTP API server so you can drive missions and chat from your editor.

## Install (dev)

```bash
cd apps/forge-vscode
npm install
npm run compile
# Then either:
#   1. Press F5 in VS Code with this folder open (Extension Development Host), OR
#   2. Package: npm run package  →  produces forge-os.vsix, install with:
#      code --install-extension forge-os.vsix
```

## Commands

| Command                              | What it does                                                                     |
|--------------------------------------|----------------------------------------------------------------------------------|
| `Forge: Check server health`         | Pings `/health`, shows a notification.                                           |
| `Forge: Run mission (prompt)`        | Asks for a title (+ optional description), POSTs to `/missions`, returns the id. |
| `Forge: Send selection as chat`      | Sends the current selection (or an input box) to `/v1/chat/completions`, opens a scratch markdown doc with the response. |

## Config

`settings.json`:

```jsonc
{
  "forgeOs.apiUrl":   "http://127.0.0.1:7823",
  "forgeOs.apiToken": ""      // prefer setting $env:FORGE_API_TOKEN instead
}
```

If both are set, `FORGE_API_TOKEN` wins.

## What it doesn't do (yet)

- No SSE tailing inside VS Code — use the desktop app or `forge events --follow`.
- No inline code actions — the shim is chat-completions-shaped, so the response is a mission summary, not a diff.
- No streaming — the server doesn't stream completions today.
