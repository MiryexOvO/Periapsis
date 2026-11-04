//! 单视频MoQ有界并发组接收；本层配额不替代上游MoQ缓存池配额。
use super::decode_frame;
use crate::moq;
use peri_native::{
    Error,
    media::{Catalog, Frame, FrameKind, TrackKind},
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{AbortHandle, Id, JoinHandle, JoinSet},
};
pub type ReceiveError = Box<dyn std::error::Error + Send + Sync>;
/// 组并发、持有帧槽、声明字节额度均有硬上限；group_frames限制单组工作量。
#[derive(Clone, Copy)]
pub struct LiveLimits {
    /// 最大同时运行的组读取任务，范围1至8。
    pub groups: usize,
    /// 全部读取中、排队和借出的帧槽，上限128且不小于组数。
    pub frames: usize,
    /// 本层声明字节许可证，上限16MiB，不是上游缓存或进程RSS上限。
    pub bytes: usize,
    /// read_all之前允许的单包声明尺寸，上限为原生最大对象加25字节。
    pub packet_bytes: usize,
    /// 单组最多消费的帧数，上限240，超出按协议错误处理。
    pub group_frames: usize,
    /// 单次组内读取及排队窗口，非整个GOP的播放时长，上限一秒。
    pub residence: Duration,
    /// 无活动组时等待下一组的空闲上限，最多30秒。
    pub idle: Duration,
}
impl Default for LiveLimits {
    fn default() -> Self {
        Self {
            groups: 4,
            frames: 8,
            bytes: 4 * (peri_native::MAX_OBJECT_BYTES + 25),
            packet_bytes: peri_native::MAX_OBJECT_BYTES + 25,
            group_frames: 120,
            residence: Duration::from_millis(300),
            idle: Duration::from_secs(5),
        }
    }
}
/// 接收结果持有配额；释放或转移Frame时归还本层配额，转移后由调度器负责缓存上限。
pub struct LiveFrame {
    pub group: u64,
    pub received: Instant,
    /// 原始接收窗口的绝对截止，交给调度器时不能重新起算。
    pub deadline: Instant,
    frame: Frame,
    _bytes: OwnedSemaphorePermit,
    _slot: OwnedSemaphorePermit,
}
impl LiveFrame {
    /// 不转移所有权地查看已校验的媒体帧。
    pub fn frame(&self) -> &Frame {
        &self.frame
    }
    /// 将帧交给有界GOP调度器，本层不再计其内存。
    pub fn into_frame(self) -> Frame {
        self.frame
    }
}
/// Gap只标记单组被跳过或过期；畸形媒体与未知传输错误通过Err返回。
pub enum LiveEvent {
    Frame(LiveFrame),
    Gap { group: u64, reason: &'static str },
}
/// 持有后台读取任务；释放接收器会取消任务及组消费者，不关闭调用方持有的整个MoQ会话。
pub struct LiveReceiver {
    events: mpsc::Receiver<Result<LiveEvent, ReceiveError>>,
    driver: JoinHandle<()>,
    bytes: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    limits: LiveLimits,
    floor: watch::Receiver<(u64, u64)>,
}
impl LiveReceiver {
    /// Catalog须先按协商结果校验；此接口只接收选定的视频轨道，错误的轨道不能混入。
    pub fn new(
        reader: moq::track::Subscriber,
        catalog: Catalog,
        track: u16,
        limits: LiveLimits,
    ) -> Result<Self, Error> {
        if limits.groups == 0
            || limits.groups > 8
            || limits.frames < limits.groups
            || limits.frames > 128
            || limits.packet_bytes < 30
            || limits.packet_bytes > peri_native::MAX_OBJECT_BYTES + 25
            || limits.bytes < limits.packet_bytes * limits.groups
            || limits.bytes > 16 * 1024 * 1024
            || limits.group_frames == 0
            || limits.group_frames > 240
            || limits.residence.is_zero()
            || limits.residence > Duration::from_secs(1)
            || limits.idle.is_zero()
            || limits.idle > Duration::from_secs(30)
            || !catalog
                .tracks
                .iter()
                .any(|t| t.id == track && t.kind == TrackKind::Video)
        {
            return Err(Error::Protocol("MoQ live limits or track"));
        }
        let (tx, events) = mpsc::channel(limits.frames);
        let bytes = Arc::new(Semaphore::new(limits.bytes));
        let slots = Arc::new(Semaphore::new(limits.frames));
        let (floor_tx, floor) = watch::channel((0u64, 0u64));
        let driver = tokio::spawn(drive(
            reader,
            Arc::new(catalog),
            track,
            limits,
            tx,
            bytes.clone(),
            slots.clone(),
            floor_tx,
        ));
        Ok(Self {
            events,
            driver,
            bytes,
            slots,
            limits,
            floor,
        })
    }
    /// 队列关闭返回None；错误返回后调用方应释放接收器并执行会话错误处理。
    pub async fn recv(&mut self) -> Option<Result<LiveEvent, ReceiveError>> {
        loop {
            match self.events.recv().await? {
                Ok(LiveEvent::Frame(frame)) if frame.group < self.floor.borrow().0 => continue,
                Ok(LiveEvent::Gap { group, .. }) if group < self.floor.borrow().0 => continue,
                Ok(LiveEvent::Frame(frame)) if Instant::now() >= frame.deadline => {
                    return Some(Ok(LiveEvent::Gap {
                        group: frame.group,
                        reason: "queue_expired",
                    }));
                }
                event => return Some(event),
            }
        }
    }
    /// 包含排队、读取中及调用方尚未释放的接收句柄，便于验证配额回收。
    pub fn leased_bytes(&self) -> usize {
        self.limits.bytes - self.bytes.available_permits()
    }
    /// 已占用的帧槽数，包含读取中及交给调用方的句柄。
    pub fn leased_frames(&self) -> usize {
        self.limits.frames - self.slots.available_permits()
    }
}
impl Drop for LiveReceiver {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
#[allow(clippy::too_many_arguments)]
async fn drive(
    mut reader: moq::track::Subscriber,
    catalog: Arc<Catalog>,
    track: u16,
    limits: LiveLimits,
    tx: mpsc::Sender<Result<LiveEvent, ReceiveError>>,
    bytes: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    floor: watch::Sender<(u64, u64)>,
) {
    let mut tasks = JoinSet::new();
    let mut active = HashMap::<Id, (u64, AbortHandle)>::new();
    let mut ended = false;
    loop {
        let no_active = tasks.is_empty();
        tokio::select! {
         biased;
         _=tx.closed()=>break,
         completed=tasks.join_next_with_id(),if !tasks.is_empty()=>{
          match completed.expect("active group") {
           Ok((id,(group,result)))=>{active.remove(&id);match result {
            Ok(Some(reason))=>{if tx.send(Ok(LiveEvent::Gap{group,reason})).await.is_err(){break;}},
            Ok(None)=>{},Err(error)=>{let _=tx.send(Err(error)).await;break;}
           }},
           Err(error)=>{active.remove(&error.id());if !error.is_cancelled(){let _=tx.send(Err(Box::new(error))).await;break;}}
          }
         },
         incoming=async { if no_active {tokio::time::timeout(limits.idle,reader.recv_group()).await} else {Ok(reader.recv_group().await)} },if !ended=>{
          let group=match incoming {Ok(Ok(Some(group)))=>group,Ok(Ok(None))=>{ended=true;if tasks.is_empty(){break;}continue;},Ok(Err(error))=>{let _=tx.send(Err(Box::new(error))).await;break;},Err(_)=>{let _=tx.send(Err(Box::new(Error::Timeout))).await;break;}};
          let sequence=group.sequence;
          if sequence<floor.borrow().0{continue;}
          // 并发耗尽时仅淘汰最旧组；取消任务后先回收任务槽，避免短暂超额启动。
          if tasks.len()>=limits.groups {
           if let Some((&id,(old,handle)))=active.iter().min_by_key(|(_,v)|v.0) {
            if sequence<=*old {continue;}
            handle.abort();active.remove(&id);
           }
           while tasks.len()>=limits.groups {if let Some(done)=tasks.join_next_with_id().await {match done {Ok((id,(_,Err(error))))=>{active.remove(&id);let _=tx.send(Err(error)).await;return;},Ok((id,_))=>{active.remove(&id);},Err(error)=>{active.remove(&error.id());if !error.is_cancelled(){let _=tx.send(Err(Box::new(error))).await;return;}}}}}
          }
          let handle=tasks.spawn(read_group(group,catalog.clone(),track,limits,tx.clone(),bytes.clone(),slots.clone(),floor.clone()));
          active.insert(handle.id(),(sequence,handle));
         }
        }
        if ended && tasks.is_empty() {
            break;
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn read_group(
    mut group: moq::group::Consumer,
    catalog: Arc<Catalog>,
    track: u16,
    limits: LiveLimits,
    tx: mpsc::Sender<Result<LiveEvent, ReceiveError>>,
    bytes: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    floor: watch::Sender<(u64, u64)>,
) -> (u64, Result<Option<&'static str>, ReceiveError>) {
    let sequence = group.sequence;
    let changed = floor.subscribe();
    let mut count = 0;
    let mut key = 0;
    let mut previous = 0;
    let result=async {
  loop {
   if sequence<changed.borrow().0{return Ok(Some("superseded"));}
   let received=Instant::now();
   let operation=async {
    let Some(mut packet)=group.next_frame().await? else{return Ok::<bool,ReceiveError>(false);};
    // 声明大小在本层read_all和媒体拷贝之前检查；上游缓存分配由MoQ自身配额负责。
    if packet.size>limits.packet_bytes as u64||packet.size<30 {return Err(Box::new(Error::Protocol("MoQ packet size")) as ReceiveError);}
    count+=1;if count>limits.group_frames {return Err(Box::new(Error::Protocol("MoQ group frame limit")) as ReceiveError);}
    let slot=slots.clone().acquire_owned().await?;
    let permit=bytes.clone().acquire_many_owned(packet.size as u32).await?;
    let data=packet.read_all().await?;
    let frame=decode_frame(data.as_ref(),&catalog).await?;
    if frame.track!=track||frame.sequence<=previous {return Err(Box::new(Error::Protocol("MoQ group track or order")) as ReceiveError);}
    if count==1 {
     if frame.kind!=FrameKind::Key {return Err(Box::new(Error::Protocol("MoQ group must start with key")) as ReceiveError);}
     key=frame.sequence;
     let mut invalid=false;
     floor.send_if_modified(|value|{
      if sequence>value.0 || value.1==0 {
       if frame.sequence<=value.1 {invalid=true;return false;}
       *value=(sequence,frame.sequence);true
      } else {false}
     });
     if invalid {return Err(Box::new(Error::Protocol("MoQ key sequence reversal")) as ReceiveError);}
    } else if frame.kind==FrameKind::Key||frame.dependencies.iter().any(|id|*id<key) {return Err(Box::new(Error::Protocol("MoQ cross-group dependency")) as ReceiveError);}
    previous=frame.sequence;
    if sequence<floor.borrow().0{return Ok(false);}
    tx.send(Ok(LiveEvent::Frame(LiveFrame{group:sequence,received,deadline:received+limits.residence,frame,_bytes:permit,_slot:slot}))).await.map_err(|_|Error::Protocol("receiver closed"))?;
    Ok(true)
   };
   tokio::select! {
    result=tokio::time::timeout(limits.residence,operation)=>match result {
     Ok(Ok(true))=>{},Ok(Ok(false))=>return Ok(None),Err(_)=>return Ok(Some("residence")),
     Ok(Err(error))=>{
      if let Some(moq::Error::Old|moq::Error::Lagged|moq::Error::Evicted|moq::Error::Dropped)=error.downcast_ref::<moq::Error>() {return Ok(Some("upstream_gap"));}
      return Err(error);
     }
    },
    _=superseded(changed.clone(),sequence)=>return Ok(Some("superseded"))
   }
  }
 }.await;
    (sequence, result)
}

// 只在更高组已验证关键帧时唤醒取消；自己的watch更新不能取消自己的关键帧发送。
async fn superseded(mut changed: watch::Receiver<(u64, u64)>, sequence: u64) {
    loop {
        if changed.borrow_and_update().0 > sequence {
            return;
        }
        if changed.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_native::media::{Track, TrackKind};
    struct Fixture {
        _origin: moq::origin::Producer,
        _broadcast: moq::broadcast::Producer,
        track: moq::track::Producer,
        receiver: LiveReceiver,
        driver: JoinHandle<moq::Error>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.driver.abort();
        }
    }
    fn catalog() -> Catalog {
        Catalog {
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                codec: "video/av1".into(),
                init: vec![],
                width: Some(320),
                height: Some(180),
                sample_rate: None,
                channels: None,
            }],
        }
    }
    fn fixture(limits: LiveLimits) -> Result<Fixture, ReceiveError> {
        let (origin, driver) =
            moq::origin::Producer::new(moq::origin::Config::new(moq::Hop::new(1).unwrap()));
        let driver = tokio::spawn(moq::time::run(driver));
        let broadcast = origin.create_broadcast("test/live")?;
        let track = broadcast.create_track(
            "video",
            Some(moq::track::Info::default().with_max_age(Duration::from_secs(10))),
        )?;
        let reader = track
            .subscribe(moq::track::Subscription::default().with_max_age(Duration::from_secs(10)));
        let receiver = LiveReceiver::new(reader, catalog(), 1, limits)?;
        Ok(Fixture {
            _origin: origin,
            _broadcast: broadcast,
            track,
            receiver,
            driver,
        })
    }
    fn key(sequence: u64) -> Frame {
        Frame {
            track: 1,
            sequence,
            timestamp_us: sequence * 66667,
            duration_us: 66667,
            kind: FrameKind::Key,
            dependencies: vec![],
            payload: vec![42; 32],
        }
    }
    async fn next(receiver: &mut LiveReceiver) -> Result<LiveFrame, ReceiveError> {
        loop {
            match tokio::time::timeout(Duration::from_millis(500), receiver.recv())
                .await?
                .ok_or("closed")??
            {
                LiveEvent::Frame(frame) => return Ok(frame),
                LiveEvent::Gap { .. } => {}
            }
        }
    }
    fn publish(track: &moq::track::Producer, sequence: u64) -> Result<(), ReceiveError> {
        let group = track.append_group()?;
        let mut group = group;
        group.write_frame(
            moq::Timestamp::from_micros(sequence * 66667)?,
            super::super::encode_frame(&key(sequence), &catalog())?,
        )?;
        group.finish()?;
        Ok(())
    }
    #[tokio::test]
    async fn newer_key_bypasses_partial_old_group_and_returns_permits() -> Result<(), ReceiveError>
    {
        let mut f = fixture(LiveLimits {
            residence: Duration::from_secs(1),
            ..LiveLimits::default()
        })?;
        let mut old = f.track.append_group()?;
        let pending = old.create_frame(moq::frame::Info {
            size: 1000,
            timestamp: moq::Timestamp::ZERO,
        })?;
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.receiver.leased_frames() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        publish(&f.track, 3)?;
        let received = next(&mut f.receiver).await?;
        assert_eq!(received.frame().sequence, 3);
        assert!(received.group > 0);
        drop(received);
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.receiver.leased_bytes() != 0 || f.receiver.leased_frames() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        drop(pending);
        Ok(())
    }
    #[tokio::test]
    async fn declared_oversize_fails_before_waiting_for_payload() -> Result<(), ReceiveError> {
        let mut f = fixture(LiveLimits {
            residence: Duration::from_secs(1),
            ..LiveLimits::default()
        })?;
        let mut group = f.track.append_group()?;
        let _pending = group.create_frame(moq::frame::Info {
            size: (f.receiver.limits.packet_bytes + 1) as u64,
            timestamp: moq::Timestamp::ZERO,
        })?;
        let result = tokio::time::timeout(Duration::from_millis(500), f.receiver.recv())
            .await?
            .ok_or("closed")?;
        assert!(result.is_err());
        assert_eq!(f.receiver.leased_bytes(), 0);
        assert_eq!(f.receiver.leased_frames(), 0);
        Ok(())
    }
    #[tokio::test]
    async fn held_frames_apply_backpressure_and_release_allows_new_key() -> Result<(), ReceiveError>
    {
        let mut f = fixture(LiveLimits {
            groups: 2,
            frames: 2,
            residence: Duration::from_secs(1),
            ..LiveLimits::default()
        })?;
        publish(&f.track, 1)?;
        let first = next(&mut f.receiver).await?;
        publish(&f.track, 2)?;
        let second = next(&mut f.receiver).await?;
        assert_eq!(f.receiver.leased_frames(), 2);
        publish(&f.track, 3)?;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.receiver.recv())
                .await
                .is_err()
        );
        drop(first);
        let third = next(&mut f.receiver).await?;
        assert_eq!(third.frame().sequence, 3);
        drop((second, third));
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.receiver.leased_frames() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        Ok(())
    }
    #[tokio::test]
    async fn drop_cancels_stalled_reader_and_releases_byte_lease() -> Result<(), ReceiveError> {
        let f = fixture(LiveLimits {
            residence: Duration::from_secs(1),
            ..LiveLimits::default()
        })?;
        // 单独持有待测试接收器，以实际Drop触发取消，而不是直接调用abort。
        let receiver = LiveReceiver::new(
            f.track.subscribe(
                moq::track::Subscription::default().with_max_age(Duration::from_secs(10)),
            ),
            catalog(),
            1,
            LiveLimits {
                residence: Duration::from_secs(1),
                ..LiveLimits::default()
            },
        )?;
        let mut group = f.track.append_group()?;
        let _pending = group.create_frame(moq::frame::Info {
            size: 1000,
            timestamp: moq::Timestamp::ZERO,
        })?;
        tokio::time::timeout(Duration::from_millis(500), async {
            while receiver.leased_bytes() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let credits = receiver.bytes.clone();
        let total = receiver.limits.bytes;
        drop(receiver);
        let _all = tokio::time::timeout(
            Duration::from_millis(500),
            credits.acquire_many_owned(total as u32),
        )
        .await??;
        Ok(())
    }
    #[tokio::test]
    async fn finished_empty_track_closes_receiver() -> Result<(), ReceiveError> {
        let mut f = fixture(LiveLimits::default())?;
        f.track.finish()?;
        assert!(
            tokio::time::timeout(Duration::from_millis(500), f.receiver.recv())
                .await?
                .is_none()
        );
        Ok(())
    }
    #[tokio::test]
    async fn residence_expiry_is_local_and_next_group_still_arrives() -> Result<(), ReceiveError> {
        let mut f = fixture(LiveLimits {
            residence: Duration::from_millis(50),
            ..LiveLimits::default()
        })?;
        let mut group = f.track.append_group()?;
        let _pending = group.create_frame(moq::frame::Info {
            size: 1000,
            timestamp: moq::Timestamp::ZERO,
        })?;
        let event = tokio::time::timeout(Duration::from_millis(500), f.receiver.recv())
            .await?
            .ok_or("closed")??;
        assert!(matches!(
            event,
            LiveEvent::Gap {
                reason: "residence",
                ..
            }
        ));
        assert_eq!(f.receiver.leased_bytes(), 0);
        publish(&f.track, 3)?;
        assert_eq!(next(&mut f.receiver).await?.frame().sequence, 3);
        Ok(())
    }
    #[tokio::test]
    async fn bounded_concurrency_evicts_oldest_stalled_group() -> Result<(), ReceiveError> {
        let mut f = fixture(LiveLimits {
            groups: 2,
            residence: Duration::from_secs(1),
            ..LiveLimits::default()
        })?;
        let mut g0 = f.track.append_group()?;
        let _p0 = g0.create_frame(moq::frame::Info {
            size: 1000,
            timestamp: moq::Timestamp::ZERO,
        })?;
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.receiver.leased_frames() < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let mut g1 = f.track.append_group()?;
        let _p1 = g1.create_frame(moq::frame::Info {
            size: 1000,
            timestamp: moq::Timestamp::ZERO,
        })?;
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.receiver.leased_frames() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        publish(&f.track, 5)?;
        assert_eq!(next(&mut f.receiver).await?.frame().sequence, 5);
        Ok(())
    }
    #[tokio::test]
    async fn queued_frame_expires_before_delivery() -> Result<(), ReceiveError> {
        let mut f = fixture(LiveLimits {
            residence: Duration::from_millis(50),
            ..LiveLimits::default()
        })?;
        publish(&f.track, 1)?;
        tokio::time::timeout(Duration::from_millis(500), async {
            while f.receiver.leased_frames() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert!(matches!(
            f.receiver.recv().await.ok_or("closed")??,
            LiveEvent::Gap {
                reason: "queue_expired",
                ..
            }
        ));
        assert_eq!(f.receiver.leased_frames(), 0);
        Ok(())
    }
    #[tokio::test]
    async fn newer_group_cannot_roll_back_media_key_sequence() -> Result<(), ReceiveError> {
        let mut f = fixture(LiveLimits::default())?;
        publish(&f.track, 5)?;
        drop(next(&mut f.receiver).await?);
        publish(&f.track, 3)?;
        let result = tokio::time::timeout(Duration::from_millis(500), f.receiver.recv())
            .await?
            .ok_or("closed")?;
        let error = match result {
            Err(error) => error,
            Ok(_) => return Err("expected sequence error".into()),
        };
        assert!(matches!(
            error.downcast_ref::<Error>(),
            Some(Error::Protocol("MoQ key sequence reversal"))
        ));
        Ok(())
    }
}
