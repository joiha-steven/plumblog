// The pure decisions of the image encoder (web/admin/src/encode/plan.ts): which widths, the
// size limits, the timeout, how to tell what a browser really wrote, and EXIF orientation.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  MAX_PIXELS,
  TIMEOUT_MS,
  WIDTHS,
  deadline,
  exifOrientation,
  heightFor,
  imageSize,
  orientedSize,
  pickWidths,
  refuse,
  sniff,
} from "../../web/admin/src/encode/plan.ts";

test("the sizes are 480, 960, 1600 and 2400", () => {
  assert.deepEqual([...WIDTHS], [480, 960, 1600, 2400]);
});

test("widths: never upscaled, equal to the original is allowed", () => {
  assert.deepEqual(pickWidths(4000), [480, 960, 1600, 2400]);
  assert.deepEqual(pickWidths(2400), [480, 960, 1600, 2400]);
  assert.deepEqual(pickWidths(2399), [480, 960, 1600]);
  assert.deepEqual(pickWidths(1000), [480, 960]);
  assert.deepEqual(pickWidths(480), [480]);
  assert.deepEqual(pickWidths(481), [480]);
});

test("widths: a picture narrower than the smallest size gets its own width, and nothing is made from nothing", () => {
  assert.deepEqual(pickWidths(300), [300]);
  assert.deepEqual(pickWidths(1), [1]);
  assert.deepEqual(pickWidths(300.9), [300]);
  assert.deepEqual(pickWidths(0), []);
  assert.deepEqual(pickWidths(-5), []);
  assert.deepEqual(pickWidths(NaN), []);
});

test("widths: a caller's list is sorted, deduplicated and still never upscaled", () => {
  assert.deepEqual(pickWidths(1000, [960, 480, 480, 2000]), [480, 960]);
});

test("height follows the aspect ratio and is at least 1", () => {
  assert.equal(heightFor(4000, 3000, 480), 360);
  assert.equal(heightFor(4000, 3000, 2400), 1800);
  assert.equal(heightFor(1000, 7, 480), 3);
  assert.equal(heightFor(10000, 1, 480), 1);
});

test("orientation: 5 to 8 turn the picture a quarter", () => {
  for (const o of [1, 2, 3, 4]) assert.deepEqual(orientedSize(600, 1000, o), { width: 600, height: 1000 });
  for (const o of [5, 6, 7, 8]) assert.deepEqual(orientedSize(600, 1000, o), { width: 1000, height: 600 });
});

/** A JPEG skeleton with an EXIF block holding one orientation entry (byte order `le` or big). */
function jpegWithOrientation(value, le = false) {
  const u16 = (n) => (le ? [n & 255, n >> 8] : [n >> 8, n & 255]);
  const u32 = (n) => (le ? [n & 255, (n >> 8) & 255, 0, 0] : [0, 0, n >> 8, n & 255]);
  const tiff = [...(le ? [0x49, 0x49, 0x2a, 0] : [0x4d, 0x4d, 0, 0x2a]), ...u32(8), ...u16(1), ...u16(0x0112), ...u16(3), ...u32(1), ...u16(value), 0, 0, 0, 0, 0, 0];
  const body = [0x45, 0x78, 0x69, 0x66, 0, 0, ...tiff];
  return new Uint8Array([0xff, 0xd8, 0xff, 0xe1, ...[(body.length + 2) >> 8, (body.length + 2) & 255], ...body, 0xff, 0xda, 0, 2, 0, 0]);
}

test("orientation is read from EXIF in both byte orders, 1 when absent or broken", () => {
  for (const le of [false, true]) for (let v = 1; v <= 8; v++) assert.equal(exifOrientation(jpegWithOrientation(v, le)), v, `${le} ${v}`);
  assert.equal(exifOrientation(jpegWithOrientation(9)), 1, "out of range");
  assert.equal(exifOrientation(new Uint8Array([0xff, 0xd8, 0xff, 0xda, 0, 2])), 1, "no EXIF");
  assert.equal(exifOrientation(jpegWithOrientation(6).subarray(0, 14)), 1, "truncated");
  assert.equal(exifOrientation(new Uint8Array([1, 2, 3])), 1, "not a JPEG");
  assert.equal(exifOrientation(new Uint8Array()), 1, "empty");
});

test("limits: too many pixels is refused, the edge is not", () => {
  assert.equal(refuse(8000, 8000), undefined); // 64 MP exactly
  assert.equal(refuse(8000, 8001), "too_large");
  assert.equal(refuse(100, 100, 9999), "too_large");
  assert.equal(refuse(100, 100, 10000), undefined);
  assert.equal(MAX_PIXELS, 64_000_000);
});

test("deadline: the run stops at the timeout, not before", () => {
  assert.equal(deadline(0), "continue");
  assert.equal(deadline(TIMEOUT_MS - 1), "continue");
  assert.equal(deadline(TIMEOUT_MS), "timeout");
  assert.equal(deadline(5, 5), "timeout");
  assert.equal(deadline(4, 5), "continue");
});

test("sniff names what the bytes are, so a PNG written for a type the browser cannot write is caught", () => {
  const ascii = (s) => [...s].map((c) => c.charCodeAt(0));
  assert.equal(sniff(new Uint8Array([...ascii("RIFF"), 0, 0, 0, 0, ...ascii("WEBPVP8 ")])), "image/webp");
  assert.equal(sniff(new Uint8Array([0, 0, 0, 28, ...ascii("ftypavif"), 0, 0, 0, 0])), "image/avif");
  assert.equal(sniff(new Uint8Array([0, 0, 0, 28, ...ascii("ftypmif1"), 0, 0, 0, 0])), undefined, "HEIF is not AVIF");
  assert.equal(sniff(new Uint8Array([0x89, ...ascii("PNG\r\n"), 0x1a, 0x0a])), "image/png");
  assert.equal(sniff(new Uint8Array([0xff, 0xd8, 0xff, 0xe0])), "image/jpeg");
  assert.equal(sniff(new Uint8Array([1, 2, 3])), undefined);
  assert.equal(sniff(new Uint8Array()), undefined);
});

const ascii = (s) => [...s].map((c) => c.charCodeAt(0));
const be32 = (n) => [(n / 2 ** 24) & 255, (n >> 16) & 255, (n >> 8) & 255, n & 255];
const be16 = (n) => [n >> 8, n & 255];
const le16 = (n) => [n & 255, n >> 8];
const le24 = (n) => [n & 255, (n >> 8) & 255, (n >> 16) & 255];

test("imageSize reads the declared size from each format's header, without decoding", () => {
  const png = [0x89, ...ascii("PNG\r\n"), 0x1a, 0x0a, ...be32(13), ...ascii("IHDR"), ...be32(40000), ...be32(30000), 8, 0, 0, 0, 0];
  assert.deepEqual(imageSize(new Uint8Array(png)), { width: 40000, height: 30000 });
  assert.deepEqual(imageSize(new Uint8Array([...ascii("GIF89a"), ...le16(640), ...le16(480), 0, 0, 0])), { width: 640, height: 480 });
  // JPEG: an APP0 segment and a comment first, then a baseline frame (height before width).
  const jpeg = [0xff, 0xd8, 0xff, 0xe0, ...be16(16), ...Array(14).fill(0), 0xff, 0xfe, ...be16(4), 0, 0, 0xff, 0xc0, ...be16(17), 8, ...be16(3000), ...be16(4000), 3];
  assert.deepEqual(imageSize(new Uint8Array(jpeg)), { width: 4000, height: 3000 });
  const riff = [...ascii("RIFF"), 0, 0, 0, 0, ...ascii("WEBP")];
  assert.deepEqual(imageSize(new Uint8Array([...riff, ...ascii("VP8X"), 10, 0, 0, 0, 0, 0, 0, 0, ...le24(1999), ...le24(999)])), { width: 2000, height: 1000 });
  const bits = 799 | (599 << 14);
  assert.deepEqual(imageSize(new Uint8Array([...riff, ...ascii("VP8L"), 5, 0, 0, 0, 0x2f, bits & 255, (bits >> 8) & 255, (bits >> 16) & 255, (bits >>> 24) & 255, 0, 0, 0, 0, 0, 0, 0])), { width: 800, height: 600 });
  assert.deepEqual(imageSize(new Uint8Array([...riff, ...ascii("VP8 "), 10, 0, 0, 0, 0, 0, 0, 0x9d, 0x01, 0x2a, ...le16(321), ...le16(123)])), { width: 321, height: 123 });
  // AVIF: several images; the largest counts.
  const ispe = (w, h) => [...be32(20), ...ascii("ispe"), 0, 0, 0, 0, ...be32(w), ...be32(h)];
  assert.deepEqual(imageSize(new Uint8Array([0, 0, 0, 24, ...ascii("ftypavif"), 0, 0, 0, 0, ...ascii("avifmif1"), ...ispe(160, 120), ...ispe(8000, 6000)])), { width: 8000, height: 6000 });
});

test("imageSize says nothing it does not know: other types, junk, a header cut short", () => {
  assert.equal(imageSize(new Uint8Array()), undefined);
  assert.equal(imageSize(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24])), undefined);
  assert.equal(imageSize(new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0, 16, 0, 0])), undefined, "JPEG cut before its frame");
  assert.equal(imageSize(new Uint8Array([0xff, 0xd8, 0xff, 0xda, 0, 4, 0, 0])), undefined, "scan before any frame");
  assert.equal(imageSize(new Uint8Array([0x89, ...ascii("PNG"), 0, 0, 0, 0])), undefined, "PNG cut before IHDR");
  assert.equal(refuse(40000, 40000), "too_large");
});
