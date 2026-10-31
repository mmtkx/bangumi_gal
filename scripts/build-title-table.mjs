#!/usr/bin/env node
/**
 * 生成「罗马音 → 日文原名」对照表。
 *
 * 数据源：manami-project/anime-offline-database（ODbL v1.0 + DbCL v1.0）。
 * 该数据集只有 title 与 synonyms，没有独立的日文名字段，
 * 因此从同义词里挑「含假名」的那一条作为日文原名。
 *
 * 输出：crates/bangumi-sync/data/titles.tsv，按 key 字节序升序（FST 构建要求）。
 * 该 TSV 同时是 ODbL 意义上「衍生数据库」的可获取副本，必须保留在仓库里。
 *
 * 用法：node scripts/build-title-table.mjs [输入 json 路径]
 */
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const input =
  process.argv[2] ??
  path.join(process.env.TEMP ?? '/tmp', 'anime-offline-database-minified.json');
const output = path.join(root, 'crates/bangumi-sync/data/titles.tsv');

if (!fs.existsSync(input)) {
  console.error(`找不到数据集：${input}`);
  console.error('请先从 https://github.com/manami-project/anime-offline-database/releases 下载');
  console.error('anime-offline-database-minified.json 后重跑本脚本。');
  process.exit(1);
}

const raw = JSON.parse(fs.readFileSync(input, 'utf8'));
const entries = raw.data ?? [];
const KANA = /[\u3040-\u309f\u30a0-\u30ff]/;
const LATIN = /^[A-Za-z0-9\s'\-:!?.,&_~+/]+$/;

// 与运行时一致的归一化：小写，只保留字母数字（含假名/汉字）。
function normalizeKey(text) {
  let out = '';
  for (const ch of text.toLowerCase()) {
    if (/[\p{L}\p{N}]/u.test(ch)) out += ch;
  }
  return out;
}

const table = new Map();
let withoutNative = 0;
for (const entry of entries) {
  const titles = [entry.title ?? '', ...(entry.synonyms ?? [])];
  const native = titles.find((title) => title && KANA.test(title));
  if (!native) {
    withoutNative += 1;
    continue;
  }
  for (const title of titles) {
    if (!title || title.length < 3 || !LATIN.test(title)) continue;
    const key = normalizeKey(title);
    if (key && !table.has(key)) table.set(key, native);
  }
}

const keys = [...table.keys()].sort();
fs.mkdirSync(path.dirname(output), { recursive: true });
const lines = keys.map((key) => `${key}\t${table.get(key).replace(/[\t\r\n]/g, ' ')}`);
fs.writeFileSync(output, `${lines.join('\n')}\n`, 'utf8');

const bytes = fs.statSync(output).size;
console.log(`条目总数        ${entries.length}`);
console.log(`无日文名条目    ${withoutNative}`);
console.log(`映射条数        ${keys.length}`);
console.log(`输出            ${output}（${(bytes / 1048576).toFixed(2)} MB）`);
