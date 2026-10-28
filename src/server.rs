use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::protocol::*;

const HISTORY_LIMIT: usize = 5 * 1024 * 1024;

type ClientWriter = Arc<Mutex<UnixStream>>;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Отвязаться от терминала
    unsafe {
        libc::setsid();
    }

    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;

    // PTY
    let pty_sys = native_pty_system();
    let (cols, rows) = (80u16, 24u16);
    let pair = pty_sys.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let _child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer()?));
    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));
    let history: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let clients: Arc<Mutex<HashMap<usize, ClientWriter>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(Mutex::new(0usize));
    let pty_size: Arc<Mutex<(u16, u16)>> = Arc::new(Mutex::new((cols, rows)));

    // PTY reader
    let mut reader = master.lock().unwrap().try_clone_reader()?;
    let history_cl = Arc::clone(&history);
    let clients_cl = Arc::clone(&clients);
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    {
                        let mut h = history_cl.lock().unwrap();
                        h.extend_from_slice(&data);
                        if h.len() > HISTORY_LIMIT {
                            let excess = h.len() - HISTORY_LIMIT;
                            h.drain(0..excess);
                        }
                    }
                    let msg = serde_json::to_string(&ServerMsg::PtyOutput { data }).unwrap();
                    let mut cls = clients_cl.lock().unwrap();
                    cls.retain(|_, w| {
                        let mut w = match w.lock() {
                            Ok(g) => g,
                            Err(_) => return false,
                        };
                        writeln!(*w, "{}", msg).is_ok()
                    });
                }
                Err(_) => break,
            }
        }
    });

    // Accept loop
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let id = {
            let mut n = next_id.lock().unwrap();
            let id = *n;
            *n += 1;
            id
        };
        let writer = Arc::clone(&writer);
        let master = Arc::clone(&master);
        let history = Arc::clone(&history);
        let clients = Arc::clone(&clients);
        let pty_size = Arc::clone(&pty_size);
        thread::spawn(move || {
            handle_client(id, stream, writer, master, history, clients, pty_size);
        });
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn handle_client(
    id: usize,
    stream: UnixStream,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    history: Arc<Mutex<Vec<u8>>>,
    clients: Arc<Mutex<HashMap<usize, ClientWriter>>>,
    pty_size: Arc<Mutex<(u16, u16)>>,
) {
    let write_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let write_stream: ClientWriter = Arc::new(Mutex::new(write_stream));

    // Init
    let (cols, rows) = *pty_size.lock().unwrap();
    let init = ServerMsg::Init {
        history: history.lock().unwrap().clone(),
        cols,
        rows,
    };
    {
        let mut w = write_stream.lock().unwrap();
        if writeln!(*w, "{}", serde_json::to_string(&init).unwrap()).is_err() {
            return;
        }
    }

    clients.lock().unwrap().insert(id, Arc::clone(&write_stream));

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let msg: ClientMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        match msg {
            ClientMsg::Input { data } => {
                let mut w = pty_writer.lock().unwrap();
                let _ = w.write_all(&data);
                let _ = w.flush();
            }
            








ClientMsg::Resize { cols, rows } => {
    *pty_size.lock().unwrap() = (cols, rows);
    let _ = pty_master.lock().unwrap().resize(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    });
}       




            ClientMsg::Detach => break,
            ClientMsg::Kill => {
                std::process::exit(0);
            }
        }
    }

    clients.lock().unwrap().remove(&id);
}
