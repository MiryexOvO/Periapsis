//! Line-oriented control responder for cross-language interoperability checks.
//! 用于跨语言互通检查的逐行控制消息响应端，标准输出只承载协议对象。
use peri_session::{
    handshake::{Handshake, Role, State},
    wire,
};
use peri_types::{Hello, PERI_VERSION, Profile};
use std::io::{self, BufRead, Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id = std::env::args().nth(1).ok_or("missing session id")?;
    let local = Hello {
        version: PERI_VERSION,
        profiles: vec![Profile::Live],
        codecs: vec!["application/octet-stream".into()],
        extensions: vec![],
    };
    let mut session = Handshake::new(Role::Responder, id, local)?;
    session.transport_ready()?;
    session.start()?;
    // take 限制读取分配；过长行不能先被无限缓存再做大小判断。
    let mut input = io::stdin().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = input
            .by_ref()
            .take((peri_types::MAX_CONTROL_BYTES + 2) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err("control stream ended before activation".into());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let message = wire::decode(&line)?;
        if let Some(reply) = session.receive(message)? {
            let bytes = wire::encode(&reply)?;
            io::stdout().write_all(&bytes)?;
            io::stdout().write_all(b"\n")?;
            io::stdout().flush()?;
        }
        if session.state() == State::Active {
            println!("{{\"state\":\"ACTIVE\"}}");
            return Ok(());
        }
    }
}
