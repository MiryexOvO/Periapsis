import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const html=fs.readFileSync(new URL('../crates/peri-webtransport/web/watch.html',import.meta.url),'utf8');
const script=html.slice(html.lastIndexOf('<script>')+8,html.lastIndexOf('</script>'))
  .replace('__PERI_WT_CONFIG__','{}');
function player(){
  let now=0,id=0,loads=0;
  const intervals=new Map(),status={},quality={value:'source',options:[{},{}],selectedOptions:[{}]};
  const video={currentTime:0,paused:false,seeking:false,readyState:0,muted:true,dataset:{},
    buffered:{length:0},canPlayType:()=> 'probably',
    removeAttribute(){},load(){loads++;this.currentTime=0;},play(){this.paused=false;return Promise.resolve();}};
  const document={hidden:false,querySelector:selector=>selector==='video'?video:selector==='#quality'?quality:status};
  const context=vm.createContext({document,window:{},navigator:{userAgent:'test',platform:'test'},
    performance:{now:()=>now},Date:{now:()=>now},Math,encodeURIComponent,
    setTimeout:()=>++id,clearTimeout(){},setInterval:fn=>{intervals.set(++id,fn);return id;},
    clearInterval:n=>intervals.delete(n)});
  vm.runInContext(script,context);
  return {video,document,quality,context,intervals,get loads(){return loads;},
    tick(ms){now+=ms;for(const fn of [...intervals.values()])fn();}};
}
test('HTTP live reconnects a stalled clock with a fresh URL without waiting for media error',()=>{
  const p=player(),original=p.video.src;
  p.video.currentTime=1;p.tick(1000);
  p.video.onwaiting();p.tick(3000);assert.equal(p.loads,1);
  p.tick(1000);assert.equal(p.loads,2);
  assert.notEqual(p.video.src,original);
  assert.match(p.video.src,/^\/live\.webm\?session=/);
  assert.equal(p.intervals.size,1);
});
test('advancing black or static media and deliberate pauses do not reconnect',()=>{
  const p=player();
  for(let i=0;i<20;i++){p.video.currentTime+=1;p.tick(1000);}
  assert.equal(p.loads,1);
  p.video.paused=true;p.tick(20000);assert.equal(p.loads,1);
  p.video.paused=false;p.video.currentTime+=1;p.tick(1000);assert.equal(p.loads,1);
});
test('initial join gets a longer grace period and HLS owns its recovery',()=>{
  const p=player();p.tick(14000);assert.equal(p.loads,1);
  p.tick(1000);assert.equal(p.loads,2);
  p.quality.value='720';vm.runInContext('tune(false)',p.context);
  const loads=p.loads;p.tick(20000);
  assert.equal(p.loads,loads);assert.equal(p.intervals.size,0);
});
