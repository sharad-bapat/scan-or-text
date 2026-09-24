// Time the WebAssembly build in Node (V8, the same engine as Chrome) over the file list.
// usage: node tools/wasm-bench.mjs <files.txt> <out.jsonl> [repeat]
import { readFileSync, writeFileSync } from 'node:fs';
import { initSync, classify_json } from '../wasm-pkg/scan_or_text.js';

const [list, out, rep = '9'] = process.argv.slice(2);
initSync({ module: readFileSync(new URL('../wasm-pkg/scan_or_text_bg.wasm', import.meta.url)) });
const lines = [];
for (const file of readFileSync(list, 'utf8').split(/\r?\n/).filter(Boolean)) {
  const bytes = new Uint8Array(readFileSync(file));
  const times = [];
  let json;
  for (let i = 0; i < +rep; i++) {
    const t = performance.now();
    json = classify_json(bytes);
    times.push((performance.now() - t) * 1000);
  }
  times.sort((a, b) => a - b);
  const r = JSON.parse(json);
  lines.push(JSON.stringify({ file, micros: +times[times.length >> 1].toFixed(1), ...r }));
}
writeFileSync(out, lines.join('\n') + '\n');
console.log(`${lines.length} files`);
