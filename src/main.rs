#![allow(dead_code)]

mod client;
mod config;
mod layout;
mod pane;
mod protocol;
mod server;




fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match cmd {
        "server" => server::run(),
        "kill" => {
            use std::io::Write;
            let mut s = std::os::unix::net::UnixStream::connect(protocol::socket_path())?;
            writeln!(
                s,
                "{}",
                serde_json::to_string(&protocol::ClientMsg::Kill)?
            )?;
            Ok(())
        }


       
        "attach" | "a" | "" => {
            if std::os::unix::net::UnixStream::connect(protocol::socket_path()).is_err() {
                let exe = std::env::current_exe()?;
                // pre_exec: отвязаться от терминала ДО exec, чтобы сервер
                // не получал SIGHUP при закрытии клиента.
                use std::os::unix::process::CommandExt;



                let mut cmd = std::process::Command::new(exe);
                cmd.arg("server")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                unsafe {
                    cmd.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
                let child = cmd.spawn()?;



                // Не ждать ребёнка — он должен жить сам по себе
                std::mem::forget(child);
                for _ in 0..60 {
                    if std::os::unix::net::UnixStream::connect(protocol::socket_path()).is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            client::run()
        }



        _ => {
            eprintln!("Usage: cmdnest [server|attach|kill]");
            Ok(())
        }
    }
}
