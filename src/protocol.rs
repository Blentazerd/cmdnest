use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum SplitDirP {
    Vertical,
    Horizontal,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum NavDirP {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum ResizeDirP {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct RectS {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    Input { data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Detach,
    Kill,
    NewTab,
    NextTab,
    PrevTab,
    CloseTab,
    SplitPane { dir: SplitDirP },
    ClosePane,
    FocusPane { dir: NavDirP },
    ResizePane { dir: ResizeDirP },
    SetFocus { pane_id: u64 },
    ZoomPane,
    UnzoomPane,

}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMsg {
    Snapshot { snap: Snapshot },
    PaneOutput { tab: u64, pane: u64, data: Vec<u8> },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Snapshot {
    pub tabs: Vec<TabSnap>,
    pub active: u64,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TabSnap {
    pub id: u64,
    pub focused: u64,
    pub panes: Vec<PaneSnap>,
    pub zoomed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]



pub struct PaneSnap {
    pub id: u64,
    pub rect: RectS,
    pub history: Vec<u8>,
    pub hidden: bool,
}




pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join(format!("cmdnest-{}.sock", unsafe { libc::getuid() }))
}
