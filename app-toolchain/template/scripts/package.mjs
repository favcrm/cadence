import { createHash } from "node:crypto";
import { constants, closeSync, existsSync, fstatSync, lstatSync, mkdirSync, openSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { crc32 } from "node:zlib";

const root = process.argv[2] ? path.resolve(process.argv[2]) : path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const appRoot = path.join(root, "app");
const MAX_FILES = 128;
const MAX_BYTES = 2 * 1024 * 1024;
const MAX_ARCHIVE_BYTES = 8 * 1024 * 1024;
const fail = (message) => { throw new Error(`package: ${message}`); };

// The installed CLI owns the canonical bundle grammar, file caps and validation.
// Inspection checks portable package integrity, not Native or hosted admission.
const checked = spawnSync(process.env.CADENCE_BIN || "cadence", ["app", "inspect", appRoot, "--json"], { encoding: "utf8" });
if (checked.error) fail(`could not run cadence app inspect: ${checked.error.message}`);
if (checked.status !== 0) fail(`cadence app inspect refused this bundle: ${(checked.stderr || checked.stdout).trim()}`);
let report;
try { report = JSON.parse(checked.stdout); } catch { fail("cadence app inspect returned an unreadable receipt"); }
if (report?.schema !== 1 || report.check !== "app-package-inspection/1" || report.validation !== "package-integrity" || report.secret_policy !== "no-host-allowlist" || report.ok !== true ||
    typeof report.digest !== "string" || !/^sha256:[0-9a-f]{64}$/.test(report.digest) || !Array.isArray(report.files) ||
    report.files.length < 1 || report.files.length > MAX_FILES || !Array.isArray(report.secret_warnings))
  fail("cadence app inspect returned an incomplete receipt");
if (report.secret_warnings.length) fail("the checker reported possible secrets; resolve them before packaging");

const expected = new Map();
for (const file of report.files) {
  if (!file || typeof file.path !== "string" || !Number.isSafeInteger(file.bytes) || file.bytes < 0 ||
      file.path.split("/").some((segment) => !segment || segment.startsWith(".") || !/^[\x21-\x7e]+$/.test(segment) || segment.includes("\\")) ||
      expected.has(file.path)) fail("the checker returned an invalid file list");
  expected.set(file.path, file.bytes);
}
const found = [];
function walk(directory, prefix = "") {
  if (lstatSync(directory).isSymbolicLink()) fail(`symlink refused: ${prefix || "app"}`);
  for (const name of readdirSync(directory).sort()) {
    if (!prefix && name === ".git") continue; // A checkout's Git metadata is never app content.
    const relative = prefix ? `${prefix}/${name}` : name;
    const member = path.join(directory, name);
    const info = lstatSync(member);
    if (info.isSymbolicLink()) fail(`symlink refused: ${relative}`);
    if (info.isDirectory()) walk(member, relative);
    else if (info.isFile()) found.push(relative);
    else fail(`not a regular file: ${relative}`);
  }
}
walk(appRoot);
found.sort();
const names = [...expected.keys()].sort();
if (found.length !== names.length || found.some((name, index) => name !== names[index]))
  fail("the app directory changed or contains a file not included in the checked bundle");

const hash = createHash("sha256");
const members = [];
let total = 0;
const length = (size) => { const out = Buffer.alloc(8); out.writeBigUInt64BE(BigInt(size)); return out; };
for (const name of names) {
  const fd = openSync(path.join(appRoot, ...name.split("/")), constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  let data;
  try {
    const info = fstatSync(fd);
    if (!info.isFile() || info.size !== expected.get(name)) fail(`file changed after check: ${name}`);
    data = readFileSync(fd);
  } finally { closeSync(fd); }
  try { new TextDecoder("utf-8", { fatal: true }).decode(data); } catch { fail(`not UTF-8: ${name}`); }
  total += data.length;
  if (total > MAX_BYTES) fail("the app exceeds the 2 MiB bundle limit");
  const encodedName = Buffer.from(name, "utf8");
  hash.update(length(encodedName.length)).update(encodedName).update(length(data.length)).update(data);
  members.push({ name: encodedName, data, crc: crc32(data) >>> 0 });
}
if (`sha256:${hash.digest("hex")}` !== report.digest) fail("the app changed after checking; build and check it again");

// ZIP stored entries: fixed timestamps, stable ordering and no extra fields make identical input byte-identical.
const chunks = [];
const directory = [];
let offset = 0;
for (const member of members) {
  const header = Buffer.alloc(30);
  header.writeUInt32LE(0x04034b50, 0); header.writeUInt16LE(20, 4); header.writeUInt16LE(0x800, 6);
  header.writeUInt16LE(0x21, 12); header.writeUInt32LE(member.crc, 14);
  header.writeUInt32LE(member.data.length, 18); header.writeUInt32LE(member.data.length, 22);
  header.writeUInt16LE(member.name.length, 26);
  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6);
  central.writeUInt16LE(0x800, 8); central.writeUInt16LE(0x21, 14); central.writeUInt32LE(member.crc, 16);
  central.writeUInt32LE(member.data.length, 20); central.writeUInt32LE(member.data.length, 24);
  central.writeUInt16LE(member.name.length, 28); central.writeUInt32LE(offset, 42);
  chunks.push(header, member.name, member.data); directory.push(central, member.name);
  offset += header.length + member.name.length + member.data.length;
}
const central = Buffer.concat(directory);
const end = Buffer.alloc(22);
end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(members.length, 8); end.writeUInt16LE(members.length, 10);
end.writeUInt32LE(central.length, 12); end.writeUInt32LE(offset, 16);
const archive = Buffer.concat([...chunks, central, end]);
const outputDir = path.join(root, "dist");
mkdirSync(outputDir, { recursive: true });
const outputInfo = lstatSync(outputDir);
if (!outputInfo.isDirectory() || outputInfo.isSymbolicLink()) fail("dist must be a real directory");
const output = path.join(outputDir, `app-${report.digest.slice(7, 19)}.zip`);
if (archive.length > MAX_ARCHIVE_BYTES) fail("the ZIP exceeds the 8 MiB upload limit");
if (existsSync(output)) {
  if (lstatSync(output).isSymbolicLink() || !readFileSync(output).equals(archive)) fail(`output already exists with different bytes: ${output}`);
} else writeFileSync(output, archive, { flag: "wx" });
console.log(`Private app package: ${output}\nBundle digest: ${report.digest}\nThe workspace must still check this ZIP before installation.`);
