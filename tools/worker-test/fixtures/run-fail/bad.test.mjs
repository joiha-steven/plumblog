import assert from "node:assert/strict";
import { test } from "node:test";
test("the failing one", () => assert.equal(1, 2, "marker-expected-one-got-two"));
