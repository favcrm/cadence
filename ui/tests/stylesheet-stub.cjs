// Compiled tests keep stylesheet imports (`tsrs` writes only JS to its
// output directory) and transitive `@hugeicons/*` imports (which fail under
// Node CJS when `"type": "module"`). Answer them with lightweight stubs; tests
// that supply their own `Module.prototype.require` shim run first as usual.
const Module = require("node:module");

const load = Module._load;
Module._load = function (request, ...rest) {
  if (typeof request === "string" && request.endsWith(".css")) return {};
  if (request === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (request === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return load.call(this, request, ...rest);
};
