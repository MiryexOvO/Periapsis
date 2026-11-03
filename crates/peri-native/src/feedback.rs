//! Bounded feedback on the retained, authenticated control stream.
use crate::{Control, Error, wire};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
pub(crate) struct Feedback {
    send: AsyncMutex<(quinn::SendStream, Option<Instant>)>,
    inbox: Mutex<mpsc::Receiver<u16>>,
    reader: tokio::task::JoinHandle<()>,
    connection: quinn::Connection,
}
impl Feedback {
    pub fn new(
        send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        connection: quinn::Connection,
        publisher: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel(8);
        let reader_connection = connection.clone();
        let reader = tokio::spawn(async move {
            let mut window = Instant::now();
            let mut count = 0;
            loop {
                let result = wire::read_control::<_, Control>(&mut recv).await;
                if window.elapsed() >= Duration::from_secs(1) {
                    window = Instant::now();
                    count = 0;
                }
                count += 1;
                match result {
                    Ok(Control::Keyframe { track }) if publisher && track > 0 && count <= 10 => {
                        let _ = tx.try_send(track);
                    }
                    _ => {
                        reader_connection.close(2u32.into(), b"invalid feedback");
                        break;
                    }
                }
            }
        });
        Self {
            send: AsyncMutex::new((send, None)),
            inbox: Mutex::new(rx),
            reader,
            connection,
        }
    }
    pub fn poll(&self) -> Option<u16> {
        self.inbox.lock().expect("feedback inbox").try_recv().ok()
    }
    pub async fn request(&self, track: u16) -> Result<(), Error> {
        if track == 0 {
            return Err(Error::Protocol("invalid feedback track"));
        }
        let mut state = self.send.lock().await;
        if state
            .1
            .is_some_and(|last| last.elapsed() < Duration::from_millis(200))
        {
            return Ok(());
        }
        struct WriteGuard(Option<quinn::Connection>);
        impl Drop for WriteGuard {
            fn drop(&mut self) {
                if let Some(connection) = self.0.take() {
                    connection.close(2u32.into(), b"feedback write cancelled");
                }
            }
        }
        let mut guard = WriteGuard(Some(self.connection.clone()));
        // Cancelled partial writes cannot be reused: close the connection on failure.
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            wire::write_control(&mut state.0, &Control::Keyframe { track }),
        )
        .await;
        match result {
            Ok(Ok(())) => {
                guard.0 = None;
                state.1 = Some(Instant::now());
                Ok(())
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(Error::Timeout),
        }
    }
}
impl Drop for Feedback {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
