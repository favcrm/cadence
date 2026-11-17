import { jpegDimensions } from "../src/features/app-shell/chat/jpegDimensions";

const frame = [0xff, 0xc0, 0, 8, 8, 0, 2, 0, 3, 1];
const scan = [0xff, 0xda, 0, 2, 0x12, 0xff, 0, 0x34];
const valid = [0xff, 0xd8, ...frame, ...scan, 0xff, 0xd9];
const size = jpegDimensions(Uint8Array.from(valid));
if (size.width !== 3 || size.height !== 2) throw new Error("JPEG frame dimensions were misread");
for (const bytes of [
  [0xff, 0xd8, ...frame, ...scan, ...frame, 0xff, 0xd9],
  valid.slice(0, -1),
  [...valid, 0],
]) {
  let refused = false;
  try { jpegDimensions(Uint8Array.from(bytes)); } catch { refused = true; }
  if (!refused) throw new Error("unsafe JPEG frame was accepted");
}
console.log("jpegDimensions.test.ts: passed");
