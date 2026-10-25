//! blotterd: keep the board of Claude Code sessions and serve it.
//!
//!   POST /events             one event from blotter-hook
//!   GET  /state[?after=N]    the board; with `after`, waits (up to 25 s) for a version newer
//!                            than N, so clients hear about changes without polling
//!   GET  /healthz            "ok", no token needed
//!
//! Every other request needs `Authorization: Bearer <token>`. The board is saved after each
//! change, so a restart doesn't blank the screen.
//!
//! Usage: blotterd [--config PATH]    (default ~/.config/blotter/config.toml)

use blotter_proto::{now_ms, parse_duration, Board, Config, Event};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const LONG_POLL: Duration = Duration::from_secs(25);
const MAX_BODY: u64 = 64 * 1024;

struct Shared {
    board: Mutex<Board>,
    changed: Condvar,
    token: String,
    state_file: PathBuf,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("blotterd: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = Config::default_path();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = args.next().ok_or("--config needs a path")?.into(),
            "-h" | "--help" => {
                println!("blotterd [--config PATH]");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    let config = Config::load(&config_path)?;
    let forget_after = parse_duration(&config.feed.forget_after)?;

    let state_file = config.state_file();
    let board = load_board(&state_file);
    eprintln!("blotterd: {} sessions from {}", board.sessions.len(), state_file.display());

    let shared = Arc::new(Shared {
        board: Mutex::new(board),
        changed: Condvar::new(),
        token: config.feed.token.clone(),
        state_file,
    });

    // Forget sessions that died without a SessionEnd.
    let pruner = Arc::clone(&shared);
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(60));
        let mut board = pruner.board.lock().unwrap();
        if board.prune(now_ms(), forget_after) {
            pruner.save(&board);
            pruner.changed.notify_all();
        }
    });

    let server = Server::http(&config.feed.listen).map_err(|e| format!("listening on {}: {}", config.feed.listen, e))?;
    eprintln!("blotterd: listening on {}", config.feed.listen);
    for request in server.incoming_requests() {
        let shared = Arc::clone(&shared);
        // A thread per request: long polls wait without holding up anyone else.
        thread::spawn(move || handle(&shared, request));
    }
    Ok(())
}

fn handle(shared: &Shared, mut request: Request) {
    let url = request.url().to_owned();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));

    if path == "/healthz" {
        let _ = request.respond(Response::from_string("ok\n"));
        return;
    }
    if !authorised(&request, &shared.token) {
        let _ = request.respond(Response::from_string("unauthorised\n").with_status_code(401));
        return;
    }

    let response = match (request.method(), path) {
        (Method::Post, "/events") => post_event(shared, &mut request),
        (Method::Get, "/state") => get_state(shared, query),
        _ => Response::from_string("not found\n").with_status_code(404),
    };
    let _ = request.respond(response);
}

fn authorised(request: &Request, token: &str) -> bool {
    let expected = format!("Bearer {}", token);
    request
        .headers()
        .iter()
        .any(|h| h.field.equiv("Authorization") && constant_time_eq(h.value.as_str().as_bytes(), expected.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn post_event(shared: &Shared, request: &mut Request) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut body = String::new();
    if request.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).is_err() || body.len() as u64 > MAX_BODY {
        return Response::from_string("bad body\n").with_status_code(400);
    }
    let event: Event = match serde_json::from_str(&body) {
        Ok(e) => e,
        Err(e) => return Response::from_string(format!("bad event: {}\n", e)).with_status_code(400),
    };
    let mut board = shared.board.lock().unwrap();
    if board.apply(&event) {
        shared.save(&board);
        shared.changed.notify_all();
    }
    Response::from_string("").with_status_code(204)
}

fn get_state(shared: &Shared, query: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let after: Option<u64> = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("after="))
        .and_then(|v| v.parse().ok());

    let board = shared.board.lock().unwrap();
    let board = match after {
        Some(after) => shared.changed.wait_timeout_while(board, LONG_POLL, |b| b.version <= after).unwrap().0,
        None => board,
    };
    let json = serde_json::to_string(&*board).unwrap_or_default();
    drop(board);
    Response::from_string(json)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

impl Shared {
    /// Write the board atomically, so a crash mid-write can't leave a broken file.
    fn save(&self, board: &Board) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.state_file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.state_file.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(board)?)?;
            std::fs::rename(&tmp, &self.state_file)
        })();
        if let Err(e) = result {
            eprintln!("blotterd: saving {}: {}", self.state_file.display(), e);
        }
    }
}

fn load_board(path: &Path) -> Board {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
