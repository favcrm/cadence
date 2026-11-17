import { isHeif } from "../src/features/app-shell/chat/photoKind";

function bmff(major: string, compatible: string[] = [], zeroSize = false, name = "renamed.jpg", type = "image/jpeg"): File {
  const bytes = new Uint8Array(16 + compatible.length * 4);
  new DataView(bytes.buffer).setUint32(0, zeroSize ? 0 : bytes.length);
  for (const [index, brand] of ["ftyp", major, ...compatible].entries()) {
    const offset = index === 0 ? 4 : index === 1 ? 8 : 16 + (index - 2) * 4;
    for (let i = 0; i < 4; i++) bytes[offset + i] = brand.charCodeAt(i);
  }
  return new File([bytes], name, { type });
}

async function main(): Promise<void> {
  const signal = new AbortController().signal;
  for (const [major, compatible, zeroSize] of [
    ["heim", [], false], ["mif2", [], false], ["avif", [], false],
    ["isom", ["heic"], false], ["isom", ["mif3"], true],
  ] as [string, string[], boolean][]) {
    if (!await isHeif(bmff(major, compatible, zeroSize, "renamed.bin", "application/octet-stream"), signal))
      throw new Error(`HEIF brand ${major} was missed despite a misleading filename`);
  }
  if (!await isHeif(bmff("new1"), signal)) throw new Error("unknown image-labelled BMFF was sent unchanged");
  if (await isHeif(bmff("isom", ["mp41", "mp42"], false, "clip.mp4", "video/mp4"), signal)) throw new Error("ordinary MP4 was mistaken for a photo");
  console.log("photoKind.test.ts: passed");
}
void main();
