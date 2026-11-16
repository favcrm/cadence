// Compiled tests keep a component's stylesheet import, but `tsrs` writes only
// JavaScript into its output directory, so `require("./decisions.css")` fails on
// a file that does not exist there. Answer every stylesheet require with an
// empty module: styles never decide a test's behaviour, and a component that
// adds an import must not break the lane.
const Module = require("node:module");

const load = Module._load;
Module._load = function (request, ...rest) {
  if (typeof request === "string" && request.endsWith(".css")) return {};
  return load.call(this, request, ...rest);
};
