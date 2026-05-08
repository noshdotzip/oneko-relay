use std::{
    collections::HashMap,
    error::Error,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use oneko_desktop::protocol::{ClientMessage, PeerSnapshot, ServerMessage};
use tokio::{net::{TcpListener, TcpStream}, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const MAX_TEXT_BYTES: usize = 4096;
const MAX_NAME_LEN: usize = 24;
const MAX_CATS: usize = 8;
const MAX_ROOM_PEERS: usize = 16;
const PRESENCE_INTERVAL: Duration = Duration::from_millis(16);
const INVALID_LIMIT: u8 = 4;

#[derive(Default)]
struct RoomState {
    peers: HashMap<String, PeerSnapshot>,
    clients: HashMap<String, mpsc::UnboundedSender<ServerMessage>>,
}

#[derive(Default)]
struct RelayState {
    rooms: HashMap<String, RoomState>,
    seq: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn Error>> {
    let bind = std::env::var("ONEKO_RELAY_BIND").unwrap_or_else(|_| "0.0.0.0:8118".to_string());
    let listener = TcpListener::bind(&bind).await?;
    let state = Arc::new(Mutex::new(RelayState::default()));
    println!("oneko relay listening on {}", bind);
    loop {
        let (stream, addr) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(stream, addr, state).await {
                eprintln!("client error: {err}");
            }
        });
    }
}

async fn handle(stream: TcpStream, _addr: SocketAddr, state: Arc<Mutex<RelayState>>) -> Result<(), Box<dyn Error>> {
    let ws = accept_async(stream).await?;
    let (mut write, mut read) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let text = serde_json::to_string(&msg).unwrap_or_default();
            if write.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let mut client_id = String::new();
    let mut room_code = String::new();
    let mut last_presence = Instant::now() - PRESENCE_INTERVAL;
    let mut invalid_count = 0u8;
    while let Some(message) = read.next().await {
        let message = match message {
            Ok(Message::Text(text)) if text.len() <= MAX_TEXT_BYTES => text,
            Ok(Message::Text(_)) => {
                invalid_count += 1;
                let _ = tx.send(ServerMessage::Error { message: "Message too large".into() });
                if invalid_count >= INVALID_LIMIT {
                    break;
                }
                continue;
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(_) => break,
        };
        let parsed: ClientMessage = match serde_json::from_str(&message) {
            Ok(msg) => msg,
            Err(_) => {
                invalid_count += 1;
                let _ = tx.send(ServerMessage::Error { message: "Invalid request".into() });
                if invalid_count >= INVALID_LIMIT {
                    break;
                }
                continue;
            }
        };
        invalid_count = 0;
        match parsed {
            ClientMessage::CreateRoom { display_name, cats } => {
                let display_name = match sanitize_name(&display_name) {
                    Some(name) => name,
                    None => {
                        let _ = tx.send(ServerMessage::Error { message: "Invalid display name".into() });
                        continue;
                    }
                };
                let cats = match sanitize_cats(cats) {
                    Some(cats) => cats,
                    None => {
                        let _ = tx.send(ServerMessage::Error { message: "Invalid cat payload".into() });
                        continue;
                    }
                };
                if !client_id.is_empty() {
                    let old_room = room_code.clone();
                    cleanup(&state, &room_code, &client_id);
                    broadcast(&state, &old_room, None);
                }
                let (id, room) = {
                    let mut guard = state.lock().unwrap();
                    let id = next_id(&mut guard.seq);
                    let room = loop {
                        let room = next_room(&mut guard.seq);
                        if !guard.rooms.contains_key(&room) {
                            break room;
                        }
                    };
                    let peer = PeerSnapshot { id: id.clone(), display_name, cats, ..Default::default() };
                    let entry = guard.rooms.entry(room.clone()).or_default();
                    entry.peers.insert(id.clone(), peer);
                    entry.clients.insert(id.clone(), tx.clone());
                    (id, room)
                };
                client_id = id;
                room_code = room.clone();
                broadcast(&state, &room, Some((&client_id, ServerMessage::Joined {
                    client_id: client_id.clone(),
                    room_code: room.clone(),
                    peers: snapshot(&state, &room),
                })));
            }
            ClientMessage::JoinRoom { room_code: join, display_name, cats } => {
                let join = match sanitize_room_code(&join) {
                    Some(code) => code,
                    None => {
                        let _ = tx.send(ServerMessage::Error { message: "Invalid room code".into() });
                        continue;
                    }
                };
                let display_name = match sanitize_name(&display_name) {
                    Some(name) => name,
                    None => {
                        let _ = tx.send(ServerMessage::Error { message: "Invalid display name".into() });
                        continue;
                    }
                };
                let cats = match sanitize_cats(cats) {
                    Some(cats) => cats,
                    None => {
                        let _ = tx.send(ServerMessage::Error { message: "Invalid cat payload".into() });
                        continue;
                    }
                };
                if !client_id.is_empty() {
                    let old_room = room_code.clone();
                    cleanup(&state, &room_code, &client_id);
                    broadcast(&state, &old_room, None);
                }
                let joined = {
                    let mut guard = state.lock().unwrap();
                    if !guard.rooms.contains_key(&join) {
                        let _ = tx.send(ServerMessage::Error { message: "Room not found".into() });
                        false
                    } else if guard.rooms.get(&join).is_some_and(|room| room.peers.len() >= MAX_ROOM_PEERS) {
                        let _ = tx.send(ServerMessage::Error { message: "Room is full".into() });
                        false
                    } else {
                        let id = next_id(&mut guard.seq);
                        let peer = PeerSnapshot { id: id.clone(), display_name, cats, ..Default::default() };
                        let room = guard.rooms.get_mut(&join).unwrap();
                        room.peers.insert(id.clone(), peer);
                        room.clients.insert(id.clone(), tx.clone());
                        client_id = id;
                        room_code = join.clone();
                        true
                    }
                };
                if joined {
                    broadcast(&state, &room_code, Some((&client_id, ServerMessage::Joined {
                        client_id: client_id.clone(),
                        room_code: room_code.clone(),
                        peers: snapshot(&state, &room_code),
                    })));
                }
            }
            ClientMessage::Presence { cursor_x, cursor_y, locked, cats, renders } => {
                if client_id.is_empty() || room_code.is_empty() {
                    let _ = tx.send(ServerMessage::Error { message: "Join a room first".into() });
                    continue;
                }
                if last_presence.elapsed() < PRESENCE_INTERVAL {
                    continue;
                }
                let Some(cats) = sanitize_cats(cats) else {
                    let _ = tx.send(ServerMessage::Error { message: "Invalid cat payload".into() });
                    continue;
                };
                let Some(renders) = sanitize_renders(renders, cats.len()) else {
                    let _ = tx.send(ServerMessage::Error { message: "Invalid cat render payload".into() });
                    continue;
                };
                let Some((cursor_x, cursor_y)) = sanitize_cursor(cursor_x, cursor_y) else {
                    let _ = tx.send(ServerMessage::Error { message: "Invalid cursor position".into() });
                    continue;
                };
                last_presence = Instant::now();
                let mut guard = state.lock().unwrap();
                if let Some(room) = guard.rooms.get_mut(&room_code) {
                    if let Some(peer) = room.peers.get_mut(&client_id) {
                        peer.cursor_x = cursor_x;
                        peer.cursor_y = cursor_y;
                        peer.locked = locked;
                        peer.cats = cats;
                        peer.renders = renders;
                    }
                }
                drop(guard);
                broadcast(&state, &room_code, None);
            }
            ClientMessage::Leave => break,
        }
    }

    cleanup(&state, &room_code, &client_id);
    broadcast(&state, &room_code, None);
    writer.abort();
    Ok(())
}

fn snapshot(state: &Arc<Mutex<RelayState>>, room_code: &str) -> Vec<PeerSnapshot> {
    state.lock().unwrap().rooms.get(room_code).map(|r| r.peers.values().cloned().collect()).unwrap_or_default()
}

fn broadcast(state: &Arc<Mutex<RelayState>>, room_code: &str, joined: Option<(&str, ServerMessage)>) {
    let (peers, clients) = {
        let guard = state.lock().unwrap();
        let Some(room) = guard.rooms.get(room_code) else { return; };
        let peers: Vec<PeerSnapshot> = room.peers.values().cloned().collect();
        let clients: Vec<(String, mpsc::UnboundedSender<ServerMessage>)> = room.clients.iter().map(|(id, tx)| (id.clone(), tx.clone())).collect();
        (peers, clients)
    };
    for (id, tx) in clients {
        if let Some((join_id, joined_msg)) = &joined {
            if id == *join_id {
                let _ = tx.send(joined_msg.clone());
                continue;
            }
        }
        let _ = tx.send(ServerMessage::Snapshot { room_code: room_code.to_string(), peers: peers.clone() });
    }
}

fn cleanup(state: &Arc<Mutex<RelayState>>, room_code: &str, client_id: &str) {
    let mut guard = state.lock().unwrap();
    let Some(room) = guard.rooms.get_mut(room_code) else { return; };
    room.peers.remove(client_id);
    room.clients.remove(client_id);
    if room.peers.is_empty() {
        guard.rooms.remove(room_code);
    }
}

fn next_id(seq: &mut u64) -> String {
    *seq += 1;
    format!("{:x}{:x}", now_seed(), *seq)
}

fn next_room(seq: &mut u64) -> String {
    *seq += 1;
    format!("{:06}", (now_seed() as u64 + *seq) % 1_000_000)
}

fn now_seed() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1)
}

fn sanitize_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_LEN {
        return None;
    }
    Some(trimmed.chars().filter(|c| !c.is_control()).collect())
}

fn sanitize_room_code(code: &str) -> Option<String> {
    let trimmed = code.trim();
    (trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit())).then(|| trimmed.to_string())
}

fn sanitize_cats(cats: Vec<oneko_desktop::protocol::CatStyle>) -> Option<Vec<oneko_desktop::protocol::CatStyle>> {
    if cats.is_empty() || cats.len() > MAX_CATS {
        return None;
    }
    Some(cats)
}

fn sanitize_cursor(x: f32, y: f32) -> Option<(f32, f32)> {
    let valid = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    (valid(x) && valid(y)).then_some((x, y))
}

fn sanitize_renders(
    renders: Vec<oneko_desktop::protocol::CatRenderSnapshot>,
    cat_count: usize,
) -> Option<Vec<oneko_desktop::protocol::CatRenderSnapshot>> {
    if renders.len() != cat_count {
        return None;
    }
    let valid_coord = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    let valid = renders
        .iter()
        .all(|r| valid_coord(r.x) && valid_coord(r.y) && r.sprite_x < 8 && r.sprite_y < 4);
    valid.then_some(renders)
}
