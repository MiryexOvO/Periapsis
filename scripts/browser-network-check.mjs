// Product-path network diagnostics: real OBS, Relay, gateway and browser, isolated config.
// 产品链路弱网诊断：真实 OBS、Relay、网关和浏览器，使用隔离配置。
import fs from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import crypto from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {udpProxy} from './browser-network-proxy.mjs';
import {summarizeDisplay} from './browser-network-metrics.ts';
import {browserProfile,uplinkCases} from './browser-network-profiles.mjs';
import {assessNetwork} from './browser-network-acceptance.mjs';
import {assessProduct,productTierFor,productPolicyVersion} from './browser-product-acceptance.mjs';
import {writeTimeline} from './recovery-timeline.mjs';
const require=createRequire(import.meta.url),{chromium}=require('playwright-core');
const root=process.cwd(),directory=fs.mkdtempSync(path.join(root,'dist/browser-network-'));
const manifest={node:process.version,platform:process.platform,binaries:{}};
for(const file of ['target/release/peri.exe','target/release/peri-webtransport.exe',
  'target/obs-package/peri-output/bin/64bit/peri-output.dll','target/obs-package/peri-output/bin/64bit/peri_obs.dll',
  'target/obs-sdk/obs-recovery-source.exe'])
  manifest.binaries[file]=crypto.createHash('sha256').update(fs.readFileSync(path.join(root,file))).digest('hex');
fs.writeFileSync(path.join(directory,'manifest.json'),JSON.stringify(manifest,null,2));
const selected=(process.env.PERI_BROWSER_CASES??'clean,uplink,downlink,slow-viewer').split(',');
const qualities=(process.env.PERI_BROWSER_QUALITIES??'source,720').split(',');
const forceHttp=process.env.PERI_BROWSER_FORCE_HTTP==='1';
if(forceHttp && (selected.some(x=>x!=='clean') || qualities.some(x=>x!=='source')))
  throw Error('HTTP diagnostic supports only clean source; UDP shaping would not measure HTTP');
const declaredProductTier=process.env.PERI_BROWSER_PRODUCT_TIER;
if(declaredProductTier)productTierFor('clean',declaredProductTier);
const productPlan=Object.fromEntries(selected.map(name=>[name,productTierFor(name,declaredProductTier)]));
manifest.productPolicy={version:productPolicyVersion,plan:productPlan};
fs.writeFileSync(path.join(directory,'manifest.json'),JSON.stringify(manifest,null,2));
if(!process.env.PERI_OBS_UPLINK_EXPERIMENT &&
   (process.env.PERI_OBS_ENCODER_BUDGET || process.env.PERI_OBS_PROFILE || process.env.PERI_OBS_CC || process.env.PERI_OBS_KEY_ADMISSION))
  throw Error('encoder profile, budget and CC experiments require PERI_OBS_UPLINK_EXPERIMENT');
if(process.env.PERI_OBS_PROFILE && !['source','720p30','360p30'].includes(process.env.PERI_OBS_PROFILE))
  throw Error('invalid encoder profile');
if(selected.some(x=>!['clean',...uplinkCases,'downlink','slow-viewer'].includes(x)) || qualities.some(x=>!['source','720'].includes(x)))throw Error('invalid matrix');
const duration=Number(process.env.PERI_BROWSER_DAMAGE_MS??30000);
if(!Number.isInteger(duration)||duration<5000||duration>30000)throw Error('damage duration must be 5000..30000 ms');
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const results=[];
console.log('ARTIFACTS '+directory);
async function port(){const s=net.createServer();await new Promise(r=>s.listen(0,'127.0.0.1',r));const p=s.address().port;await new Promise(r=>s.close(r));return p;}
async function trial(name,quality){
  const dir=path.join(directory,name+'-'+quality);fs.mkdirSync(dir);
  const jobs=[],proxies=[],pages=[];let browser,monitor;
  const token=crypto.randomBytes(32).toString('hex');
  const env={...process.env,TMP:dir,TEMP:dir,PERI_PUBLISH_TOKEN:token,PERI_SUBSCRIBE_TOKEN:crypto.randomBytes(32).toString('hex'),
    PERI_UNI_STREAM_WINDOW:process.env.PERI_UNI_STREAM_WINDOW??(process.env.PERI_OBS_UPLINK_EXPERIMENT?'32':'8'),
    PERI_OBS_TEST_NETWORK:'1',PERI_OBS_DIAGNOSTICS:'1',PERI_OBS_TEST_SECONDS:'90',PERI_OBS_TEST_STOP:path.join(dir,'stop'),
    PERI_OBS_TEST_CONFIG:path.join(dir,'obs-config'),PERI_CONFIG:path.join(dir,'config.json'),
    PERI_FFMPEG:process.env.PERI_FFMPEG??path.join(process.env.USERPROFILE,'scoop/apps/ffmpeg/current/bin/ffmpeg.exe')};
  fs.mkdirSync(env.PERI_OBS_TEST_CONFIG);
  function run(label,bin,args,cwd=root){
    const p=spawn(bin,args,{cwd,env,windowsHide:true}),job={p,label,log:''};
    for(const s of [p.stdout,p.stderr])s.on('data',b=>job.log=(job.log+b).slice(-4*1024*1024));
    job.done=new Promise((resolve,reject)=>{p.once('error',reject);p.once('close',resolve)});job.done.catch(()=>{});
    jobs.push(job);return job;
  }
  async function wait(job,re){
    for(let i=0;i<300;i++){const m=job.log.match(re);if(m)return m;if(job.p.exitCode!==null)break;await sleep(50);}
    throw Error('missing '+re+' in '+job.label);
  }
  const report={name,quality,scope:'local-real-OBS-pixel-clock-to-browser; synthetic one-way UDP impairment',
    productTier:productPlan[name],
    uplinkExperiment:!!env.PERI_OBS_UPLINK_EXPERIMENT,
    keyAdmissionPolicy:env.PERI_OBS_UPLINK_EXPERIMENT&&env.PERI_OBS_KEY_ADMISSION?'reserved-key-and-prefix-skip':'fifo',
    sourceQueue:{frames:32,bytes:2097152,deadlineMs:env.PERI_OBS_UPLINK_EXPERIMENT?750:5000},
    encoderBudget:!!env.PERI_OBS_ENCODER_BUDGET,
    encoderProfile:env.PERI_OBS_PROFILE??'source',
    originalFrameDivisor:Number(env.PERI_OBS_TEST_FRAME_DIVISOR??1),
    congestionController:env.PERI_OBS_CC??'cubic',
    negotiatedStreamWindow:Number(env.PERI_UNI_STREAM_WINDOW),
    relayQueue:env.PERI_UNI_STREAM_WINDOW==='32'?{frames:64,bytes:2097152,residenceMs:300}:
      {frames:8,bytes:12*(1048576-68),residenceMs:300},
    encoderStepTest:{initialKbps:Number(env.PERI_OBS_TEST_BITRATE??20000),steps:!!env.PERI_OBS_TEST_RATE_STEPS},
    source:{width:env.PERI_OBS_PROFILE==='720p30'?1280:env.PERI_OBS_PROFILE==='360p30'?640:2560,
      height:env.PERI_OBS_PROFILE==='720p30'?720:env.PERI_OBS_PROFILE==='360p30'?360:1440,
      fps:['720p30','360p30'].includes(env.PERI_OBS_PROFILE)?30:60/Number(env.PERI_OBS_TEST_FRAME_DIVISOR??1),
      targetMbps:Math.min(Number(env.PERI_OBS_TEST_BITRATE??20000),env.PERI_OBS_PROFILE==='720p30'?4000:env.PERI_OBS_PROFILE==='360p30'?1000:100000)/1000,keyintSeconds:1,
      texture:env.PERI_OBS_TEST_STATIC_TEXTURE?'seeded-static-320x180-upscaled':env.PERI_OBS_TEST_PAN_TEXTURE?
        'seeded-pan-320x180-upscaled':'seeded-per-frame-320x180-upscaled'},
    queue:{maxPackets:512,maxBytes:2097152,maxScheduledDelayMs:250},durationMs:duration,seed:20261107,
    fallbackScope:'HTTP/TCP fallback is not shaped; switching transport is not proof of UDP recovery',
    avSync:'not measured',memoryScope:'sampled relay/gateway/OBS and gateway direct children; excludes browser',viewers:[]};
  try{
    const bin=path.join(root,'target/release/peri.exe'),gatewayBin=path.join(root,'target/release/peri-webtransport.exe');
    execFileSync(bin,['init',path.join(dir,'tls')],{env,stdio:'pipe'});
    const cert=path.join(dir,'tls/cert.der'),resource='live/network';
    const relay=run('relay',bin,['relay','127.0.0.1:0',cert,path.join(dir,'tls/key.key'),resource]);
    const addr=(await wait(relay,/PERI_RELAY_READY (\S+)/))[1],relayPort=Number(addr.split(':')[1]);
    const publisherProxy=await udpProxy(relayPort,20261107);proxies.push(publisherProxy);
    const httpPort=await port(),wtPort=await port(),origin='http://127.0.0.1:'+httpPort;
    const certPem=path.join(dir,'wt.pem'),keyPem=path.join(dir,'wt.key');
    execFileSync('C:/Program Files/Git/usr/bin/openssl.exe',['req','-x509','-newkey','ec','-pkeyopt','ec_paramgen_curve:P-256','-nodes','-days','7','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1,DNS:localhost','-keyout',keyPem,'-out',certPem],{stdio:'ignore'});
    Object.assign(env,{PERI_WT_BIND:'127.0.0.1:'+wtPort,PERI_WT_URL:'https://127.0.0.1:'+wtPort,PERI_PAGE_ORIGIN:origin,
      PERI_WT_CERT:certPem,PERI_WT_KEY:keyPem,PERI_WT_CERT_HASH:crypto.createHash('sha256').update(new crypto.X509Certificate(fs.readFileSync(certPem)).raw).digest('base64')});
    fs.writeFileSync(env.PERI_CONFIG,JSON.stringify({udp:relayPort,tcp:httpPort,cert,name:'localhost',resource}));
    const gateway=run('gateway',gatewayBin,['watch']);await wait(gateway,/PERI_WT_READY/);
    env.PERI_OBS_TEST_TICKET='peri://v1/'+Buffer.from(JSON.stringify({address:'127.0.0.1:'+publisherProxy.port,name:'localhost',resource,token,cert:fs.readFileSync(cert).toString('base64')})).toString('base64url');
    if(env.PERI_OBS_TEST_CONNECTION==='separate') {
      const signingConfig=path.join(dir,'publisher-config.json');
      fs.writeFileSync(signingConfig,JSON.stringify({public:'127.0.0.1',udp:relayPort,external:publisherProxy.port,
        tcp:httpPort,web_external:httpPort,cert,key:path.join(dir,'tls/key.key'),name:'localhost',resource,
        tokens:{PERI_PUBLISH_TOKEN:token,PERI_SUBSCRIBE_TOKEN:env.PERI_SUBSCRIBE_TOKEN}}));
      const details=execFileSync(gatewayBin,['link'],{env:{...env,PERI_CONFIG:signingConfig},encoding:'utf8'});
      env.PERI_OBS_TEST_TICKET=details.split('Stream key: ')[1]?.trim();
      if(!env.PERI_OBS_TEST_TICKET?.startsWith('peri1_'))throw Error('Missing generated publish code');
      env.PERI_OBS_TEST_SERVER='peri://127.0.0.1:'+publisherProxy.port;
      report.connectionMode='separate-address-and-code';
    } else report.connectionMode='legacy-invitation';
    const obsDir='D:/OBS/obs-studio/bin/64bit';env.PATH=obsDir+';'+process.env.PATH;
    const source=run('source',path.join(root,'target/obs-sdk/obs-recovery-source.exe'),[],obsDir);await wait(relay,/PERI_RESOURCE_READY/);
    browser=await chromium.launch({executablePath:'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',headless:true,args:['--autoplay-policy=no-user-gesture-required','--disable-background-timer-throttling','--disable-renderer-backgrounding']});
    report.browser=browser.version();
    const viewerProxies=[];
    for(let i=0;i<2;i++){
      const proxy=await udpProxy(wtPort,20261107+i);proxies.push(proxy);viewerProxies.push(proxy);
      const context=await browser.newContext(),page=await context.newPage();pages.push(page);
      await page.addInitScript(({port})=>{
        const Native=window.WebTransport;
        window.WebTransport=class extends Native{constructor(url,options){const u=new URL(url);u.port=String(port);super(u.href,options)}};
      },{port:proxy.port});
      await page.addInitScript({path:path.join(root,'scripts/browser-network-probe.js')});
      if(forceHttp){
        await page.addInitScript(()=>{
          window.WebTransport=undefined;
          window.__httpDiagnostic=[];
          addEventListener('DOMContentLoaded',()=>{
            const v=document.querySelector('video'),record=event=>{
              if(window.__httpDiagnostic.length<3000)window.__httpDiagnostic.push({at:Date.now(),event,
                time:v.currentTime,ready:v.readyState,paused:v.paused,delivery:v.dataset.httpDelivery,
                buffered:Array.from({length:v.buffered.length},(_,i)=>[v.buffered.start(i),v.buffered.end(i)])});
            };
            for(const name of ['waiting','stalled','playing','error','emptied'])v.addEventListener(name,()=>record(name));
            setInterval(()=>record('sample'),200);
          });
        });
        const cdp=await context.newCDPSession(page);await cdp.send('Network.enable');
        const mediaIds=new Set(),arrivals=[];
        cdp.on('Network.responseReceived',e=>{if(e.response.url.includes('/live.webm'))mediaIds.add(e.requestId)});
        cdp.on('Network.dataReceived',e=>{if(mediaIds.has(e.requestId)&&arrivals.length<20000)arrivals.push({at:Date.now(),bytes:e.dataLength})});
        page.__httpArrivals=arrivals;
      }
      const errors=[];page.on('console',m=>{if(m.type()==='debug'||m.type()==='error')errors.push({at:Date.now(),message:m.text()})});
      page.on('pageerror',e=>errors.push({at:Date.now(),error:e.message}));
      page.__errors=errors;
      await page.goto(origin);
      if(quality==='720')await page.locator('#quality').selectOption('720');
      if(forceHttp){
        await page.waitForFunction(()=>document.querySelector('video').currentTime>0,null,{timeout:30000});
        await page.evaluate(()=>{document.querySelector('video').muted=false});
      }else{
        await page.waitForFunction(h=>{const c=document.querySelector('main>canvas');return !c.hidden&&c.height===h&&!!c.dataset.pts},quality==='720'?720:report.source.height,{timeout:30000});
        await page.locator('#sound').click();
      }
    }
    // Independent monitor avoids timing process sampling inside the UDP event loop.
    // 独立进程采样内存和 CPU，避免进程查询阻塞 UDP 事件循环。
    const ids=[relay.p.pid,gateway.p.pid,source.p.pid];
    const monitorScript=`while ($true) { $pidsToRead = @(${ids.join(',')}); $children = Get-CimInstance Win32_Process -Filter 'ParentProcessId = ${gateway.p.pid}' -ErrorAction SilentlyContinue; $pidsToRead += @($children | ForEach-Object ProcessId); $items = @(Get-Process -Id $pidsToRead -ErrorAction SilentlyContinue | Select-Object Id,ProcessName,WorkingSet64,PrivateMemorySize64,CPU); [pscustomobject]@{at=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds();items=$items} | ConvertTo-Json -Compress -Depth 4; Start-Sleep -Seconds 2 }`;
    monitor=run('resources','powershell.exe',['-NoProfile','-Command',monitorScript]);
    const warmStart=Date.now();await sleep(3000);
    const start=Date.now();
    const profile=browserProfile(name,quality);
    for(const p of proxies)p.setPhase('damage');
    if(uplinkCases.includes(name))publisherProxy.setPhase('damage',profile);
    if(name==='downlink')viewerProxies[0].setPhase('damage',profile);
    if(name==='slow-viewer')viewerProxies[1].setPhase('damage',profile);
    report.profile=name==='clean'?null:profile;
    console.log('RUN '+name+' '+quality+' damage='+duration+'ms');
    await sleep(duration);
    const damageEnd=Date.now();for(const p of proxies)p.setPhase('recovery');
    await sleep(8000);const end=Date.now();
    report.windows={baselineStart:warmStart,damageStart:start,damageEnd,end};
    for(let i=0;i<pages.length;i++){
      if(forceHttp)fs.writeFileSync(path.join(dir,'http-'+i+'.json'),JSON.stringify({
        samples:await pages[i].evaluate(()=>window.__httpDiagnostic),arrivals:pages[i].__httpArrivals}));
      const data=await pages[i].evaluate(()=>{clearInterval(window.__periProbe.timer);const{frames,ticks,events,pixelDebug,decoder}=window.__periProbe;return {frames,ticks,events,pixelDebug,decoder}});
      await pages[i].screenshot({path:path.join(dir,'viewer-'+i+'.png')});
      const recoveryTicks=data.ticks.filter(t=>t.at>=damageEnd);
      const healthy=recoveryTicks.findIndex((t,n)=>t.lagMs!==null&&t.lagMs<1000&&recoveryTicks.slice(n,n+5).length===5&&recoveryTicks.slice(n,n+5).every(x=>x.lagMs!==null&&x.lagMs<1000));
      const fps=quality==='720'?60:report.source.fps;
      const viewer={index:i,baseline:summarizeDisplay(data.frames,data.ticks,warmStart,start,fps),damage:summarizeDisplay(data.frames,data.ticks,start,damageEnd,fps),
        recovery:summarizeDisplay(data.frames,data.ticks,damageEnd,end,fps),recoveredUnder1sAfterMs:healthy<0?null:recoveryTicks[healthy].at-damageEnd,
        frameDurations:data.decoder?.durations,errors:pages[i].__errors};report.viewers.push(viewer);
      fs.writeFileSync(path.join(dir,'viewer-'+i+'.json'),JSON.stringify(data));
    }
    report.network={publisher:publisherProxy.stats,viewers:viewerProxies.map(p=>p.stats)};
    report.resources=monitor.log.trim().split(/\r?\n/).flatMap(line=>{try{return [JSON.parse(line)]}catch{return []}});
    fs.writeFileSync(env.PERI_OBS_TEST_STOP,'stop');
    const stopped=await Promise.race([source.done,sleep(8000).then(()=>null)]);
    report.sourceExit=stopped;
    report.sourceBytes=Number(source.log.match(/BYTES (\d+)/)?.[1]??0);
    report.sourceRecoveryRequests=(source.log.match(/Recovery submitted/g)??[]).length;
    report.sourceBitrateChanges=[...source.log.matchAll(/Uplink bitrate.*?: (\d+) -> (\d+) kbps/g)]
      .map(m=>({fromKbps:Number(m[1]),toKbps:Number(m[2])}));
    report.encoderMeasurements=[...source.log.matchAll(/Encoded video.*?: kbps=(\d+) target=(\d+) keys=(\d+) key_bytes=(\d+)/g)]
      .map(m=>({actualKbps:Number(m[1]),targetKbps:Number(m[2]),keys:Number(m[3]),keyBytes:Number(m[4])}));
    report.status=stopped!==0?'source-failed':report.viewers.some(v=>v.baseline.pixelSamples<20)?'instrumentation-invalid':'measured';
  }catch(e){
    report.status='failed';report.error=e.message;
    report.failureNetwork=proxies.map(p=>p.stats);
    for(let i=0;i<pages.length;i++)try{
      const data=await pages[i].evaluate(()=>({probe:window.__periProbe,status:document.querySelector('[role=status]')?.textContent,transport:document.querySelector('video')?.dataset.transport}));
      fs.writeFileSync(path.join(dir,'failure-viewer-'+i+'.json'),JSON.stringify({data,errors:pages[i].__errors}));
      await pages[i].screenshot({path:path.join(dir,'failure-viewer-'+i+'.png')});
    }catch{}
  }
  finally{
    if(browser)await browser.close();
    for(const p of proxies)p.close();
    for(const job of jobs.reverse()){
      if(job.p.exitCode===null&&!job.p.killed){
        try{execFileSync('taskkill',['/pid',String(job.p.pid),'/T','/F'],{stdio:'ignore',windowsHide:true})}catch{}
      }
      let log=job.log;for(const secret of [env.PERI_PUBLISH_TOKEN,env.PERI_SUBSCRIBE_TOKEN,env.PERI_OBS_TEST_TICKET])if(secret)log=log.split(secret).join('[redacted]');
      fs.writeFileSync(path.join(dir,job.label+'.log'),log);
    }
    fs.writeFileSync(path.join(dir,'report.json'),JSON.stringify(report,null,2));
  }
  return report;
}
for(const quality of qualities)for(const name of selected){
  const report=await trial(name,quality);report.acceptance=assessNetwork(report);results.push(report);
  report.productAcceptance=assessProduct(report);
  const dir=path.join(directory,name+'-'+quality);
  fs.writeFileSync(path.join(dir,'report.json'),JSON.stringify(report,null,2));
  if(report.status==='measured')writeTimeline(dir);
  fs.writeFileSync(path.join(directory,'summary.json'),JSON.stringify(results,null,2));
  console.log(JSON.stringify({case:name,quality,status:report.status,error:report.error,acceptance:report.acceptance,productAcceptance:report.productAcceptance,
    viewers:report.viewers.map(v=>({index:v.index,...v.damage,recoveryMs:v.recoveredUnder1sAfterMs}))}));
}
if(results.some(r=>r.status!=='measured') || (process.env.PERI_BROWSER_ENFORCE_ACCEPTANCE && results.some(r=>!r.acceptance.passed)))process.exitCode=1;
if(process.env.PERI_BROWSER_ENFORCE_PRODUCT && results.some(r=>!r.productAcceptance.passed))process.exitCode=1;
