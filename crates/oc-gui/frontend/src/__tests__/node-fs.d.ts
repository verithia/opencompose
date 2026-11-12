/**
 * Minimal Node surface for tests that read source CSS and write the layout
 * fixture. The app tsconfig intentionally omits the Node type package, so
 * declare only what is used.
 */
declare const process: { cwd(): string; env: Record<string, string | undefined> };

declare module "node:fs" {
  export interface Dirent {
    name: string;
    isDirectory(): boolean;
    isFile(): boolean;
  }
  export function readFileSync(path: string, encoding: "utf8"): string;
  export function readdirSync(path: string, options: { withFileTypes: true }): Dirent[];
  export function writeFileSync(path: string, data: string): void;
  export function mkdirSync(path: string, options?: { recursive?: boolean }): void;
}

declare module "node:path" {
  const path: {
    resolve(...parts: string[]): string;
    join(...parts: string[]): string;
    basename(file: string): string;
  };
  export default path;
}
