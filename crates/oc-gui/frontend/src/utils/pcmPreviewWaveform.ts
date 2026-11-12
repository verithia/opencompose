/** Measure the same PCM16 WAV bytes that the transcript player receives. */
export function pcmPreviewWaveform(base64: string) {
  // The producer caps its base64 audio payload at 2 MiB.
  if (base64.length > 2 * 1024 * 1024) return null;
  let bytes: string;
  try {
    bytes = atob(base64);
  } catch {
    return null;
  }
  if (bytes.slice(0, 4) !== "RIFF" || bytes.slice(8, 12) !== "WAVE") return null;
  const view = new DataView(Uint8Array.from(bytes, (byte) => byte.charCodeAt(0)).buffer);
  let channels = 0;
  let rate = 0;
  for (let offset = 12; offset + 8 <= view.byteLength;) {
    const type = bytes.slice(offset, offset + 4);
    const length = view.getUint32(offset + 4, true);
    const start = offset + 8;
    if (start + length > view.byteLength) return null;
    if (type === "fmt ") {
      if (length < 16 || view.getUint16(start, true) !== 1 || view.getUint16(start + 14, true) !== 16) return null;
      channels = view.getUint16(start + 2, true);
      rate = view.getUint32(start + 4, true);
    }
    if (type === "data") {
      if ((channels !== 1 && channels !== 2) || !rate) return null;
      const frameBytes = channels * 2;
      const frames = Math.floor(length / frameBytes);
      if (!frames) return null;
      const columns = Math.min(256, frames);
      const paths: string[] = [];
      for (let column = 0; column < columns; column++) {
        const first = Math.floor(column * frames / columns);
        const end = Math.floor((column + 1) * frames / columns);
        let min = 0;
        let max = 0;
        for (let index = start + first * frameBytes; index < start + end * frameBytes; index += 2) {
          const sample = view.getInt16(index, true) / 32768;
          min = Math.min(min, sample);
          max = Math.max(max, sample);
        }
        paths.push(`M${column + 0.5},${(-max).toFixed(3)}V${(-min).toFixed(3)}`);
      }
      return { columns, path: paths.join(""), duration: frames / rate };
    }
    offset = start + length + (length % 2);
  }
  return null;
}
