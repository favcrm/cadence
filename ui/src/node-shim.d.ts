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
}

declare module "node:process" {
  const process: { env: Record<string, string | undefined> };
  export default process;
}
