// Run by harness.test.mjs with a lowered per-test bound.
import { describe, test } from "node:test";
import "../harness.mjs";

describe("suite Z", () => {
  test("this one hangs", () => new Promise(() => setInterval(() => {}, 1000)));
});
