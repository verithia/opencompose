import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  Executable,
} from "vscode-languageclient/node";

import { resolveServerPath } from "./serverPath";

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("opencompose");
  const serverPath = resolveServerPath(config.get<string>("lsp.path"), context.extensionPath);

  const serverExecutable: Executable = {
    command: serverPath,
    args: [],
  };

  const serverOptions: ServerOptions = {
    run: serverExecutable,
    debug: serverExecutable,
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "opencompose" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.ocp"),
    },
  };

  client = new LanguageClient(
    "ocp-lsp",
    "OpenCompose Language Server",
    serverOptions,
    clientOptions
  );

  client.start();
  context.subscriptions.push({
    dispose: () => {
      if (client) client.stop();
    },
  });

  vscode.window.showInformationMessage("OpenCompose LSP started");
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) return undefined;
  return client.stop();
}
