/** CAD-1108 / contract I1: the chat code names no app. A source scan, so a
 *  later change cannot add an app branch back unnoticed. */
declare function require(name: string): any;
declare const process: { cwd(): string };
export {};
const fs = require("fs"), path = require("path");
const root = path.join(process.cwd(), "src", "features");
const files: string[] = [];
const walk = (dir: string) => {
  for (const name of fs.readdirSync(dir)) {
    const full = path.join(dir, name);
    if (fs.statSync(full).isDirectory()) walk(full);
    else files.push(full);
  }
};
walk(path.join(root, "app-shell", "chat"));
for (const name of fs.readdirSync(path.join(root, "home"))) {
  if (name.startsWith("ThreadView")) files.push(path.join(root, "home", name));
}
if (files.length < 10) throw new Error(`source scan found only ${files.length} files`);
const banned = /installation\.name|isSocial|\bcrm\b|social-content/;
for (const file of files) {
  const hit = banned.exec(fs.readFileSync(file, "utf8"));
  if (hit) throw new Error(`${path.relative(root, file)} names an app: ${hit[0]}`);
}
console.log("chat source scan passed");
