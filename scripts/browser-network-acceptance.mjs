// Explicit product gate. A measured run, a QUIC ACK or a fallback is not a pass.
export function assessNetwork(report) {
  const failures=[];
  const fps=report.quality==='720'?60:report.source.fps;
  if(report.status!=='measured')failures.push('measurement incomplete');
  if(report.sourceExit!==0)failures.push('source did not stop cleanly');
  if(report.viewers.length!==2)failures.push('two viewers required');
  for(const v of report.viewers){
    if(Object.keys(v.frameDurations??{}).some(d=>Number(d)>1_500_000/fps))
      failures.push(`viewer ${v.index}: frame duration exceeds expected cadence`);
    for(const phase of ['baseline','damage','recovery']){
      const d=v[phase], prefix=`viewer ${v.index} ${phase}`;
      if(!d){failures.push(`${prefix}: missing`);continue;}
      if(d.renderFps<fps*.9)failures.push(`${prefix}: FPS below 90%`);
      if(d.freezeRatio>.05)failures.push(`${prefix}: freeze above 5%`);
      if(d.pixelP95Ms===null || d.pixelP95Ms>=1000)failures.push(`${prefix}: P95 not below 1s`);
      if(d.pixelSamples<20 || d.missingPixelSamples/(d.pixelSamples+d.missingPixelSamples)>.1)
        failures.push(`${prefix}: insufficient pixel-clock coverage`);
      if(d.transports.length!==1 || d.transports[0]!=='webcodecs')failures.push(`${prefix}: transport fallback`);
    }
    if(v.recoveredUnder1sAfterMs===null || v.recoveredUnder1sAfterMs>1000)
      failures.push(`viewer ${v.index}: recovery exceeds 1s`);
    if(v.errors?.some(e=>e.error))failures.push(`viewer ${v.index}: uncaught page error`);
  }
  return {passed:failures.length===0,criteria:{fpsRatio:.9,freezeRatio:.05,pixelP95Ms:1000,recoveryMs:1000},failures};
}
