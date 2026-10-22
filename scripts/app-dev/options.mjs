import { existsSync } from "node:fs";

/** Development only: fixtures; there is deliberately no live backend option. */
export function devOptions(args, { inContainer = existsSync("/.dockerenv") } = {}) {
  const app = args[0] || "social-content";
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(app))
    throw new Error("Use an app preview name, not a path.");
  const options = {
    app,
    port: 3186,
    host: "127.0.0.1",
    allowedHosts: [],
    container: false,
  };
  for (let i = 1; i < args.length; i += 2) {
    const key = args[i],
      value = args[i + 1];
    if (!value) throw new Error(`Missing value for ${key}`);
    if (
      key === "--port" &&
      /^\d+$/.test(value) &&
      Number(value) >= 3110 &&
      Number(value) <= 3199
    )
      options.port = Number(value);
    else if (key === "--host" && ["127.0.0.1", "0.0.0.0"].includes(value))
      options.host = value;
    else if (key === "--container" && value === "true")
      options.container = true;
    else if (
      key === "--allow-host" &&
      /^[a-z0-9-]+\.tail[a-z0-9]+\.ts\.net$/.test(value)
    )
      options.allowedHosts.push(value);
    else
      throw new Error(
        `Unsupported development option ${key}. Fixtures only; ports 3110–3199; no backend, proxy or credentials.`,
      );
  }
  if (options.container && !inContainer)
    throw new Error(
      "--container is only valid inside the app preview container.",
    );
  if (
    options.host === "0.0.0.0" &&
    !options.allowedHosts.length &&
    !options.container
  )
    throw new Error(
      "Private sharing needs an explicit existing tailnet hostname.",
    );
  return options;
}
