use serde::{Deserialize, Serialize};

pub const DEFAULT_ENDPOINT: &str = "wss://oneko.nosh.zip:8118/";

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CatStyle {
    pub body: u32,
    pub outline: u32,
    pub opacity: u8,
}

impl Default for CatStyle {
    fn default() -> Self {
        Self { body: 0x00ff_ffff, outline: 0x0000_0000, opacity: 255 }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct LobbyConfig {
    pub endpoint: String,
    pub room_code: String,
    pub display_name: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct PeerSnapshot {
    pub id: String,
    pub display_name: String,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub locked: bool,
    pub cats: Vec<CatStyle>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    CreateRoom {
        display_name: String,
        cats: Vec<CatStyle>,
    },
    JoinRoom {
        room_code: String,
        display_name: String,
        cats: Vec<CatStyle>,
    },
    Presence {
        cursor_x: f32,
        cursor_y: f32,
        locked: bool,
        cats: Vec<CatStyle>,
    },
    Leave,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Joined {
        client_id: String,
        room_code: String,
        peers: Vec<PeerSnapshot>,
    },
    Snapshot {
        room_code: String,
        peers: Vec<PeerSnapshot>,
    },
    Error {
        message: String,
    },
}
