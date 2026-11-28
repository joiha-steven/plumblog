// The pure decisions of the image encoder (no DOM, no I/O), so tools/admin-test can pin them.

/** Variant widths, in pixels (docs/ARCHITECTURE.md, Images). */
export const WIDTHS: readonly number[] = [480, 960, 1600, 2400];

/** Most pixels the encoder takes before it refuses (decoded RGBA is 4 bytes a pixel). */
export const MAX_PIXELS = 64_000_000;

/** Whole run, wall clock, before the caller falls back to the original alone. */
export const TIMEOUT_MS = 90_000;

export type Mime = "image/avif" | "image/webp";

/** Why something was not made. Codes, not text: the caller maps them to locale keys. */
export type SkipReason =
  | "decode_failed" // the browser could not read the file as an image
  | "too_large" // more pixels than MAX_PIXELS
  | "encoder_unavailable" // the AVIF encoder did not load
  | "native_unsupported" // WebP: this browser's canvas cannot write it
  | "encode_failed" // an encoder threw on one image
  | "timeout" // TIMEOUT_MS passed
  | "worker_failed"; // the worker died or never started

/**
 * Widths to make for an image `width` pixels wide: the listed ones that do not exceed it
 * (equal is fine, never upscaled). An image narrower than all of them gets its own width, so
 * even a small picture gets a modern-format copy.
 */
export function pickWidths(width: number, widths: readonly number[] = WIDTHS): number[] {
  const own = Math.floor(width);
  if (!(own >= 1)) return [];
  const fit = [...new Set(widths)].filter((w) => w >= 1 && w <= own).sort((a, b) => a - b);
  return fit.length > 0 ? fit : [own];
}

/** Height for `width` at the original's aspect ratio, at least 1. */
export function heightFor(origWidth: number, origHeight: number, width: number): number {
  return Math.max(1, Math.round((origHeight * width) / origWidth));
}

/** The reason an image of this size is refused outright, if any. */
export function refuse(width: number, height: number, maxPixels: number = MAX_PIXELS): SkipReason | undefined {
  return width * height > maxPixels ? "too_large" : undefined;
}

/** What happens to a run `elapsed` ms in: keep going, or stop and fall back. */
export function deadline(elapsed: number, timeoutMs: number = TIMEOUT_MS): "continue" | "timeout" {
  return elapsed >= timeoutMs ? "timeout" : "continue";
}

/**
 * The image type the bytes really are, by magic number. Browsers silently write PNG when
 * asked for a type they cannot encode, so the blob's own `type` is never trusted.
 */
export function sniff(bytes: Uint8Array): Mime | "image/png" | "image/jpeg" | undefined {
  const at = (i: number, s: string) => [...s].every((c, k) => bytes[i + k] === c.charCodeAt(0));
  if (bytes.length >= 12 && at(0, "RIFF") && at(8, "WEBP")) return "image/webp";
  if (bytes.length >= 12 && at(4, "ftyp") && (at(8, "avif") || at(8, "avis"))) return "image/avif";
  if (bytes.length >= 8 && bytes[0] === 0x89 && at(1, "PNG")) return "image/png";
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return "image/jpeg";
  return undefined;
}

/**
 * The EXIF orientation (1-8) of a JPEG, 1 when absent or unreadable. The browser applies it
 * itself (`imageOrientation: "from-image"`); this is for callers and tests that must know the
 * displayed size without decoding.
 */
export function exifOrientation(bytes: Uint8Array): number {
  if (sniff(bytes) !== "image/jpeg") return 1;
  let p = 2;
  while (p + 4 <= bytes.length && bytes[p] === 0xff) {
    const marker = bytes[p + 1] ?? 0;
    const len = ((bytes[p + 2] ?? 0) << 8) | (bytes[p + 3] ?? 0);
    if (marker === 0xda || len < 2) break; // start of scan: no metadata after this
    if (marker === 0xe1 && String.fromCharCode(...bytes.subarray(p + 4, p + 10)) === "Exif\0\0") {
      const tiff = p + 10; // TIFF header
      const le = bytes[tiff] === 0x49;
      const dv = new DataView(bytes.buffer, bytes.byteOffset + tiff, Math.max(0, Math.min(p + 2 + len, bytes.length) - tiff));
      try {
        const ifd = dv.getUint32(4, le);
        for (let i = 0, n = dv.getUint16(ifd, le); i < n; i++) {
          const e = ifd + 2 + i * 12;
          if (dv.getUint16(e, le) === 0x0112) {
            const v = dv.getUint16(e + 8, le);
            return v >= 1 && v <= 8 ? v : 1;
          }
        }
      } catch {
        return 1; // a truncated block: treat as upright, the browser decides the rest
      }
      return 1;
    }
    p += 2 + len;
  }
  return 1;
}

/** Displayed size after an EXIF orientation: 5-8 turn the image a quarter. */
export function orientedSize(width: number, height: number, orientation: number): { width: number; height: number } {
  return orientation >= 5 && orientation <= 8 ? { width: height, height: width } : { width, height };
}

/** How many leading bytes of a file `imageSize` needs: the server reads the same for AVIF. */
export const HEADER_BYTES = 256 * 1024;

const u16be = (b: Uint8Array, o: number) => ((b[o] ?? 0) << 8) | (b[o + 1] ?? 0);
const u16le = (b: Uint8Array, o: number) => (b[o] ?? 0) | ((b[o + 1] ?? 0) << 8);
const u24le = (b: Uint8Array, o: number) => u16le(b, o) | ((b[o + 2] ?? 0) << 16);
const u32be = (b: Uint8Array, o: number) => u16be(b, o) * 65536 + u16be(b, o + 2);
const ascii = (b: Uint8Array, o: number, n: number) => String.fromCharCode(...b.subarray(o, o + n));

/**
 * The pixel size a file declares in its header (PNG, GIF, JPEG, WebP, AVIF), read without
 * decoding, so a picture of absurd size is refused before a decoder allocates for it. `bytes`
 * is the start of the file (HEADER_BYTES is enough). Undefined when the header does not say.
 * AVIF may declare several images; the largest counts. Orientation is not applied.
 */
export function imageSize(bytes: Uint8Array): { width: number; height: number } | undefined {
  const some = (w: number, h: number) => (w > 0 && h > 0 ? { width: w, height: h } : undefined);
  if (bytes.length >= 24 && bytes[0] === 0x89 && ascii(bytes, 1, 3) === "PNG" && ascii(bytes, 12, 4) === "IHDR") {
    return some(u32be(bytes, 16), u32be(bytes, 20));
  }
  if (bytes.length >= 10 && ascii(bytes, 0, 4) === "GIF8") return some(u16le(bytes, 6), u16le(bytes, 8));
  const kind = sniff(bytes);
  if (kind === "image/jpeg") {
    let p = 2;
    while (p + 4 <= bytes.length) {
      if (bytes[p] !== 0xff) return undefined;
      const marker = bytes[p + 1] ?? 0;
      if (marker === 0xff) {
        p++; // fill byte
        continue;
      }
      // SOF0-15 except DHT (C4), JPG (C8) and DAC (CC) carry the frame size.
      if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
        return some(u16be(bytes, p + 7), u16be(bytes, p + 5));
      }
      if (marker === 0xd9 || marker === 0xda) return undefined;
      if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd8)) {
        p += 2; // no length
        continue;
      }
      p += 2 + u16be(bytes, p + 2);
    }
    return undefined;
  }
  if (kind === "image/webp" && bytes.length >= 30) {
    const chunk = ascii(bytes, 12, 4);
    if (chunk === "VP8 ") return some(u16le(bytes, 26) & 0x3fff, u16le(bytes, 28) & 0x3fff);
    if (chunk === "VP8L") {
      const bits = u16le(bytes, 21) | (u16le(bytes, 23) << 16);
      return some(1 + (bits & 0x3fff), 1 + ((bits >>> 14) & 0x3fff));
    }
    if (chunk === "VP8X") return some(1 + u24le(bytes, 24), 1 + u24le(bytes, 27));
    return undefined;
  }
  if (kind === "image/avif") {
    let best: { width: number; height: number } | undefined;
    const text = new TextDecoder("latin1").decode(bytes);
    for (let at = text.indexOf("ispe"); at >= 0; at = text.indexOf("ispe", at + 4)) {
      const s = some(u32be(bytes, at + 8), u32be(bytes, at + 12));
      if (s && (!best || s.width * s.height > best.width * best.height)) best = s;
    }
    return best;
  }
  return undefined;
}
