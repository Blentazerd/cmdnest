#![allow(dead_code)]

mod client;
mod config;
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
                // Запустить сервер
                let exe = std::env::current_exe()?;
                std::process::Command::new(exe)
                    .arg("server")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::inherit())
                    .spawn()?;
                // Ждём готовности сокета до 3 секунд
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
