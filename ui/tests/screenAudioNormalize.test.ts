import {
  AUDIO_SAMPLE_RATE,
  AudioNormalizeError,
  deriveAudioHeldAt,
  encodeWav16kMonoPcm16,
  floatToPcm16Sample,
  MAX_AUDIO_UPLOAD_BYTES,
  normalizeAudioFile,
  normalizeChannelDataToWavChunks,
  resampleMonoFallback,
  sniffAudioContainer,
  startLiveMicrophoneCapture,
  trySliceCanonicalWav16k,
} from "../src/features/workspace-apps/screen/audioNormalize";

function check(condition: unknown, label: string): void {
  if (!condition) throw new Error(`screenAudioNormalize test failed: ${label}`);
}

function asciiBytes(text: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) out[i] = text.charCodeAt(i);
  return out;
}

// 1. Mobile & desktop container magic-byte sniffing.
check(sniffAudioContainer(asciiBytes("RIFF\x24\x00\x00\x00WAVEfmt ")) === "wav", "WAV container sniffs");
check(sniffAudioContainer(new Uint8Array([0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70, 0x4d, 0x34, 0x41, 0x20])) === "mp4", "iPhone Voice Memos .m4a ftyp sniffs");
check(sniffAudioContainer(new Uint8Array([0x00, 0x00, 0x00, 0x18, 0x66, 0x74, 0x79, 0x70, 0x33, 0x67, 0x70, 0x35])) === "mp4", "Android .3gp ftyp sniffs");
check(sniffAudioContainer(asciiBytes("OggS\x00\x02\x00\x00")) === "ogg", "WhatsApp/Telegram .opus/.ogg sniffs");
check(sniffAudioContainer(new Uint8Array([0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81])) === "webm", "WebM/Matroska audio sniffs");
check(sniffAudioContainer(asciiBytes("fLaC\x00\x00\x00\x22")) === "flac", "FLAC sniffs");
check(sniffAudioContainer(asciiBytes("#!AMR\n\x3c\x55")) === "amr", "AMR voice note sniffs");
check(sniffAudioContainer(asciiBytes("ID3\x04\x00\x00\x00\x00")) === "mpeg", "ID3v2 MP3 sniffs");
check(sniffAudioContainer(new Uint8Array([0xff, 0xfb, 0x90, 0x64])) === "mpeg", "MPEG-1 Layer 3 frame sync sniffs");
check(sniffAudioContainer(new Uint8Array([0xff, 0xf1, 0x50, 0x80])) === "mpeg", "ADTS .aac frame sync sniffs");

// Non-audio / reserved headers must fail closed.
check(sniffAudioContainer(asciiBytes("%PDF-1.7\n")) === null, "PDF rejected");
check(sniffAudioContainer(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) === null, "PNG rejected");
check(sniffAudioContainer(asciiBytes("RIFF\x24\x00\x00\x00AVI ")) === null, "RIFF AVI rejected");
check(sniffAudioContainer(new Uint8Array([0xff, 0xe8, 0x00, 0x00])) === null, "Reserved MPEG version 01 rejected");
check(sniffAudioContainer(new Uint8Array([0x00, 0x01, 0x02])) === null, "Truncated <4 byte header rejected");

// 2. Canonical 44-byte 16 kHz mono PCM16 WAV encoder.
const testSamples = new Float32Array([-1.5, -0.5, 0, 0.5, 1.5, Number.NaN]);
const encodedWav = encodeWav16kMonoPcm16(testSamples);
check(encodedWav.byteLength === 44 + testSamples.length * 2, "44-byte canonical WAV header + PCM16 payload");
const wavView = new DataView(encodedWav.buffer, encodedWav.byteOffset, encodedWav.byteLength);
check(wavView.getUint16(20, true) === 1, "AudioFormat = 1 (PCM)");
check(wavView.getUint16(22, true) === 1, "NumChannels = 1 (mono)");
check(wavView.getUint32(24, true) === AUDIO_SAMPLE_RATE, "SampleRate = 16000");
check(wavView.getUint16(34, true) === 16, "BitsPerSample = 16");
check(wavView.getInt16(44, true) === -32768, "Negative clipping clamps to -32768");
check(wavView.getInt16(44 + 8, true) === 32767, "Positive clipping clamps to +32767");
check(wavView.getInt16(44 + 10, true) === 0, "NaN sample normalizes to 0");
check(floatToPcm16Sample(0) === 0, "Zero sample stays 0");

// 3. Fast-path canonical 16 kHz WAV chunk slicing (10s -> 4s + 4s + 2s).
const tenSecSamples = new Int16Array(10 * AUDIO_SAMPLE_RATE);
for (let i = 0; i < tenSecSamples.length; i++) tenSecSamples[i] = (i % 2000) - 1000;
const tenSecWav = encodeWav16kMonoPcm16(tenSecSamples);
const sliced = trySliceCanonicalWav16k(tenSecWav, "2026-11-13T03:00:00.000Z", 4);
check(sliced !== null, "Canonical 16 kHz mono PCM16 WAV takes fast slice path");
check(sliced!.chunks.length === 3, "10s WAV at 4s window slices into 3 chunks");
check(sliced!.chunks[0]!.frames === 4 * AUDIO_SAMPLE_RATE, "First chunk has 4s frames");
check(sliced!.chunks[1]!.frames === 4 * AUDIO_SAMPLE_RATE, "Second chunk has 4s frames");
check(sliced!.chunks[2]!.frames === 2 * AUDIO_SAMPLE_RATE, "Final chunk has 2s tail frames");
check(sliced!.durationSeconds === 10, "Duration rounds to 10s");

// Non-16kHz WAV falls through (returns null) so WebAudio resamples it.
const wav48k = new Uint8Array(tenSecWav);
new DataView(wav48k.buffer).setUint32(24, 48_000, true);
check(trySliceCanonicalWav16k(wav48k, "2026-11-13T03:00:00.000Z") === null, "48 kHz WAV bypasses 16 kHz fast slicer");

// 4. Held-at timestamp bounds.
const refNow = Date.parse("2026-11-13T03:00:00.000Z");
const fileTime = Date.parse("2026-11-12T10:15:00.000Z");
check(deriveAudioHeldAt(fileTime, refNow) === "2026-11-12T10:15:00.000Z", "Valid file lastModified preserved");
check(deriveAudioHeldAt(0, refNow) === "2026-11-13T03:00:00.000Z", "Epoch 0 falls back to now");

// 5. Async stereo 48 kHz -> 16 kHz mono windowed normalization & file validation.
void (async () => {
  const rate48k = 48_000;
  const frames3s = 3 * rate48k;
  const left = new Float32Array(frames3s).fill(0.5);
  const right = new Float32Array(frames3s).fill(-0.1);
  const progress: string[] = [];
  const normalized = await normalizeChannelDataToWavChunks([left, right], rate48k, {
    container: "mp4",
    heldAt: "2026-11-13T03:00:00.000Z",
    chunkSeconds: 2,
    resampler: async (mono, srcRate, targetFrames) => resampleMonoFallback(mono, srcRate, targetFrames),
    onChunkNormalized: (done, total) => progress.push(`${done}/${total}`),
  });
  check(normalized.chunks.length === 2, "3s stereo 48 kHz splits into 2 chunks at 2s window");
  check(normalized.totalFrames === 3 * AUDIO_SAMPLE_RATE, "Resampled total frames match 3s at 16 kHz");
  check(JSON.stringify(progress) === '["1/2","2/2"]', "Chunk normalization progress reported");
  const chunk0View = new DataView(normalized.chunks[0]!.wavBytes.buffer);
  const midSample = chunk0View.getInt16(44 + 200, true);
  // (0.5 + -0.1) / 2 = 0.2 -> ~6553 in Int16
  check(Math.abs(midSample - 6553) <= 2, "Stereo mixdown + 48k->16k box-filter preserves amplitude");

  // AbortSignal stops normalization cleanly.
  const aborted = new AbortController();
  aborted.abort();
  let sawAbort = false;
  try {
    await normalizeChannelDataToWavChunks([left], rate48k, { signal: aborted.signal });
  } catch (err) {
    sawAbort = err instanceof DOMException && err.name === "AbortError";
  }
  check(sawAbort, "Aborted signal rejects with AbortError");

  // File-level validation: empty, oversize, unsupported format, and canonical WAV fast-path.
  let emptyCode = "";
  try {
    await normalizeAudioFile(new Blob([]));
  } catch (err) {
    if (err instanceof AudioNormalizeError) emptyCode = err.code;
  }
  check(emptyCode === "empty_audio", "0-byte Blob rejected with empty_audio");

  let oversizeCode = "";
  try {
    const fakeOversize = {
      size: MAX_AUDIO_UPLOAD_BYTES + 1,
      slice: () => new Blob([]),
      arrayBuffer: async () => new ArrayBuffer(0),
    } as unknown as Blob;
    await normalizeAudioFile(fakeOversize);
  } catch (err) {
    if (err instanceof AudioNormalizeError) oversizeCode = err.code;
  }
  check(oversizeCode === "audio_too_large", ">50 MiB file rejected with audio_too_large");

  let badFormatCode = "";
  try {
    await normalizeAudioFile(new Blob([asciiBytes("%PDF-1.7 not audio")]));
  } catch (err) {
    if (err instanceof AudioNormalizeError) badFormatCode = err.code;
  }
  check(badFormatCode === "unsupported_audio_format", "Disguised PDF rejected with unsupported_audio_format");

  const wavBlob = new Blob([tenSecWav], { type: "application/octet-stream" });
  const fromWavBlob = await normalizeAudioFile(wavBlob, { chunkSeconds: 4 });
  check(fromWavBlob.chunks.length === 3 && fromWavBlob.durationSeconds === 10,
    "Canonical WAV Blob normalizes even when MIME type is application/octet-stream");

  // Mock AudioContext to verify .m4a container decode path in Node without browser WebAudio.
  const prevAudioContext = (globalThis as { AudioContext?: unknown }).AudioContext;
  class FakeAudioContext {
    sampleRate = AUDIO_SAMPLE_RATE;
    state = "running";
    async decodeAudioData(_buf: ArrayBuffer) {
      const ch = new Float32Array(2 * AUDIO_SAMPLE_RATE).fill(0.25);
      return {
        numberOfChannels: 1,
        sampleRate: AUDIO_SAMPLE_RATE,
        getChannelData: () => ch,
      };
    }
    async close() {}
  }
  (globalThis as { AudioContext?: unknown }).AudioContext = FakeAudioContext;
  try {
    const m4aHeader = new Uint8Array([0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70, 0x4d, 0x34, 0x41, 0x20, 0, 0, 0, 0]);
    const m4aFile = new File([m4aHeader], "voice-memo.m4a", { type: "", lastModified: fileTime });
    const decodedM4a = await normalizeAudioFile(m4aFile);
    check(decodedM4a.container === "mp4" && decodedM4a.durationSeconds === 2 && decodedM4a.heldAt === "2026-11-12T10:15:00.000Z",
      "iPhone .m4a with empty MIME type decodes via AudioContext and preserves lastModified held_at");
  } finally {
    (globalThis as { AudioContext?: unknown }).AudioContext = prevAudioContext;
  }

  // 7. Live microphone capture: Firefox sample-rate mismatch fallback + hardware track cleanup on setup throw.
  const prevNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  try {
    // 7a. Firefox NotSupportedError on 16kHz AudioContext.createMediaStreamSource falls back to default 48kHz context cleanly.
    let trackStopped = 0;
    const closedContexts: number[] = [];
    let scriptProcessorHandler: ((ev: { inputBuffer: { getChannelData: (ch: number) => Float32Array } }) => void) | null = null;
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          async getUserMedia() {
            return {
              getTracks: () => [{ stop: () => { trackStopped++; } }],
            };
          },
        },
      },
    });
    class FirefoxAudioContext {
      sampleRate: number;
      state = "running";
      destination = {};
      constructor(opts?: { sampleRate?: number }) {
        this.sampleRate = opts?.sampleRate ?? 48_000;
      }
      createMediaStreamSource() {
        if (this.sampleRate === AUDIO_SAMPLE_RATE) {
          throw new DOMException(
            "Connecting AudioNodes from AudioContexts with different sample-rate is currently not supported",
            "NotSupportedError",
          );
        }
        return { connect() {}, disconnect() {} };
      }
      createGain() {
        return { gain: { value: 1 }, connect() {}, disconnect() {} };
      }
      createScriptProcessor() {
        const node = {
          onaudioprocess: null as typeof scriptProcessorHandler,
          connect() {},
          disconnect() {},
        };
        Object.defineProperty(node, "onaudioprocess", {
          get: () => scriptProcessorHandler,
          set: (fn) => { scriptProcessorHandler = fn; },
        });
        return node;
      }
      async close() {
        closedContexts.push(this.sampleRate);
      }
    }
    (globalThis as { AudioContext?: unknown }).AudioContext = FirefoxAudioContext;

    const liveChunks: Array<{ frames: number }> = [];
    const live = await startLiveMicrophoneCapture({
      segmentSeconds: 1,
      onSegment: (chunk) => liveChunks.push({ frames: chunk.frames }),
    });
    check(closedContexts.length === 1 && closedContexts[0] === 16_000, "mismatched 16kHz AudioContext closed immediately on Firefox fallback");
    check(trackStopped === 0, "mic track stays live after Firefox fallback");
    const block48k = new Float32Array(48_000).fill(0.2);
    const invokeProcessor = scriptProcessorHandler as ((ev: {
      inputBuffer: { getChannelData: (ch: number) => Float32Array };
    }) => void) | null;
    invokeProcessor?.({ inputBuffer: { getChannelData: () => block48k } });
    const fin = await live.finish();
    check(liveChunks.length === 1 && liveChunks[0]!.frames === 16_000 && fin.durationSeconds === 1,
      "48kHz fallback resamples into canonical 16kHz WAV segment");
    check(trackStopped === 1 && closedContexts.includes(48_000), "finish() stops mic track and closes fallback AudioContext");

    // 7b. Post-getUserMedia setup throw stops all MediaStream tracks and closes AudioContext (zero hardware mic leak).
    let leakedTrackStops = 0;
    let leakedCtxCloses = 0;
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          async getUserMedia() {
            return {
              getTracks: () => [{ stop: () => { leakedTrackStops++; } }],
            };
          },
        },
      },
    });
    class ThrowingSetupAudioContext {
      sampleRate = 48_000;
      state = "running";
      destination = {};
      createMediaStreamSource() {
        return { connect() {}, disconnect() {} };
      }
      createGain() {
        return { gain: { value: 1 }, connect() {}, disconnect() {} };
      }
      createScriptProcessor(): never {
        throw new Error("script_processor_unavailable");
      }
      async close() {
        leakedCtxCloses++;
      }
    }
    (globalThis as { AudioContext?: unknown }).AudioContext = ThrowingSetupAudioContext;
    let threwSetup = false;
    try {
      await startLiveMicrophoneCapture({ onSegment: () => {} });
    } catch {
      threwSetup = true;
    }
    check(threwSetup && leakedTrackStops === 1 && leakedCtxCloses === 1,
      "post-getUserMedia throw stops MediaStream tracks and closes AudioContext");
  } finally {
    (globalThis as { AudioContext?: unknown }).AudioContext = prevAudioContext;
    if (prevNavigator) {
      Object.defineProperty(globalThis, "navigator", prevNavigator);
    } else {
      delete (globalThis as { navigator?: unknown }).navigator;
    }
  }

  console.log("screenAudioNormalize checks pass");
})();
