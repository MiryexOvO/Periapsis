// Versioned product objectives; the original stress gate remains independent.
// 产品目标与原有压力门槛分别记录，不用分档覆盖历史失败。
export const productPolicyVersion='2026-11-07.1';
export const productTiers=['normal','recoverable','severe'];
export function productTierFor(name,declared){
  if(declared){
    if(!productTiers.includes(declared))throw Error('invalid product tier');
    return declared;
  }
  // Capacity tests must declare their tier: the same 8 Mbps pipe can be ample or insufficient.
  // 同一8 Mbps容量对不同源可能充裕或不足，不从最终播放成绩倒推档位。
  if(name==='clean')return 'normal';
  if(name==='slow-viewer')return 'severe';
  if(name==='uplink-capacity')return null;
  return 'recoverable';
}

export function assessProduct(report,tier=report.productTier){
  const failures=[],missing=[];
  if(!productTiers.includes(tier))missing.push('product tier must be declared before the run');
  if(tier==='severe')missing.push('automatic downgrade, observed output and sustained playback evidence are not implemented in this runner');
  const fps=report.quality==='720'?60:report.source?.fps;
  if(!Number.isFinite(fps)||fps<=0)missing.push('valid expected frame rate required');
  if(report.status!=='measured')failures.push('measurement incomplete');
  if(report.sourceExit!==0)failures.push('source did not stop cleanly');
  if(report.viewers?.length!==2)failures.push('two viewers required');
  for(const v of report.viewers??[]){
    for(const phase of ['baseline','damage','recovery']){
      const d=v[phase],prefix=`viewer ${v.index} ${phase}`;
      if(!d){missing.push(`${prefix}: missing metrics`);continue;}
      // Continuity is allowed to trade frame rate during a recoverable impairment.
      // 可恢复弱网期间允许帧率让步，正常和恢复后仍要达到声明帧率的90%。
      const minFps=tier==='recoverable'&&phase==='damage'?Math.min(24,fps*.9):fps*.9;
      for(const field of ['renderFps','freezeRatio','maxGapMs','pixelP95Ms','pixelSamples','missingPixelSamples'])
        if(!Number.isFinite(d[field]))missing.push(`${prefix}: invalid ${field}`);
      if(d.renderFps<minFps)failures.push(`${prefix}: FPS below ${minFps}`);
      if(d.freezeRatio>.05)failures.push(`${prefix}: freeze above 5%`);
      if(d.maxGapMs>1000)failures.push(`${prefix}: single display gap exceeds 1s`);
      if(d.pixelP95Ms>=1000)failures.push(`${prefix}: P95 not below 1s`);
      if(d.pixelSamples<20 || d.missingPixelSamples/(d.pixelSamples+d.missingPixelSamples)>.1)
        failures.push(`${prefix}: insufficient pixel-clock coverage`);
      if(d.transports?.length!==1||d.transports[0]!=='webcodecs')failures.push(`${prefix}: transport fallback`);
    }
    if(!Number.isFinite(v.recoveredUnder1sAfterMs))missing.push(`viewer ${v.index}: missing recovery evidence`);
    else if(v.recoveredUnder1sAfterMs>1000)failures.push(`viewer ${v.index}: recovery exceeds 1s`);
    if(v.errors?.some(e=>e.error))failures.push(`viewer ${v.index}: uncaught page error`);
    if(Object.keys(v.frameDurations??{}).some(d=>Number(d)>1_500_000/fps))
      failures.push(`viewer ${v.index}: frame duration exceeds expected cadence`);
  }
  const status=missing.length?'not-evaluated':failures.length?'fail':'pass';
  return {version:productPolicyVersion,tier:tier??null,status,passed:status==='pass',
    scope:'local video continuity only; audio sync, picture quality and public-network readiness require separate validation',
    criteria:{normalFpsRatio:.9,impairedMinFps:tier==='recoverable'?Math.min(24,fps*.9):null,
      freezeRatio:.05,maxGapMs:1000,pixelP95Ms:1000,recoveryMs:1000},failures,missing};
}
