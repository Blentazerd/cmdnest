use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    Input { data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Detach,
    Kill,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMsg {
    Init { history: Vec<u8>, cols: u16, rows: u16 },
    PtyOutput { data: Vec<u8> },
    Error { msg: String },
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join(format!("cmdnest-{}.sock", unsafe { libc::getuid() }))
}
