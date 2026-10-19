/**
 * Minimal node typings for vite.config.ts (CAD-573): @types/node is
 * deliberately absent — its ambient `require` collides with the
 * `declare const require` seam in tests/operatorFetch.test.ts
 * (CAD-571). Declare only the imports the config makes.
 */
declare module "node:child_process" {
  export function execSync(
    command: string,
    options?: { encoding?: string; stdio?: unknown },
  ): string;
}

declare module "node:fs" {
  export function readFileSync(path: unknown, encoding: string): string;
  export function readFileSync(path: unknown): Uint8Array;
  export function readdirSync(path: string): string[];
  export function statSync(path: string): { size: number; isDirectory(): boolean };
  export function writeFileSync(path: string, data: Uint8Array): void;
}

declare module "node:path" {
  export function join(...paths: string[]): string;
  export function resolve(...paths: string[]): string;
}

declare module "node:zlib" {
  export const constants: { BROTLI_PARAM_QUALITY: number };
  export function brotliCompressSync(
    data: Uint8Array,
    options?: { params?: Record<number, number> },
  ): Uint8Array;
  export function gzipSync(data: Uint8Array, options?: { level?: number }): Uint8Array;
}

declare module "node:process" {
  const process: { env: Record<string, string | undefined> };
  export default process;
}
