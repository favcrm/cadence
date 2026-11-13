/**
 * Generic client-side audio container sniffing, 16 kHz mono PCM16 WAV
 * chunk normalization, and live microphone segment capture for `screen-audio.v1`.
 *
 * Raw audio bytes never cross a screen iframe `MessagePort`; normalization and
 * broker upload stay strictly in the top-level board host.
 */

export const SCREEN_AUDIO_ACCEPT =
  "audio/*,.m4a,.mp3,.wav,.webm,.ogg,.opus,.aac,.flac,.3gp,.amr,.mp4";

export const AUDIO_SAMPLE_RATE = 16_000;
export const MAX_AUDIO_UPLOAD_BYTES = 50 * 1024 * 1024;
export const UPLOAD_CHUNK_SECONDS = 45;
export const RECORD_SEGMENT_SECONDS = 10;
export const MAX_AUDIO_DURATION_SECONDS = 2 * 60 * 60;

export type AudioContainerKind =
  | "wav"
  | "mp4"
  | "ogg"
  | "webm"
  | "flac"
  | "amr"
  | "mpeg";

export type AudioNormalizeErrorCode =
  | "empty_audio"
  | "audio_too_large"
  | "unsupported_audio_format"
  | "decode_failed"
  | "duration_too_long";

export class AudioNormalizeError extends Error {
  readonly code: AudioNormalizeErrorCode;

  constructor(code: AudioNormalizeErrorCode, message: string) {
    super(message);
    this.name = "AudioNormalizeError";
    this.code = code;
  }
}

export interface WavAudioChunk {
  index: number;
  frames: number;
  durationMs: number;
  wavBytes: Uint8Array;
  base64: string;
}

export interface NormalizedAudioChunks {
  container: AudioContainerKind;
  totalFrames: number;
  durationSeconds: number;
  heldAt: string;
  chunks: WavAudioChunk[];
}

export type OfflineResampler = (
  mono: Float32Array,
  sourceRate: number,
  targetFrames: number,
) => Promise<Float32Array>;

function asciiAt(bytes: Uint8Array, offset: number, expected: string): boolean {
  if (offset < 0 || offset + expected.length > bytes.length) return false;
  for (let i = 0; i < expected.length; i++) {
    if (bytes[offset + i] !== expected.charCodeAt(i)) return false;
  }
  return true;
}

function isMpegOrAdtsSync(bytes: Uint8Array, offset: number): boolean {
  if (offset + 2 > bytes.length) return false;
  const b0 = bytes[offset];
  const b1 = bytes[offset + 1];
  if (b0 !== 0xff || (b1 & 0xe0) !== 0xe0) return false;
  const versionBits = (b1 >> 3) & 0x03;
  // MPEG version 01 is reserved in both MP3 frame headers and ADTS.
  return versionBits !== 0x01;
}

/**
 * Sniff mobile/desktop audio containers from leading magic bytes rather than
 * trusting browser `File.type` (iOS Files `.m4a` and Android/WhatsApp `.opus`
 * routinely report `application/octet-stream`, `video/mp4`, or `""`).
 */
export function sniffAudioContainer(header: Uint8Array): AudioContainerKind | null {
  if (header.length < 4) return null;
  if (header.length >= 12 && asciiAt(header, 0, "RIFF") && asciiAt(header, 8, "WAVE")) {
    return "wav";
  }
  if (header.length >= 8 && asciiAt(header, 4, "ftyp")) {
    return "mp4";
  }
  if (asciiAt(header, 0, "OggS")) {
    return "ogg";
  }
  if (
    header.length >= 4 &&
    header[0] === 0x1a &&
    header[1] === 0x45 &&
    header[2] === 0xdf &&
    header[3] === 0xa3
  ) {
    return "webm";
  }
  if (asciiAt(header, 0, "fLaC")) {
    return "flac";
  }
  if (header.length >= 5 && asciiAt(header, 0, "#!AMR")) {
    return "amr";
  }
  if (asciiAt(header, 0, "ID3")) {
    return "mpeg";
  }
  if (isMpegOrAdtsSync(header, 0)) {
    return "mpeg";
  }
  return null;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += step) {
    const slice = bytes.subarray(offset, offset + step);
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

export function floatToPcm16Sample(sample: number): number {
  if (!Number.isFinite(sample)) return 0;
  const clamped = sample < -1 ? -1 : sample > 1 ? 1 : sample;
  return clamped < 0 ? Math.round(clamped * 0x8000) : Math.round(clamped * 0x7fff);
}

/** Encode mono samples into a canonical 44-byte header 16 kHz PCM16 WAV. */
export function encodeWav16kMonoPcm16(samples: Float32Array | Int16Array): Uint8Array<ArrayBuffer> {
  const frames = samples.length;
  const dataBytes = frames * 2;
  const out = new Uint8Array(44 + dataBytes);
  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);

  // RIFF chunk descriptor
  out.set([0x52, 0x49, 0x46, 0x46], 0); // "RIFF"
  view.setUint32(4, 36 + dataBytes, true);
  out.set([0x57, 0x41, 0x56, 0x45], 8); // "WAVE"

  // fmt sub-chunk
  out.set([0x66, 0x6d, 0x74, 0x20], 12); // "fmt "
  view.setUint32(16, 16, true); // PCM fmt chunk size
  view.setUint16(20, 1, true); // AudioFormat = 1 (PCM)
  view.setUint16(22, 1, true); // NumChannels = 1 (mono)
  view.setUint32(24, AUDIO_SAMPLE_RATE, true); // SampleRate = 16000
  view.setUint32(28, AUDIO_SAMPLE_RATE * 2, true); // ByteRate = 32000
  view.setUint16(32, 2, true); // BlockAlign = 2
  view.setUint16(34, 16, true); // BitsPerSample = 16

  // data sub-chunk
  out.set([0x64, 0x61, 0x74, 0x61], 36); // "data"
  view.setUint32(40, dataBytes, true);

  if (samples instanceof Int16Array) {
    for (let i = 0; i < frames; i++) {
      view.setInt16(44 + i * 2, samples[i]!, true);
    }
  } else {
    for (let i = 0; i < frames; i++) {
      view.setInt16(44 + i * 2, floatToPcm16Sample(samples[i]!), true);
    }
  }
  return out;
}

function encodeRawPcm16BytesAsWav(pcmBytes: Uint8Array): Uint8Array {
  const dataBytes = pcmBytes.byteLength - (pcmBytes.byteLength % 2);
  const out = new Uint8Array(44 + dataBytes);
  const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
  out.set([0x52, 0x49, 0x46, 0x46], 0);
  view.setUint32(4, 36 + dataBytes, true);
  out.set([0x57, 0x41, 0x56, 0x45], 8);
  out.set([0x66, 0x6d, 0x74, 0x20], 12);
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, AUDIO_SAMPLE_RATE, true);
  view.setUint32(28, AUDIO_SAMPLE_RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  out.set([0x64, 0x61, 0x74, 0x61], 36);
  view.setUint32(40, dataBytes, true);
  out.set(pcmBytes.subarray(0, dataBytes), 44);
  return out;
}

/**
 * Fast-path slicer when an uploaded WAV is already 16 kHz mono PCM16: slices
 * raw PCM16 frames directly into <= 45s canonical WAV chunks without WebAudio.
 */
export function trySliceCanonicalWav16k(
  bytes: Uint8Array,
  heldAt: string,
  chunkSeconds = UPLOAD_CHUNK_SECONDS,
): NormalizedAudioChunks | null {
  if (bytes.length < 44 || !asciiAt(bytes, 0, "RIFF") || !asciiAt(bytes, 8, "WAVE")) {
    return null;
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 12;
  let fmtOk = false;
  let dataOffset = -1;
  let dataLength = 0;

  while (offset + 8 <= bytes.length) {
    const id = String.fromCharCode(
      bytes[offset]!,
      bytes[offset + 1]!,
      bytes[offset + 2]!,
      bytes[offset + 3]!,
    );
    const size = view.getUint32(offset + 4, true);
    const bodyStart = offset + 8;
    if (bodyStart > bytes.length) break;
    const boundedSize = Math.min(size, bytes.length - bodyStart);

    if (id === "fmt ") {
      if (boundedSize < 16) return null;
      const format = view.getUint16(bodyStart, true);
      const channels = view.getUint16(bodyStart + 2, true);
      const sampleRate = view.getUint32(bodyStart + 4, true);
      const blockAlign = view.getUint16(bodyStart + 12, true);
      const bitsPerSample = view.getUint16(bodyStart + 14, true);
      if (
        format !== 1 ||
        channels !== 1 ||
        sampleRate !== AUDIO_SAMPLE_RATE ||
        blockAlign !== 2 ||
        bitsPerSample !== 16
      ) {
        return null;
      }
      fmtOk = true;
    } else if (id === "data") {
      dataOffset = bodyStart;
      dataLength = boundedSize - (boundedSize % 2);
      break;
    }
    offset = bodyStart + size + (size % 2);
  }

  if (!fmtOk || dataOffset < 0) return null;
  const totalFrames = Math.floor(dataLength / 2);
  if (totalFrames <= 0) {
    throw new AudioNormalizeError("empty_audio", "The selected audio file contains no audio frames.");
  }
  if (totalFrames > MAX_AUDIO_DURATION_SECONDS * AUDIO_SAMPLE_RATE) {
    throw new AudioNormalizeError("duration_too_long", "Audio exceeds the 2-hour maximum duration.");
  }

  const maxChunkFrames = Math.max(1, Math.floor(chunkSeconds * AUDIO_SAMPLE_RATE));
  const chunks: WavAudioChunk[] = [];
  for (let startFrame = 0, index = 0; startFrame < totalFrames; startFrame += maxChunkFrames, index++) {
    const frames = Math.min(maxChunkFrames, totalFrames - startFrame);
    const byteStart = dataOffset + startFrame * 2;
    const pcmSlice = bytes.subarray(byteStart, byteStart + frames * 2);
    const wavBytes = encodeRawPcm16BytesAsWav(pcmSlice);
    chunks.push({
      index,
      frames,
      durationMs: Math.max(1, Math.round((frames / AUDIO_SAMPLE_RATE) * 1000)),
      wavBytes,
      base64: bytesToBase64(wavBytes),
    });
  }

  return {
    container: "wav",
    totalFrames,
    durationSeconds: Math.max(1, Math.round(totalFrames / AUDIO_SAMPLE_RATE)),
    heldAt,
    chunks,
  };
}

/** Deterministic anti-aliased mono resampler fallback when `OfflineAudioContext` is absent. */
export function resampleMonoFallback(
  mono: Float32Array,
  sourceRate: number,
  targetFrames: number,
): Float32Array {
  if (targetFrames <= 0 || mono.length === 0) return new Float32Array(0);
  if (sourceRate === AUDIO_SAMPLE_RATE && mono.length === targetFrames) return mono;
  const out = new Float32Array(targetFrames);
  const ratio = sourceRate / AUDIO_SAMPLE_RATE;

  if (ratio > 1) {
    // Box-filter averaging prevents high-frequency aliasing when downsampling (e.g. 48k -> 16k).
    for (let i = 0; i < targetFrames; i++) {
      const start = i * ratio;
      const end = Math.min(mono.length, (i + 1) * ratio);
      const iStart = Math.floor(start);
      const iEnd = Math.max(iStart + 1, Math.ceil(end));
      let sum = 0;
      let count = 0;
      for (let j = iStart; j < iEnd && j < mono.length; j++) {
        sum += mono[j]!;
        count++;
      }
      out[i] = count > 0 ? sum / count : 0;
    }
    return out;
  }

  // Linear interpolation when upsampling (e.g. 8 kHz AMR -> 16 kHz).
  for (let i = 0; i < targetFrames; i++) {
    const pos = i * ratio;
    const idx = Math.floor(pos);
    const frac = pos - idx;
    const a = mono[Math.min(idx, mono.length - 1)] ?? 0;
    const b = mono[Math.min(idx + 1, mono.length - 1)] ?? a;
    out[i] = a + (b - a) * frac;
  }
  return out;
}

async function defaultOfflineResampler(
  mono: Float32Array,
  sourceRate: number,
  targetFrames: number,
): Promise<Float32Array> {
  if (sourceRate === AUDIO_SAMPLE_RATE) return mono;
  const OfflineCtx = globalThis.OfflineAudioContext;
  if (typeof OfflineCtx === "function") {
    try {
      const offline = new OfflineCtx(1, targetFrames, AUDIO_SAMPLE_RATE);
      const srcBuffer = offline.createBuffer(1, mono.length, sourceRate);
      srcBuffer.getChannelData(0).set(mono);
      const source = offline.createBufferSource();
      source.buffer = srcBuffer;
      source.connect(offline.destination);
      source.start(0);
      const rendered = await offline.startRendering();
      return new Float32Array(rendered.getChannelData(0));
    } catch {
      // Fall back to pure box-filter / linear resampler if OfflineAudioContext refuses the rate.
    }
  }
  return resampleMonoFallback(mono, sourceRate, targetFrames);
}

/**
 * Windowed channel mixdown + 16 kHz mono PCM16 WAV chunking.
 * Processes one <= 45s window at a time so full-hour stereo recordings never
 * allocate a second full-length resampled buffer.
 */
export async function normalizeChannelDataToWavChunks(
  channels: readonly Float32Array[],
  sampleRate: number,
  options: {
    container?: AudioContainerKind;
    heldAt?: string;
    chunkSeconds?: number;
    resampler?: OfflineResampler;
    signal?: AbortSignal;
    onChunkNormalized?: (done: number, total: number) => void;
  } = {},
): Promise<NormalizedAudioChunks> {
  if (channels.length === 0 || !Number.isFinite(sampleRate) || sampleRate <= 0) {
    throw new AudioNormalizeError("empty_audio", "The selected audio file contains no audio channels.");
  }
  const sourceFrames = channels[0]!.length;
  if (sourceFrames <= 0) {
    throw new AudioNormalizeError("empty_audio", "The selected audio file contains no audio frames.");
  }
  const durationExact = sourceFrames / sampleRate;
  if (durationExact > MAX_AUDIO_DURATION_SECONDS) {
    throw new AudioNormalizeError("duration_too_long", "Audio exceeds the 2-hour maximum duration.");
  }

  const chunkSeconds = options.chunkSeconds ?? UPLOAD_CHUNK_SECONDS;
  const resampler = options.resampler ?? defaultOfflineResampler;
  const heldAt = options.heldAt ?? new Date().toISOString();
  const windowSourceFrames = Math.max(1, Math.round(chunkSeconds * sampleRate));
  const totalChunks = Math.max(1, Math.ceil(sourceFrames / windowSourceFrames));
  const channelCount = channels.length;
  const invChannels = 1 / channelCount;

  const chunks: WavAudioChunk[] = [];
  let totalFrames16k = 0;

  for (let start = 0, index = 0; start < sourceFrames; start += windowSourceFrames, index++) {
    if (options.signal?.aborted) {
      throw new DOMException("Aborted", "AbortError");
    }
    const len = Math.min(windowSourceFrames, sourceFrames - start);
    let mono: Float32Array;
    if (channelCount === 1) {
      mono = channels[0]!.subarray(start, start + len);
    } else {
      mono = new Float32Array(len);
      for (let ch = 0; ch < channelCount; ch++) {
        const data = channels[ch]!;
        for (let i = 0; i < len; i++) {
          mono[i] = mono[i]! + (data[start + i] ?? 0) * invChannels;
        }
      }
    }

    const maxFramesPerChunk = Math.floor(60 * AUDIO_SAMPLE_RATE);
    const targetFrames = Math.min(
      maxFramesPerChunk,
      Math.max(1, Math.round((len / sampleRate) * AUDIO_SAMPLE_RATE)),
    );
    const resampled =
      sampleRate === AUDIO_SAMPLE_RATE
        ? mono
        : await resampler(mono, sampleRate, targetFrames);
    const wavBytes = encodeWav16kMonoPcm16(resampled);
    totalFrames16k += resampled.length;
    chunks.push({
      index,
      frames: resampled.length,
      durationMs: Math.max(1, Math.round((resampled.length / AUDIO_SAMPLE_RATE) * 1000)),
      wavBytes,
      base64: bytesToBase64(wavBytes),
    });
    options.onChunkNormalized?.(index + 1, totalChunks);
  }

  return {
    container: options.container ?? "wav",
    totalFrames: totalFrames16k,
    durationSeconds: Math.max(1, Math.round(totalFrames16k / AUDIO_SAMPLE_RATE)),
    heldAt,
    chunks,
  };
}

export function deriveAudioHeldAt(lastModified?: number, nowMs = Date.now()): string {
  // Accept plausible file timestamps between 2000-01-01 and +24h from now.
  const minValidMs = 946_684_800_000;
  if (
    typeof lastModified === "number" &&
    Number.isFinite(lastModified) &&
    lastModified >= minValidMs &&
    lastModified <= nowMs + 86_400_000
  ) {
    return new Date(lastModified).toISOString();
  }
  return new Date(nowMs).toISOString();
}

/**
 * Validate, decode, and normalize an uploaded mobile/desktop audio file into
 * bounded <= 45s 16 kHz mono PCM16 WAV chunks.
 */
export async function normalizeAudioFile(
  file: Blob & { name?: string; lastModified?: number },
  options: {
    signal?: AbortSignal;
    chunkSeconds?: number;
    onChunkNormalized?: (done: number, total: number) => void;
  } = {},
): Promise<NormalizedAudioChunks> {
  if (file.size <= 0) {
    throw new AudioNormalizeError("empty_audio", "The selected audio file is empty.");
  }
  if (file.size > MAX_AUDIO_UPLOAD_BYTES) {
    throw new AudioNormalizeError("audio_too_large", "Audio file exceeds the 50 MB upload limit.");
  }

  const headerBuf = await file.slice(0, 64).arrayBuffer();
  const container = sniffAudioContainer(new Uint8Array(headerBuf));
  if (!container) {
    throw new AudioNormalizeError(
      "unsupported_audio_format",
      "Unsupported audio file format. Choose an M4A, MP3, WAV, Opus/Ogg, WebM, AAC, FLAC, 3GP, or AMR recording.",
    );
  }
  if (options.signal?.aborted) {
    throw new DOMException("Aborted", "AbortError");
  }

  const heldAt = deriveAudioHeldAt(file.lastModified);
  const fullBuffer = await file.arrayBuffer();
  if (options.signal?.aborted) {
    throw new DOMException("Aborted", "AbortError");
  }

  if (container === "wav") {
    const fastSliced = trySliceCanonicalWav16k(
      new Uint8Array(fullBuffer),
      heldAt,
      options.chunkSeconds ?? UPLOAD_CHUNK_SECONDS,
    );
    if (fastSliced) {
      options.onChunkNormalized?.(fastSliced.chunks.length, fastSliced.chunks.length);
      return fastSliced;
    }
  }

  const AudioCtx =
    globalThis.AudioContext ??
    (globalThis as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (typeof AudioCtx !== "function") {
    throw new AudioNormalizeError("decode_failed", "Browser WebAudio decoding is unavailable.");
  }

  let audioCtx: AudioContext | null = null;
  try {
    try {
      // Requesting 16 kHz directly cuts decoded PCM memory by 3x on 48 kHz recordings.
      audioCtx = new AudioCtx({ sampleRate: AUDIO_SAMPLE_RATE });
    } catch {
      audioCtx = new AudioCtx();
    }
    const decoded = await audioCtx.decodeAudioData(fullBuffer);
    const channels: Float32Array[] = [];
    for (let ch = 0; ch < decoded.numberOfChannels; ch++) {
      channels.push(decoded.getChannelData(ch));
    }
    return await normalizeChannelDataToWavChunks(channels, decoded.sampleRate, {
      container,
      heldAt,
      chunkSeconds: options.chunkSeconds ?? UPLOAD_CHUNK_SECONDS,
      signal: options.signal,
      onChunkNormalized: options.onChunkNormalized,
    });
  } catch (error) {
    if (error instanceof AudioNormalizeError || (error instanceof DOMException && error.name === "AbortError")) {
      throw error;
    }
    throw new AudioNormalizeError(
      "decode_failed",
      "This browser could not decode that audio format. Try M4A, MP3, WAV, Ogg, or WebM.",
    );
  } finally {
    if (audioCtx) {
      await audioCtx.close().catch(() => {});
    }
  }
}

const WORKLET_PROCESSOR_SOURCE = `
class ScreenAudioCaptureProcessor extends AudioWorkletProcessor {
  process(inputs) {
    const input = inputs[0];
    if (input && input.length > 0 && input[0] && input[0].length > 0) {
      const chCount = input.length;
      const frames = input[0].length;
      const mono = new Float32Array(frames);
      if (chCount === 1) {
        mono.set(input[0]);
      } else {
        const scale = 1 / chCount;
        for (let c = 0; c < chCount; c++) {
          const ch = input[c];
          if (!ch) continue;
          for (let i = 0; i < frames; i++) mono[i] += ch[i] * scale;
        }
      }
      this.port.postMessage(mono, [mono.buffer]);
    }
    return true;
  }
}
registerProcessor("screen-audio-capture", ScreenAudioCaptureProcessor);
`;

export interface LiveMicrophoneCapture {
  readonly startedAtIso: string;
  pause(): void;
  resume(): void;
  finish(): Promise<{ totalFrames: number; durationSeconds: number }>;
  abort(): Promise<void>;
}

export async function startLiveMicrophoneCapture(options: {
  segmentSeconds?: number;
  onLevel?: (level: number) => void;
  onElapsedMs?: (elapsedMs: number) => void;
  onSegment: (chunk: WavAudioChunk) => void;
}): Promise<LiveMicrophoneCapture> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("microphone_unsupported");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
    },
  });

  const startedAtIso = new Date().toISOString();
  const AudioCtx =
    globalThis.AudioContext ??
    (globalThis as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (typeof AudioCtx !== "function") {
    stream.getTracks().forEach((track) => track.stop());
    throw new Error("webaudio_unsupported");
  }

  let audioCtx: AudioContext | null = null;
  let sourceNode: MediaStreamAudioSourceNode | null = null;
  let silentGain: GainNode | null = null;
  let workletNode: AudioWorkletNode | null = null;
  let fallbackNode: ScriptProcessorNode | null = null;

  try {
    try {
      audioCtx = new AudioCtx({ sampleRate: AUDIO_SAMPLE_RATE });
    } catch {
      audioCtx = new AudioCtx();
    }
    if (audioCtx.state === "suspended") {
      await audioCtx.resume().catch(() => {});
    }
    try {
      sourceNode = audioCtx.createMediaStreamSource(stream);
    } catch {
      // Firefox allows new AudioContext({ sampleRate: 16000 }) and then throws
      // NotSupportedError in createMediaStreamSource when the mic track runs at 48/44.1 kHz.
      await audioCtx.close().catch(() => {});
      audioCtx = new AudioCtx();
      if (audioCtx.state === "suspended") {
        await audioCtx.resume().catch(() => {});
      }
      sourceNode = audioCtx.createMediaStreamSource(stream);
    }

    const activeCtx = audioCtx;
    const activeSource = sourceNode;
    const inputRate = activeCtx.sampleRate;
    const segmentFrames = Math.max(
      AUDIO_SAMPLE_RATE,
      Math.floor((options.segmentSeconds ?? RECORD_SEGMENT_SECONDS) * AUDIO_SAMPLE_RATE),
    );

    let paused = false;
    let closed = false;
    let segmentIndex = 0;
    let totalFrames16k = 0;
    let buffered = new Int16Array(segmentFrames);
    let bufferedOffset = 0;

    // Fractional resampler state across blocks when inputRate !== 16000.
    const step = inputRate / AUDIO_SAMPLE_RATE;
    let sourceCursor = 0;

    const emitBuffer = (count: number) => {
      if (count <= 0) return;
      const samples = buffered.subarray(0, count);
      const wavBytes = encodeWav16kMonoPcm16(samples);
      const chunk: WavAudioChunk = {
        index: segmentIndex++,
        frames: count,
        durationMs: Math.max(1, Math.round((count / AUDIO_SAMPLE_RATE) * 1000)),
        wavBytes,
        base64: bytesToBase64(wavBytes),
      };
      buffered = new Int16Array(segmentFrames);
      bufferedOffset = 0;
      options.onSegment(chunk);
    };

    const pushMonoSamples = (mono: Float32Array) => {
      if (closed || mono.length === 0) return;
      if (paused) {
        options.onLevel?.(0);
        return;
      }

      let sumSq = 0;
      for (let i = 0; i < mono.length; i++) {
        const s = mono[i]!;
        sumSq += s * s;
      }
      const rms = Math.sqrt(sumSq / mono.length);
      options.onLevel?.(Math.min(1, rms * 3.2));

      if (inputRate === AUDIO_SAMPLE_RATE) {
        for (let i = 0; i < mono.length; i++) {
          buffered[bufferedOffset++] = floatToPcm16Sample(mono[i]!);
          totalFrames16k++;
          if (bufferedOffset >= segmentFrames) {
            emitBuffer(bufferedOffset);
          }
        }
      } else {
        while (sourceCursor < mono.length) {
          const idx = Math.floor(sourceCursor);
          const end = Math.min(mono.length, Math.max(idx + 1, Math.ceil(sourceCursor + step)));
          let acc = 0;
          let cnt = 0;
          for (let j = idx; j < end; j++) {
            acc += mono[j]!;
            cnt++;
          }
          const sample = cnt > 0 ? acc / cnt : mono[idx]!;
          buffered[bufferedOffset++] = floatToPcm16Sample(sample);
          totalFrames16k++;
          if (bufferedOffset >= segmentFrames) {
            emitBuffer(bufferedOffset);
          }
          sourceCursor += step;
        }
        sourceCursor -= mono.length;
        if (sourceCursor < 0) sourceCursor = 0;
      }

      options.onElapsedMs?.(Math.round((totalFrames16k / AUDIO_SAMPLE_RATE) * 1000));
    };

    silentGain = activeCtx.createGain();
    silentGain.gain.value = 0;
    silentGain.connect(activeCtx.destination);

    try {
      if (activeCtx.audioWorklet && typeof AudioWorkletNode === "function") {
        const blob = new Blob([WORKLET_PROCESSOR_SOURCE], { type: "application/javascript" });
        const url = URL.createObjectURL(blob);
        try {
          await activeCtx.audioWorklet.addModule(url);
        } finally {
          URL.revokeObjectURL(url);
        }
        workletNode = new AudioWorkletNode(activeCtx, "screen-audio-capture");
        workletNode.port.onmessage = (event: MessageEvent<Float32Array>) => {
          if (event.data instanceof Float32Array) {
            pushMonoSamples(event.data);
          }
        };
        activeSource.connect(workletNode);
        workletNode.connect(silentGain);
      }
    } catch {
      workletNode = null;
    }

    if (!workletNode) {
      // Fallback for restricted WebViews where Blob AudioWorklet modules are blocked.
      fallbackNode = activeCtx.createScriptProcessor(4096, 1, 1);
      fallbackNode.onaudioprocess = (event: AudioProcessingEvent) => {
        const input = event.inputBuffer.getChannelData(0);
        pushMonoSamples(new Float32Array(input));
      };
      activeSource.connect(fallbackNode);
      fallbackNode.connect(silentGain);
    }

    const activeSilentGain = silentGain;
    const teardown = async () => {
      if (closed) return;
      closed = true;
      options.onLevel?.(0);
      try {
        workletNode?.port.close();
        workletNode?.disconnect();
      } catch {
        // ignore
      }
      try {
        fallbackNode?.disconnect();
      } catch {
        // ignore
      }
      try {
        activeSource.disconnect();
        activeSilentGain.disconnect();
      } catch {
        // ignore
      }
      stream.getTracks().forEach((track) => track.stop());
      await activeCtx.close().catch(() => {});
    };

    return {
      startedAtIso,
      pause() {
        paused = true;
        options.onLevel?.(0);
      },
      resume() {
        paused = false;
      },
      async finish() {
        if (!closed) {
          const minTailFrames = segmentIndex === 0 ? 1 : Math.floor(0.15 * AUDIO_SAMPLE_RATE);
          if (bufferedOffset >= minTailFrames) {
            emitBuffer(bufferedOffset);
          }
        }
        await teardown();
        return {
          totalFrames: totalFrames16k,
          durationSeconds: Math.max(0, Math.round(totalFrames16k / AUDIO_SAMPLE_RATE)),
        };
      },
      async abort() {
        bufferedOffset = 0;
        await teardown();
      },
    };
  } catch (setupError) {
    try {
      workletNode?.port.close();
      workletNode?.disconnect();
    } catch {
      // ignore
    }
    try {
      fallbackNode?.disconnect();
    } catch {
      // ignore
    }
    try {
      sourceNode?.disconnect();
      silentGain?.disconnect();
    } catch {
      // ignore
    }
    for (const track of stream.getTracks()) {
      try {
        track.stop();
      } catch {
        // ignore
      }
    }
    if (audioCtx) {
      await audioCtx.close().catch(() => {});
    }
    throw setupError;
  }
}
