import { boundHeifWasmMemory } from "../src/features/app-shell/chat/heifWasmMemory";

// A module exporting one memory with minimum one page and maximum eight.
const moduleBytes = () => Uint8Array.from([
  0, 0x61, 0x73, 0x6d, 1, 0, 0, 0,
  5, 4, 1, 1, 1, 8,
  7, 7, 1, 3, 0x6d, 0x65, 0x6d, 2, 0,
]);
const bytes = moduleBytes();
boundHeifWasmMemory(bytes, 4);
const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes));
const memory = instance.exports.mem;
if (!(memory instanceof WebAssembly.Memory)) throw new Error("decoder memory export missing");
if (memory.grow(3) !== 1) throw new Error("decoder memory could not grow to its bound");
let refused = false;
try { memory.grow(1); } catch (error) { refused = error instanceof RangeError; }
if (!refused) throw new Error("decoder memory grew beyond its bound");

const malformed = moduleBytes().subarray(0, 13);
let invalidRefused = false;
try { boundHeifWasmMemory(malformed, 4); } catch { invalidRefused = true; }
if (!invalidRefused) throw new Error("truncated decoder module was accepted");
console.log("heifWasmMemory.test.ts: passed");
