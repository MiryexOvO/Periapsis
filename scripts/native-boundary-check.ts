import { readFile, readdir } from "node:fs/promises";
import { spawn, spawnSync } from "node:child_process";
// 审计Cargo真实解析图，包含可选feature和测试依赖；不能只检查Cargo.toml直接依赖。
// 仅审计指定构建目标，避免下载无关平台依赖；缺失本地依赖时明确失败。
const compiler=spawnSync("rustc",["-vV"],{encoding:"utf8",windowsHide:true});
const target=process.env.PERI_BOUNDARY_TARGET ?? process.env.CARGO_BUILD_TARGET ?? compiler.stdout?.match(/^host: (.+)$/m)?.[1]?.trim();
if(!target)throw new Error("unable to determine Cargo target");
const child=spawn("cargo",["metadata","--format-version=1","--locked","--all-features","--offline","--filter-platform",target],{windowsHide:true,stdio:["ignore","pipe","pipe"]});
let output="",errors="";
child.stdout.on("data",data=>output+=String(data));child.stderr.on("data",data=>errors+=String(data));
const timer=setTimeout(()=>child.kill(),30000);
const code=await new Promise<number|null>((resolve,reject)=>{child.on("error",reject);child.on("close",resolve);}).finally(()=>clearTimeout(timer));
if(code!==0) throw new Error(`Cargo dependency audit failed: ${errors}`);
interface Package {id:string;name:string}
interface Node {id:string;deps:{pkg:string}[]}
const metadata=JSON.parse(output) as {packages:Package[];resolve:{nodes:Node[]};workspace_default_members:string[]};
const packages=new Map(metadata.packages.map(pkg=>[pkg.id,pkg]));
const nodes=new Map(metadata.resolve.nodes.map(node=>[node.id,node]));
const native=metadata.packages.find(pkg=>pkg.name==="peri-native");
if(!native) throw new Error("missing peri-native package");
const expected=["peri-native","peri-scheduler","peri-session","peri-types"].sort();
const defaults=metadata.workspace_default_members.map(id=>packages.get(id)?.name).sort();
if(JSON.stringify(defaults)!==JSON.stringify(expected)) throw new Error(`unexpected default workspace members: ${defaults}`);
const policy=JSON.parse(await readFile(new URL("../tools/internal-comparison/boundary-policy.json",import.meta.url),"utf8")) as {forbiddenNamePattern:string;forbiddenTextPattern:string};
const forbidden=new RegExp(policy.forbiddenNamePattern,"i");
const seen=new Set<string>(),pending=[native.id];
while(pending.length) {
 const id=pending.pop()!;if(seen.has(id))continue;seen.add(id);
 const pkg=packages.get(id);if(!pkg)throw new Error(`unknown package ${id}`);
 if(forbidden.test(pkg.name)) throw new Error(`External baseline dependency leaked into PERI native: ${pkg.name}`);
 for(const dep of nodes.get(id)?.deps ?? []) pending.push(dep.pkg);
}
console.log(`PERI dependency boundary accepted: ${seen.size} reachable packages on ${target}; default members=${defaults.join(",")}; internal baselines excluded`);
export {};

// 正式源码、用户文档和根依赖清单不得泄漏内部对照协议名称。
const prohibited=new RegExp(policy.forbiddenTextPattern,"i");
async function scan(url:URL):Promise<void> {
 for(const entry of await readdir(url,{withFileTypes:true})) {
  const path=new URL(entry.name+(entry.isDirectory()?"/":""),url);
  if(entry.isDirectory()) await scan(path);
  else if(/\.(rs|toml|md|json|ts)$/.test(entry.name) && prohibited.test(await readFile(path,"utf8"))) throw new Error(`Internal baseline name leaked into formal source: ${path.pathname}`);
 }
}
for(const name of ["crates/","docs/","scripts/","tests/"]) await scan(new URL(`../${name}`,import.meta.url));
for(const name of ["README.md","Cargo.toml","Cargo.lock","package.json","pnpm-lock.yaml"]) {
 if(prohibited.test(await readFile(new URL(`../${name}`,import.meta.url),"utf8"))) throw new Error(`Internal baseline name leaked into public entry: ${name}`);
}
console.log("PERI public surface naming audit accepted");
