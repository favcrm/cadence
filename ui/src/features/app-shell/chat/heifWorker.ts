import { MAX_SOURCE_PIXELS, photoDimensions } from "./photoGeometry";
import { boundHeifWasmMemory } from "./heifWasmMemory";
import { jpegDimensions } from "./jpegDimensions";
import type { HeifAssets } from "./heifAssets";

type CommandResult = { output?: string; blob?: Blob; error?: boolean };
declare function postMessage(message: { blob?: Blob; tooLarge?: boolean }): void;

addEventListener("message", (event: MessageEvent<{ file: File; assets: HeifAssets }>) => {
  void convert(event.data.file, event.data.assets);
});

async function runCommand(file: File, assets: HeifAssets, command: "info" | "decode", primaryIndex = 0, imageCount = 1): Promise<CommandResult> {
  const response = await fetch(command === "info" ? assets.infoWasmUrl : assets.decodeWasmUrl);
  if (!response.ok) throw new Error("HEIF decoder is unavailable");
  const wasm = new Uint8Array(await response.arrayBuffer());
  boundHeifWasmMemory(wasm);
  return new Promise((resolve, reject) => {
    const worker = new Worker(assets.runnerUrl);
    worker.onmessage = (event: MessageEvent<CommandResult>) => {
      worker.terminate();
      if (event.data.error) reject(new Error("HEIF command failed"));
      else resolve(event.data);
    };
    worker.onerror = () => {
      worker.terminate();
      reject(new Error("HEIF worker failed"));
    };
    try {
      worker.postMessage({
        file, command, primaryIndex, imageCount, wasmBytes: wasm.buffer,
        programUrl: command === "info" ? assets.infoUrl : assets.decodeUrl,
      }, [wasm.buffer]);
    } catch {
      worker.terminate();
      reject(new Error("HEIF worker could not start"));
    }
  });
}

async function convert(file: File, assets: HeifAssets): Promise<void> {
  try {
    const info = await runCommand(file, assets, "info");
    const images = [...(info.output ?? "").matchAll(/^image: (\d+)x(\d+) \(id=\d+\)(, primary)?[ \t]*$/gm)]
      .map((match) => ({ width: Number(match[1]), height: Number(match[2]), primary: !!match[3] }));
    const primaryIndex = images.findIndex((image) => image.primary);
    if (!images.length || primaryIndex < 0 || images.filter((image) => image.primary).length !== 1) throw new Error("HEIF primary image is unavailable");
    const primary = images[primaryIndex];
    const size = photoDimensions(primary.width, primary.height);
    if (!size || images.length > 16 || images.some((image) => !photoDimensions(image.width, image.height)) ||
        images.reduce((total, image) => total + image.width * image.height, 0) > MAX_SOURCE_PIXELS) {
      postMessage({ tooLarge: true });
      return;
    }
    // Native HEIF decoders do not expose a memory ceiling. Decode untrusted
    // source only in the bounded WASM worker, then use the browser for JPEG.
    const decoded = await runCommand(file, assets, "decode", primaryIndex, images.length);
    if (!(decoded.blob instanceof Blob) || decoded.blob.size === 0) throw new Error("HEIF decoder returned no image");
    const actual = jpegDimensions(new Uint8Array(await decoded.blob.arrayBuffer()));
    if (actual.width !== primary.width || actual.height !== primary.height) throw new Error("HEIF dimensions changed during decoding");
    const bitmap = await createImageBitmap(decoded.blob, {
      imageOrientation: "from-image", resizeWidth: size.width, resizeHeight: size.height, resizeQuality: "high",
    });
    try {
      const canvas = new OffscreenCanvas(size.width, size.height);
      const context = canvas.getContext("2d", { alpha: false });
      if (!context) throw new Error("Photo canvas is unavailable");
      context.fillStyle = "white";
      context.fillRect(0, 0, size.width, size.height);
      context.drawImage(bitmap, 0, 0);
      const blob = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.86 });
      if (blob.type !== "image/jpeg" || blob.size === 0) throw new Error("JPEG encoding failed");
      postMessage({ blob });
    } finally {
      bitmap.close();
    }
  } catch {
    postMessage({});
  }
}
