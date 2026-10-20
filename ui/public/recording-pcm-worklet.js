/* Produce 16 kHz PCM even when the browser keeps a 44.1/48 kHz device clock. */
class CadencePcmTap extends AudioWorkletProcessor {
  constructor() {
    super();
    this.samples = new Float32Array(640);
    this.length = 0;
    this.peak = 0;
    this.weight = 0;
    this.sum = 0;
    this.stopped = false;
    this.port.onmessage = ({ data }) => {
      if (data !== "stop" || this.stopped) return;
      this.stopped = true;
      if (this.weight) this.pushSample(this.sum / this.weight);
      if (this.length) this.emit();
      this.port.postMessage({ type: "stopped" });
    };
  }

  emit() {
    const samples = this.length === this.samples.length
      ? this.samples
      : this.samples.slice(0, this.length);
    this.port.postMessage({ type: "pcm", samples, peak: this.peak }, [samples.buffer]);
    this.samples = new Float32Array(640);
    this.length = 0;
    this.peak = 0;
  }

  pushSample(sample) {
    this.samples[this.length++] = sample;
    this.peak = Math.max(this.peak, Math.abs(sample));
    if (this.length === this.samples.length) this.emit();
  }

  process(inputs) {
    if (this.stopped) return false;
    const mono = inputs[0]?.[0];
    if (!mono) return true;
    for (const sample of mono) {
      if (sampleRate === 16000) { this.pushSample(sample); continue; }
      // Integrate each input sample over the output clock. Integer clock
      // weights avoid drift at fractional ratios such as 44,100 / 16,000.
      let remaining = 16000;
      while (remaining) {
        const take = Math.min(remaining, sampleRate - this.weight);
        this.sum += sample * take;
        this.weight += take;
        remaining -= take;
        if (this.weight === sampleRate) {
          this.pushSample(this.sum / sampleRate);
          this.sum = 0;
          this.weight = 0;
        }
      }
    }
    return true;
  }
}
registerProcessor("cadence-pcm-tap", CadencePcmTap);
