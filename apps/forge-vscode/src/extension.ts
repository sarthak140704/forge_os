// Forge OS extension entry point.
//
// The extension holds no state and does no I/O at activation; it just
// registers three commands that speak the Forge HTTP API. Everything is a
// fetch call against the OpenAI-compat chat shim so the same code works
// against any OpenAI-shaped endpoint the user points it at.

import * as vscode from 'vscode';

interface Settings {
  apiUrl:   string;
  apiToken: string;
}

function readSettings(): Settings {
  const cfg = vscode.workspace.getConfiguration('forgeOs');
  const apiUrl   = (cfg.get<string>('apiUrl') || 'http://127.0.0.1:7823').replace(/\/+$/, '');
  // Env var wins over settings so users can keep secrets out of settings.json.
  const apiToken = process.env.FORGE_API_TOKEN || cfg.get<string>('apiToken') || '';
  return { apiUrl, apiToken };
}

async function authedFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const { apiUrl, apiToken } = readSettings();
  const headers = new Headers(init.headers);
  if (apiToken) headers.set('Authorization', `Bearer ${apiToken}`);
  headers.set('Content-Type', 'application/json');
  return fetch(`${apiUrl}${path}`, { ...init, headers });
}

async function healthCheck(): Promise<void> {
  const { apiUrl } = readSettings();
  try {
    // /health is unauthenticated on purpose.
    const r = await fetch(`${apiUrl}/health`);
    if (r.ok) {
      vscode.window.showInformationMessage(`Forge server OK at ${apiUrl}`);
    } else {
      vscode.window.showWarningMessage(`Forge server responded ${r.status} at ${apiUrl}`);
    }
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Forge server unreachable at ${apiUrl}: ${msg}`);
  }
}

async function runMissionPrompt(): Promise<void> {
  const title = await vscode.window.showInputBox({
    prompt:       'Mission title',
    placeHolder:  'e.g. "add a rate-limiter to services/auth"',
    ignoreFocusOut: true,
  });
  if (!title) return;
  const description = await vscode.window.showInputBox({
    prompt: 'Optional description (defaults to the title)',
    ignoreFocusOut: true,
  });

  await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `Forge: creating "${title}"…` },
    async () => {
      try {
        const r = await authedFetch('/missions', {
          method: 'POST',
          body:   JSON.stringify({ title, description: description || title }),
        });
        if (!r.ok) {
          const body = await r.text();
          vscode.window.showErrorMessage(`Forge: create mission failed (${r.status}): ${body}`);
          return;
        }
        const j: { id: string } = await r.json() as { id: string };
        vscode.window.showInformationMessage(
          `Forge: mission ${j.id.slice(0, 8)}… created — watch the desktop UI or run "forge events --mission ${j.id} --follow".`
        );
      } catch (err: unknown) {
        vscode.window.showErrorMessage(`Forge: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  );
}

async function runSelectionAsChat(): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    vscode.window.showWarningMessage('Forge: no active editor.');
    return;
  }
  const selection = editor.document.getText(editor.selection);
  const prompt = selection.trim().length > 0
    ? selection
    : await vscode.window.showInputBox({ prompt: 'Prompt', ignoreFocusOut: true }) ?? '';
  if (!prompt.trim()) return;

  const doc = await vscode.workspace.openTextDocument({
    language: 'markdown',
    content:  `# Forge chat\n\n**Prompt:** ${prompt}\n\n**Response:**\n\n(waiting…)`,
  });
  const panel = await vscode.window.showTextDocument(doc, { preview: true });

  try {
    const r = await authedFetch('/v1/chat/completions', {
      method: 'POST',
      body:   JSON.stringify({
        model:    'forge-mission',
        messages: [{ role: 'user', content: prompt }],
        stream:   false,
      }),
    });
    if (!r.ok) {
      const body = await r.text();
      await panel.edit((eb) =>
        eb.replace(new vscode.Range(0, 0, doc.lineCount, 0),
          `# Forge chat\n\n**Prompt:** ${prompt}\n\n**Error ${r.status}:**\n\n${body}`)
      );
      return;
    }
    const j = (await r.json()) as { choices: { message: { content: string }, finish_reason: string }[] };
    const answer = j.choices[0]?.message?.content ?? '(no content)';
    const reason = j.choices[0]?.finish_reason   ?? '?';
    await panel.edit((eb) =>
      eb.replace(new vscode.Range(0, 0, doc.lineCount, 0),
        `# Forge chat\n\n**Prompt:** ${prompt}\n\n**Response:**\n\n${answer}\n\n---\n_finish_reason: ${reason}_`)
    );
  } catch (err: unknown) {
    vscode.window.showErrorMessage(`Forge: ${err instanceof Error ? err.message : String(err)}`);
  }
}

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('forgeOs.health',              healthCheck),
    vscode.commands.registerCommand('forgeOs.runMission',          runMissionPrompt),
    vscode.commands.registerCommand('forgeOs.runSelectionAsChat',  runSelectionAsChat),
  );
}

export function deactivate(): void { /* no-op */ }
