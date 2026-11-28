// argv: start time (ms epoch), lock dir, count. Claims lock 0..count-1 and prints the ones won,
// then stays alive (its locks stay live) until stdin closes.
import { join } from "node:path";
import { claim } from "../harness.mjs";

const [start, dir, count] = process.argv.slice(2);
while (Date.now() < Number(start)) {}
const won = [];
for (let i = 0; i < Number(count); i++) if (claim(join(dir, String(i)))) won.push(i);
console.log(JSON.stringify(won));
process.stdin.resume();
process.stdin.on("end", () => process.exit(0));
