// Test-only UDP relay: independently bounded directions and explicit loss reasons.
// 仅测试用 UDP 中继：双向独立有界队列，区分随机、断网和队列丢包。
import dgram from 'node:dgram';
import {reserveDatagram} from './network-queue.ts';
import {seededRandom} from './network-profiles.ts';
export async function udpProxy(targetPort,seed) {
  const socket=dgram.createSocket('udp4'), peers=new Map();
  const random=seededRandom(seed), queues={up:[],down:[]}, next={up:0,down:0};
  const stats={up:{},down:{}};
  let profile=null,begin=0,phase='warmup',timer,closed=false;
  const count=(direction,key,n=1)=>{
    const s=stats[direction][phase]??={packets:0,bytes:0,sent:0,randomDrop:0,blackoutDrop:0,queueDrop:0,
      peakPackets:0,peakBytes:0,lateMaxMs:0,lateTotalMs:0,scheduled:0,errors:0};
    s[key]=(s[key]??0)+n;return s;
  };
  function pump(){
    timer=undefined;let nearest=Infinity;
    for(const direction of ['up','down']){
      const q=queues[direction];
      while(q.length && q[0].due<=performance.now()){
        const item=q.shift(),late=Math.max(0,performance.now()-item.due);
        item.stat.lateMaxMs=Math.max(item.stat.lateMaxMs,late);item.stat.lateTotalMs+=late;
        item.send(item.bytes);item.stat.sent++;
      }
      if(q.length)nearest=Math.min(nearest,q[0].due);
    }
    if(nearest<Infinity&&!closed)timer=setTimeout(pump,Math.max(1,nearest-performance.now()));
  }
  function forward(direction,bytes,send){
    if(closed)return;
    const s=count(direction,'packets');s.bytes+=bytes.length;
    const p=profile?.direction===direction?profile:null;
    const elapsed=performance.now()-begin;
    if(p?.blackoutMs && elapsed%5000>=2000 && elapsed%5000<2000+p.blackoutMs){s.blackoutDrop++;return;}
    if(p && random()<p.loss){s.randomDrop++;return;}
    const q=queues[direction],now=performance.now();
    if(!p && !q.length){send(bytes);s.sent++;return;}
    const queuedBytes=q.reduce((sum,item)=>sum+item.bytes.length,0);
    const r=reserveDatagram(now,next[direction],bytes.length,q.length,!!p,p?.bytesPerSecond??1,p?.delayMs??0,{packets:512,delayMs:250});
    if(!r || queuedBytes+bytes.length>2*1024*1024){s.queueDrop++;return;}
    next[direction]=r.next;
    q.push({bytes,due:now+r.delay,send,stat:s});s.scheduled++;
    s.peakPackets=Math.max(s.peakPackets,q.length);s.peakBytes=Math.max(s.peakBytes,queuedBytes+bytes.length);
    if(!timer)pump();
  }
  socket.on('error',()=>count('up','errors'));
  socket.on('message',(bytes,remote)=>{
    const key=remote.address+':'+remote.port;
    let upstream=peers.get(key);
    if(!upstream){
      if(peers.size>=8){count('up','queueDrop');return;}
      upstream=dgram.createSocket('udp4');peers.set(key,upstream);
      upstream.on('error',()=>count('down','errors'));
      upstream.on('message',b=>forward('down',b,data=>socket.send(data,remote.port,remote.address)));
    }
    const peer=upstream;
    forward('up',bytes,data=>peer.send(data,targetPort,'127.0.0.1'));
  });
  await new Promise(resolve=>socket.bind(0,'127.0.0.1',resolve));
  return {port:socket.address().port,stats,
    setPhase(name,p=null){phase=name;profile=p;begin=performance.now();},
    close(){closed=true;clearTimeout(timer);queues.up.length=queues.down.length=0;
      socket.close();for(const s of peers.values())s.close();},
  };
}
