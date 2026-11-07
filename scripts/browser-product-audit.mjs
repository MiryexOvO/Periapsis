// Retrospective review writes a new artifact and never rewrites original evidence.
import fs from 'node:fs';
import path from 'node:path';
import {assessProduct,productTierFor,productPolicyVersion} from './browser-product-acceptance.mjs';
const args=process.argv.slice(2),override=args.find(x=>x.startsWith('--tier='))?.slice(7);
if(override)productTierFor('clean',override);
const directories=args.filter(x=>!x.startsWith('--'));
if(!directories.length||args.some(x=>x.startsWith('--')&&!x.startsWith('--tier=')))
  throw Error('usage: browser-product-audit.mjs [--tier=normal|recoverable|severe] result-directory ...');
const rows=[];
for(const dir of directories){
  for(const report of JSON.parse(fs.readFileSync(path.join(dir,'summary.json'),'utf8'))){
    const tier=override??report.productTier??productTierFor(report.name);
    rows.push({directory:path.resolve(dir),name:report.name,quality:report.quality,
      originalAcceptance:report.acceptance??null,productAcceptance:assessProduct(report,tier)});
  }
}
const out=fs.mkdtempSync(path.resolve('dist/product-review-'));
fs.writeFileSync(path.join(out,'review.json'),JSON.stringify({retrospective:true,version:productPolicyVersion,rows},null,2));
console.log('ARTIFACTS '+out);
for(const r of rows)console.log(JSON.stringify({name:r.name,quality:r.quality,
  stress:r.originalAcceptance?.passed,tier:r.productAcceptance.tier,product:r.productAcceptance.status,
  failures:r.productAcceptance.failures,missing:r.productAcceptance.missing}));
