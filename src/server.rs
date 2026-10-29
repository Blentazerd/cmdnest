use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::layout::Rect;

use crate::layout::{Layout, NavDir, ResizeDir, SplitDir};
use crate::protocol::*;

const HISTORY_LIMIT: usize = 5 * 1024 * 1024;

struct PaneData {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    _child: Box<dyn Child + Send + Sync>,
    history: Arc<Mutex<Vec<u8>>>,
    reader_started: bool,

}

struct TabData {
    id: u64,
    layout: Layout,
    panes: HashMap<u64, PaneData>,
    focused: u64,
    zoomed: bool,
}



impl TabData {
    fn resize_all(&mut self, area: Rect) {
        let mut rects = Vec::new();
        self.layout.compute(area, &mut rects);
        for (id, rect) in &rects {
            if let Some(p) = self.panes.get(id) {
                let (rows, cols) = if self.zoomed {
                    if *id == self.focused {
                        (
                            area.height.saturating_sub(2).max(1),
                            area.width.saturating_sub(2).max(1),
                        )
                    } else {
                        (1, 1)
                    }
                } else {
                    (
                        rect.height.saturating_sub(2).max(1),
                        rect.width.saturating_sub(2).max(1),
                    )
                };
                let _ = p.master.lock().unwrap().resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
    }




    fn snapshot(&self, area: Rect) -> TabSnap {
        let mut rects = Vec::new();
        self.layout.compute(area, &mut rects);
        let panes = rects
            .into_iter()
            .filter_map(|(id, r)| {
                let (effective, hidden) = if self.zoomed {
                    if id == self.focused {
                        (Rect::new(0, 0, area.width, area.height), false)
                    } else {
                        (r, true)
                    }
                } else {
                    (r, false)
                };
                self.panes.get(&id).map(|p| PaneSnap {
                    id,
                    rect: RectS {
                        x: effective.x,
                        y: effective.y,
                        w: effective.width,
                        h: effective.height,
                    },
                    history: p.history.lock().unwrap().clone(),
                    hidden,
                })
            })
            .collect();
        TabSnap {
            id: self.id,
            focused: self.focused,
            panes,
            zoomed: self.zoomed,
        }
    }




}



struct State {
    tabs: Vec<TabData>,
    active: u64,
    next_tab_id: u64,
    next_pane_id: u64,
    cols: u16,
    rows: u16,
    clients: HashMap<usize, Arc<Mutex<UnixStream>>>,
    next_client_id: usize,
}

impl State {
    fn main_area(&self) -> Rect {
        Rect::new(0, 0, self.cols, self.rows.saturating_sub(1))
    }

    fn snapshot(&self) -> Snapshot {
        let area = self.main_area();
        Snapshot {
            tabs: self.tabs.iter().map(|t| t.snapshot(area)).collect(),
            active: self.active,
            cols: self.cols,
            rows: self.rows,
        }
    }

    fn broadcast_snapshot(&self) {
        let snap = self.snapshot();
        let msg = match serde_json::to_string(&ServerMsg::Snapshot { snap }) {
            Ok(m) => m,
            Err(_) => return,
        };
        for w in self.clients.values() {
            if let Ok(mut s) = w.lock() {
                let _ = writeln!(*s, "{}", msg);
            }
        }
    }
}

fn create_pane(
    next_id: &mut u64,
    rows: u16,
    cols: u16,
) -> Result<(u64, PaneData), Box<dyn std::error::Error>> {
    let pty_sys = native_pty_system();
    let pair = pty_sys.openpty(PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer()?));
    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));
    let history: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let id = *next_id;
    *next_id += 1;

    Ok((
        id,


        PaneData {
            writer,
            master,
            _child: child,
            history,
            reader_started: false,
        },


    ))
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        libc::setsid();
    }


    unsafe {
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }


    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;

    let mut next_pane_id = 1u64;
    let (first_id, first_pane) = create_pane(&mut next_pane_id, 24, 80)?;
    let first_tab = TabData {
        id: 1,
        layout: Layout::leaf(first_id),
        panes: {
            let mut m = HashMap::new();
            m.insert(first_id, first_pane);
            m
        },
        focused: first_id,
        zoomed: false,

    };

    let state = Arc::new(Mutex::new(State {
        tabs: vec![first_tab],
        active: 1,
        next_tab_id: 2,
        next_pane_id,
        cols: 80,
        rows: 24,
        clients: HashMap::new(),
        next_client_id: 0,
    }));

    // PTY readers





    // Запуск readers для всех панелей первого tab'а
    let initial_panes: Vec<(u64, u64)> = {
        let s = state.lock().unwrap();
        s.tabs
            .iter()
            .flat_map(|t| t.panes.keys().map(move |pid| (t.id, *pid)))
            .collect()
    };
    for (tid, pid) in initial_panes {
        spawn_pty_reader(Arc::clone(&state), tid, pid);
    }





    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let state_c = Arc::clone(&state);
        thread::spawn(move || {
            let _ = handle_client(stream, state_c);
        });
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn spawn_pty_reader(state: Arc<Mutex<State>>, tab_id: u64, pane_id: u64) {
    let (reader_opt, history) = {
        let mut st = state.lock().unwrap();
        let Some(tab) = st.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        let Some(pane) = tab.panes.get_mut(&pane_id) else {
            return;
        };
        if pane.reader_started {
            return;
        }
        pane.reader_started = true;
        let reader = pane.master.lock().unwrap().try_clone_reader();
        (reader, Arc::clone(&pane.history))
    };
    let Ok(mut r) = reader_opt else { return };
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    {
                        let mut h = history.lock().unwrap();
                        h.extend_from_slice(&data);
                        if h.len() > HISTORY_LIMIT {
                            let ex = h.len() - HISTORY_LIMIT;
                            h.drain(0..ex);
                        }
                    }
                    let msg = serde_json::to_string(&ServerMsg::PaneOutput {
                        tab: tab_id,
                        pane: pane_id,
                        data,
                    })
                    .unwrap();
                    let clients: Vec<(usize, Arc<Mutex<UnixStream>>)> = {
                        let st = state.lock().unwrap();
                        st.clients.iter().map(|(k, v)| (*k, v.clone())).collect()
                    };
                    let mut dead = Vec::new();
                    for (id, w) in &clients {
                        if let Ok(mut s) = w.lock() {
                            if writeln!(*s, "{}", msg).is_err() {
                                dead.push(*id);
                            }
                        } else {
                            dead.push(*id);
                        }
                    }
                    if !dead.is_empty() {
                        let mut st = state.lock().unwrap();
                        for id in dead {
                            st.clients.remove(&id);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
}



fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let write_stream = stream.try_clone()?;
    let write_stream: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(write_stream));

    let cid = {
        let mut st = state.lock().unwrap();
        let cid = st.next_client_id;
        st.next_client_id += 1;
        st.clients.insert(cid, Arc::clone(&write_stream));
        cid
    };

    {
        let st = state.lock().unwrap();
        let snap = st.snapshot();
        drop(st);
        let msg = serde_json::to_string(&ServerMsg::Snapshot { snap })?;
        let mut w = write_stream.lock().unwrap();
        writeln!(*w, "{}", msg)?;
    }

    let reader = BufReader::new(stream);
    let _ = write_stream.lock().unwrap().flush();   
 for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let msg: ClientMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mut st = state.lock().unwrap();
        match msg {
            ClientMsg::Input { data } => {
                if let Some(tab) = st.tabs.iter().find(|t| t.id == st.active) {
                    if let Some(p) = tab.panes.get(&tab.focused) {
                        if let Ok(mut w) = p.writer.lock() {
                            let _ = w.write_all(&data);
                            let _ = w.flush();
                        }
                    }
                }
            }
            ClientMsg::Resize { cols, rows } => {
                st.cols = cols.max(1);
                st.rows = rows.max(1);
                let area = st.main_area();
                for tab in &mut st.tabs {
                    tab.resize_all(area);
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }
            ClientMsg::Detach => break,
            ClientMsg::Kill => std::process::exit(0),
            ClientMsg::NewTab => {
                let area = st.main_area();
                let mut npid = st.next_pane_id;
                let (pid, pane) = create_pane(
                    &mut npid,
                    area.height.saturating_sub(2),
                    area.width.saturating_sub(2),
                )?;
                st.next_pane_id = npid;
                let tid = st.next_tab_id;
                st.next_tab_id += 1;
                let mut m = HashMap::new();
                m.insert(pid, pane);




                let mut tab = TabData {
                    id: tid,
                    layout: Layout::leaf(pid),
                    panes: m,
                    focused: pid,
                    zoomed: false,
                };



                tab.resize_all(area);


                st.tabs.push(tab);
                st.active = tid;
                drop(st);
                spawn_pty_reader(Arc::clone(&state), tid, pid);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();



            }
            ClientMsg::NextTab => {
                if let Some(idx) = st.tabs.iter().position(|t| t.id == st.active) {
                    if idx + 1 < st.tabs.len() {
                        st.active = st.tabs[idx + 1].id;
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }
            ClientMsg::PrevTab => {
                if let Some(idx) = st.tabs.iter().position(|t| t.id == st.active) {
                    if idx > 0 {
                        st.active = st.tabs[idx - 1].id;
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }
            ClientMsg::CloseTab => {
                if st.tabs.len() > 1 {
                    if let Some(idx) = st.tabs.iter().position(|t| t.id == st.active) {
                        st.tabs.remove(idx);
                        if idx >= st.tabs.len() {
                            st.active = st.tabs[st.tabs.len() - 1].id;
                        } else {
                            st.active = st.tabs[idx].id;
                        }
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }
            ClientMsg::SplitPane { dir } => {
                let area = st.main_area();
                let split_dir = match dir {
                    SplitDirP::Vertical => SplitDir::Vertical,
                    SplitDirP::Horizontal => SplitDir::Horizontal,
                };
                let mut npid = st.next_pane_id;
                let (new_pid, new_pane) = create_pane(&mut npid, 1, 1)?;
                st.next_pane_id = npid;
                let active_id = st.active;




                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    tab.zoomed = false;
                    if tab.layout.split_at(tab.focused, new_pid, split_dir) {
                        tab.panes.insert(new_pid, new_pane);
                        tab.focused = new_pid;
                        let a = area;
                        tab.resize_all(a);
                    }
                }
                drop(st);
                spawn_pty_reader(Arc::clone(&state), active_id, new_pid);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();





            }



            ClientMsg::ClosePane => {
                let active_id = st.active;
                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    if tab.panes.len() > 1 && tab.layout.close_at(tab.focused) {
                        tab.panes.remove(&tab.focused);
                        if let Some(first) = tab.panes.keys().next() {
                            tab.focused = *first;
                        }
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }




            ClientMsg::FocusPane { dir } => {
                let area = st.main_area();
                let nd = match dir {
                    NavDirP::Left => NavDir::Left,
                    NavDirP::Right => NavDir::Right,
                    NavDirP::Up => NavDir::Up,
                    NavDirP::Down => NavDir::Down,
                };
                let active_id = st.active;
                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    if let Some(n) = tab.layout.find_neighbor(area, tab.focused, nd) {
                        tab.focused = n;
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }
            ClientMsg::ResizePane { dir } => {
                let area = st.main_area();
                let rd = match dir {
                    ResizeDirP::Left => ResizeDir::Left,
                    ResizeDirP::Right => ResizeDir::Right,
                    ResizeDirP::Up => ResizeDir::Up,
                    ResizeDirP::Down => ResizeDir::Down,
                };
                let active_id = st.active;
                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    if tab.layout.adjust_ratio(tab.focused, rd, 0.03) {
                        let a = area;
                        tab.resize_all(a);
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }





            ClientMsg::SetFocus { pane_id } => {
                let active_id = st.active;
                let area = st.main_area();
                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    if tab.panes.contains_key(&pane_id) {
                        tab.focused = pane_id;
                        if tab.zoomed {
                            tab.resize_all(area);
                        }
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }







            ClientMsg::ZoomPane => {
                let active_id = st.active;
                let area = st.main_area();
                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    if !tab.zoomed {
                        tab.zoomed = true;
                        tab.resize_all(area);
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }






            ClientMsg::UnzoomPane => {
                let active_id = st.active;
                let area = st.main_area();
                if let Some(tab) = st.tabs.iter_mut().find(|t| t.id == active_id) {
                    if tab.zoomed {
                        tab.zoomed = false;
                        tab.resize_all(area);
                    }
                }
                drop(st);
                let st = state.lock().unwrap();
                st.broadcast_snapshot();
            }





        }
    }

    let mut st = state.lock().unwrap();
    st.clients.remove(&cid);
    Ok(())
}
