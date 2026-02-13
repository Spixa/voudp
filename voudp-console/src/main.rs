use std::{
    io::{Write, stdout},
    net::ToSocketAddrs,
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};

use voudp::socket::SecureUdpSocket;
use voudp::util::{self};
use voudp::{protocol::VOUDP_SALT, socket};

enum LogMsg {
    Line(String),
    Shutdown,
}

struct Console {
    logs: Vec<String>,
    input: String,
}

impl Console {
    fn new() -> Self {
        Self {
            logs: Vec::new(),
            input: String::new(),
        }
    }

    fn push_log(&mut self, line: impl Into<String>) {
        const MAX_LOGS: usize = 10_000; // prevent unbounded memory growth
        self.logs.push(line.into());
        if self.logs.len() > MAX_LOGS {
            self.logs.drain(..self.logs.len() - MAX_LOGS);
        }
    }
}

fn render(console: &Console) -> std::io::Result<()> {
    let mut out = stdout();
    let (w, h) = size()?;
    let log_height = h.saturating_sub(1) as usize;

    execute!(out, Hide, MoveTo(0, 0), Clear(ClearType::All))?;

    let start = console.logs.len().saturating_sub(log_height);

    for (i, line) in console.logs[start..].iter().enumerate() {
        execute!(out, MoveTo(0, i as u16))?; // go to i'th line

        // UTF-8 safe truncation
        let trunc: String = line.chars().take(w as usize).collect();

        // decoded voudp-aux packet:
        let color = if trunc.starts_with("voudp-aux") {
            Color::White
        } else if trunc.starts_with("Executing") {
            Color::DarkGrey
        } else {
            Color::Green
        };

        execute!(out, SetForegroundColor(color))?;
        write!(out, "{trunc}")?;
        execute!(out, ResetColor)?;
    }

    // render input on bottom line (never wraps)
    execute!(out, MoveTo(0, h - 1))?;
    let input: String = console.input.chars().take(w as usize).collect();
    execute!(out, SetForegroundColor(Color::Yellow))?;
    write!(out, "> ")?;
    execute!(out, ResetColor)?;
    write!(out, "{input}")?;

    out.flush()?;
    Ok(())
}

fn main() -> Result<(), std::io::Error> {
    let ip: String = {
        let input = util::ask("Enter address (default 127.0.0.1:37549): ");
        if input.trim().is_empty() {
            "127.0.0.1:37549".to_string()
        } else {
            input
        }
    };

    let phrase: String = {
        let input = util::ask("Enter phrase (default voudp): ");
        if input.trim().is_empty() {
            "voudp".to_string()
        } else {
            input
        }
    };

    let password: String = {
        let input = util::ask("Enter console password (default `password`): ");
        if input.trim().is_empty() {
            "password".to_string()
        } else {
            input
        }
    };

    println!("Generating key...");

    let key = socket::derive_key_from_phrase(phrase.as_bytes(), VOUDP_SALT);
    let socket = SecureUdpSocket::create("0.0.0.0:0".to_owned(), key)?;
    // socket.connect(ip.clone())?;

    let server_addr = ip
        .to_socket_addrs()
        .unwrap_or_default()
        .find(|a| a.is_ipv4())
        .unwrap();

    let mut register_packet = vec![0xff];
    register_packet.extend_from_slice(password.as_bytes());
    let _ = socket.send_to(&register_packet, server_addr);

    // terminal setup
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;

    // panic hook so terminal always restores
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            Show,
            LeaveAlternateScreen,
            Clear(ClearType::All),
            MoveTo(0, 0)
        );
        default_hook(info);
    }));

    // recv thread
    let (tx, rx) = std::sync::mpsc::channel::<LogMsg>();
    {
        let socket = socket.clone();
        let tx = tx.clone();

        thread::spawn(move || {
            loop {
                let mut buf = [0u8; 2048];
                match socket.recv_from(&mut buf) {
                    Ok((len, addr)) => {
                        if server_addr == addr && len > 0 {
                            if let Ok(string) = String::from_utf8(buf[..len].to_vec()) {
                                if tx.send(LogMsg::Line(string)).is_err() {
                                    break;
                                }
                            } else if tx.send(LogMsg::Line("CORRUPTED MESSAGE".into())).is_err() {
                                break;
                            }
                        }
                    }
                    Err(ref e) if e.0.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => {
                        let _ = tx.send(LogMsg::Line(format!("SOCKET ERROR: {}", e.0)));
                        let _ = tx.send(LogMsg::Shutdown);
                        break;
                    }
                }

                thread::sleep(Duration::from_millis(5));
            }
        });
    }

    let mut console = Console::new();
    console.push_log("Connected to server");

    let mut running = true;

    let mut timer = Instant::now();
    while running {
        // drain logs from recv thread
        while let Ok(msg) = rx.try_recv() {
            match msg {
                LogMsg::Line(line) => console.push_log(format!(
                    "voudp-aux [{server_addr}] <-> [{}] recv: {line}",
                    socket.local_addr(),
                )),
                LogMsg::Shutdown => running = false,
            }
        }

        if timer.elapsed() >= Duration::from_secs(1) {
            let _ = socket.send_to(&[0x04], server_addr);
            timer = Instant::now();
        }

        render(&console)?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+C exit
                        let _ = socket.send_to(&[0x03], server_addr);
                        running = false;
                    }
                    KeyCode::Char(c) => console.input.push(c),
                    KeyCode::Backspace => {
                        console.input.pop();
                    }
                    KeyCode::Enter => {
                        let cmd = std::mem::take(&mut console.input);

                        // echo locally
                        console.push_log(format!("Executing '{cmd}' as console"));

                        // send to server
                        let mut packet = vec![0x0d];
                        packet.extend_from_slice(cmd.as_bytes());
                        let _ = socket.send_to(&packet, server_addr);

                        if cmd.trim() == "quit" {
                            let _ = socket.send_to(&[0x03], server_addr);
                            running = false;
                        }
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {
                    // just redraw on resize
                }
                _ => {}
            }
        }
    }

    // restore terminal
    disable_raw_mode()?;
    execute!(
        stdout(),
        Show,
        LeaveAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )?;

    stdout().flush()?;

    Ok(())
}
