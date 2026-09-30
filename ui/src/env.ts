/** Vite replaces this exact expression in dev/build; CommonJS tests can
 * evaluate it using Node's environment without requiring import.meta. */
declare const process: { env: { NODE_ENV?: string } };
export const isDev: boolean = process.env.NODE_ENV === "development";
