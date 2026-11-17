import runnerUrl from "./heifCommandWorker.js?url";
import infoUrl from "@ai-ecoverse/wasm-libheif/bin/heif-info?url";
import infoWasmUrl from "@ai-ecoverse/wasm-libheif/bin/heif-info.wasm?url";
import decodeUrl from "@ai-ecoverse/wasm-libheif/bin/heif-dec?url";
import decodeWasmUrl from "@ai-ecoverse/wasm-libheif/bin/heif-dec.wasm?url";
import licenseUrl from "@ai-ecoverse/wasm-libheif/LICENSE?url";
import noticesUrl from "@ai-ecoverse/wasm-libheif/THIRD-PARTY-NOTICES.md?url";

export interface HeifAssets {
  runnerUrl: string;
  infoUrl: string;
  infoWasmUrl: string;
  decodeUrl: string;
  decodeWasmUrl: string;
  licenseUrl: string;
  noticesUrl: string;
}

const assets: HeifAssets = { runnerUrl, infoUrl, infoWasmUrl, decodeUrl, decodeWasmUrl, licenseUrl, noticesUrl };
export default assets;
