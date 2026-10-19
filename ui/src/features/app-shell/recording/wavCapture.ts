export const TARGET_RATE = 16_000;
export const SEGMENT_SECONDS = 8;
const SEGMENT_FRAMES = TARGET_RATE * SEGMENT_SECONDS;
const MAX_QUEUED_SEGMENTS = 4;

/** Dense sequence and PCM clock persist across pause/resume. */
export interface CaptureTimeline { sequence: number; frames: number }
export interface CaptureCallbacks {
  onLevel: (peak: number, capturedFrames: number) => void;
  onFailure: (message: string) => void;
  onSegment: (wav: Uint8Array, sequence: number, startMs: number, endMs: number) => void;
  queued: () => number;
}
export interface CaptureHandle { stop(): Promise<void> }

export function encodeWav16kMonoPcm16(samples: Float32Array): Uint8Array {
  const buffer = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(buffer);
  const write = (offset: number, text: string) => {
    for (let i = 0; i < text.length; i++) view.setUint8(offset + i, text.charCodeAt(i));
  };
  write(0, "RIFF");
  view.setUint32(4, buffer.byteLength - 8, true);
  write(8, "WAVE");
  write(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, TARGET_RATE, true);
  view.setUint32(28, TARGET_RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  write(36, "data");
  view.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) {
    const value = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(44 + i * 2, value < 0 ? value * 0x8000 : value * 0x7fff, true);
  }
  return new Uint8Array(buffer);
}

/** Called only by the trusted host's explicit Record/Resume action. */
export async function startCapture(timeline: CaptureTimeline, callbacks: CaptureCallbacks): Promise<CaptureHandle> {
  if (!navigator.mediaDevices?.getUserMedia || typeof AudioWorkletNode === "undefined")
    throw new Error("Recording needs a current browser on a secure connection.");
  // Prefer native graph resampling. Some browsers retain the device rate;
  // the worklet uses that actual sample clock to emit dense 16 kHz PCM.
  const context = new AudioContext({ sampleRate: TARGET_RATE });
  let stream: MediaStream | null = null;
  let node: AudioWorkletNode | null = null;
  let source: MediaStreamAudioSourceNode | null = null;
  let sink: GainNode | null = null;
  let stopped = false;
  let failed = false;
  let stopping: Promise<void> | null = null;
  let acknowledgeStop: (() => void) | null = null;
  const blocks: Float32Array[] = [];
  let buffered = 0;

  const cut = (count: number) => {
    const samples = new Float32Array(count);
    let offset = 0;
    while (offset < count) {
      const head = blocks[0];
      const take = Math.min(head.length, count - offset);
      samples.set(head.subarray(0, take), offset);
      if (take === head.length) blocks.shift();
      else blocks[0] = head.subarray(take);
      offset += take;
    }
    buffered -= count;
    const start = timeline.frames;
    timeline.frames += count;
    callbacks.onSegment(encodeWav16kMonoPcm16(samples), timeline.sequence++,
      Math.round(start * 1000 / TARGET_RATE), Math.round(timeline.frames * 1000 / TARGET_RATE));
  };
  const cleanup = async () => {
    stopped = true;
    node?.port.close();
    source?.disconnect();
    node?.disconnect();
    sink?.disconnect();
    stream?.getTracks().forEach((track) => track.stop());
    if (context.state !== "closed") await context.close();
  };
  const fail = (message: string) => {
    if (failed || stopped) return;
    failed = true;
    callbacks.onFailure(message);
  };
  const stop = (): Promise<void> => {
    if (stopping) return stopping;
    stopping = (async () => {
      try {
        if (node !== null && context.state === "running") {
          const port = node.port;
          let timeout: number | undefined;
          try {
            await new Promise<void>((resolve, reject) => {
              acknowledgeStop = resolve;
              timeout = window.setTimeout(() => reject(new Error("The last audio block could not be confirmed. Saved transcript remains available.")), 1500);
              // Ordered acknowledgement follows the final short PCM block.
              port.postMessage("stop");
            });
          } finally {
            window.clearTimeout(timeout);
          }
        } else if (node !== null) {
          throw new Error("Audio capture was interrupted before its final block could be confirmed. Saved transcript remains available.");
        }
      } finally {
        if (buffered > 0) cut(buffered);
        await cleanup();
      }
    })();
    return stopping;
  };

  try {
    await context.resume();
    stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } });
    await context.audioWorklet.addModule(new URL("./pcmWorklet.js?no-inline", import.meta.url).href);
    node = new AudioWorkletNode(context, "cadence-pcm-tap", { channelCount: 1, channelCountMode: "explicit" });
    node.onprocessorerror = () => fail("Audio capture stopped unexpectedly. Retry pending batches before leaving.");
    node.port.onmessage = (event: MessageEvent<{ type: string; samples?: Float32Array; peak?: number }>) => {
      if (event.data.type === "stopped") { acknowledgeStop?.(); return; }
      if (stopped || event.data.type !== "pcm" || !event.data.samples) return;
      blocks.push(event.data.samples);
      buffered += event.data.samples.length;
      callbacks.onLevel(event.data.peak ?? 0, timeline.frames + buffered);
      while (!failed && buffered >= SEGMENT_FRAMES) cut(SEGMENT_FRAMES);
      if (!failed && callbacks.queued() >= MAX_QUEUED_SEGMENTS)
        fail("Uploads are falling behind. Recording stopped; retry the pending batches before leaving.");
    };
    stream.getAudioTracks().forEach((track) => track.addEventListener("ended", () => fail("The microphone disconnected. Saved batches remain available."), { once: true }));
    source = context.createMediaStreamSource(stream);
    sink = context.createGain();
    sink.gain.value = 0;
    // Keep the graph processing without feeding the microphone to speakers.
    source.connect(node).connect(sink).connect(context.destination);
    return { stop };
  } catch (error) {
    await cleanup();
    if (error instanceof DOMException && error.name === "NotAllowedError")
      throw new Error("Microphone access is blocked. Allow this site to use your microphone, then try Record again.");
    if (error instanceof DOMException && error.name === "NotFoundError")
      throw new Error("No microphone was found. Connect one, then try Record again.");
    if (error instanceof DOMException && error.name === "NotReadableError")
      throw new Error("The microphone is unavailable or busy. Check the device and try again.");
    throw error;
  }
}
