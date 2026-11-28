// Checks a JSON value against a generated shape table (web/admin/src/api/model.gen.ts `SHAPES`;
// descriptor grammar is documented there). Shared by admin-test (fixtures, mock) and
// worker-test (the real API), so one table answers for all three.

/**
 * Problems of `value` against `shapes[name]`; empty when it matches. `codeLists` maps a list
 * constant's name to its values: `{POST_STATUSES: ["draft", ...]}`. Keys must be exactly the
 * shape's, in its order.
 */
export function checkShape(shapes, codeLists, name, value, path = name) {
  const shape = shapes[name];
  if (shape === undefined) return [`${path}: unknown shape ${name}`];
  if (value === null || typeof value !== "object" || Array.isArray(value)) return [`${path}: expected an object`];
  const problems = [];
  const want = Object.keys(shape);
  const have = Object.keys(value);
  for (const k of want) if (!have.includes(k)) problems.push(`${path}.${k}: missing key`);
  for (const k of have) if (!want.includes(k)) problems.push(`${path}.${k}: extra key`);
  const common = want.filter((k) => have.includes(k));
  if (!problems.length && want.join() !== have.join()) problems.push(`${path}: key order differs (want ${want.join(",")}; got ${have.join(",")})`);
  for (const k of common) problems.push(...checkValue(shapes, codeLists, shape[k], value[k], `${path}.${k}`));
  return problems;
}

function checkValue(shapes, codeLists, descriptor, value, path) {
  let d = descriptor;
  if (d.endsWith("|null")) {
    if (value === null) return [];
    d = d.slice(0, -5);
  }
  if (d.endsWith("[]")) {
    if (!Array.isArray(value)) return [`${path}: expected a list, got ${describe(value)}`];
    return value.flatMap((v, i) => checkValue(shapes, codeLists, d.slice(0, -2), v, `${path}[${i}]`));
  }
  if (d === "string") return typeof value === "string" ? [] : [`${path}: expected string, got ${describe(value)}`];
  if (d === "integer") return Number.isInteger(value) ? [] : [`${path}: expected integer, got ${describe(value)}`];
  if (d === "number") return Number.isFinite(value) ? [] : [`${path}: expected number, got ${describe(value)}`];
  // Any JSON value but null (a nullable one says `json|null`).
  if (d === "json") return value === null || value === undefined ? [`${path}: expected a value, got ${describe(value)}`] : [];
  if (d === "boolean") return typeof value === "boolean" ? [] : [`${path}: expected boolean, got ${describe(value)}`];
  if (d.startsWith("code:")) {
    const list = codeLists[d.slice(5)];
    if (list === undefined) return [`${path}: unknown code list ${d.slice(5)}`];
    return list.includes(value) ? [] : [`${path}: ${JSON.stringify(value)} is not in ${d.slice(5)}`];
  }
  if (d.startsWith("shape:")) return checkShape(shapes, codeLists, d.slice(6), value, path);
  return [`${path}: unknown descriptor ${descriptor}`];
}

const describe = (v) => (v === null ? "null" : Array.isArray(v) ? "list" : typeof v);
