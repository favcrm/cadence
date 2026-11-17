import { MAX_SOURCE_PIXELS, photoDimensions } from "./photoGeometry";
import type { HeifAssets } from "./heifAssets";

type CommandResult = { output?: string; blob?: Blob; error?: boolean };
declare function postMessage(message: { blob?: Blob; tooLarge?: boolean }): void;

addEventListener("message", (event: MessageEvent<{ file: File; assets: HeifAssets }>) => {
  void convert(event.data.file, event.data.assets);
});

function runCommand(file: File, assets: HeifAssets, command: "info" | "decode", primaryIndex = 0, imageCount = 1): Promise<CommandResult> {
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
        file, command, primaryIndex, imageCount,
        programUrl: command === "info" ? assets.infoUrl : assets.decodeUrl,
        wasmUrl: command === "info" ? assets.infoWasmUrl : assets.decodeWasmUrl,
      });
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
    const size = photoDimensions(images[primaryIndex].width, images[primaryIndex].height);
    if (!size || images.some((image) => !photoDimensions(image.width, image.height)) ||
        images.reduce((total, image) => total + image.width * image.height, 0) > MAX_SOURCE_PIXELS) {
      postMessage({ tooLarge: true });
      return;
    }
    let bitmap: ImageBitmap | undefined;
    try {
      bitmap = await createImageBitmap(file, {
        imageOrientation: "from-image", resizeWidth: size.width, resizeHeight: size.height, resizeQuality: "high",
      });
    } catch {
      // The patched local decoder covers browsers without native HEIF support.
    }
    if (!bitmap) {
      const decoded = await runCommand(file, assets, "decode", primaryIndex, images.length);
      if (!(decoded.blob instanceof Blob) || decoded.blob.size === 0) throw new Error("HEIF decoder returned no image");
      bitmap = await createImageBitmap(decoded.blob, {
        imageOrientation: "from-image", resizeWidth: size.width, resizeHeight: size.height, resizeQuality: "high",
      });
    }
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
