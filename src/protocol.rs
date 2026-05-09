use serde::{Deserialize, Serialize};

pub const DEFAULT_ENDPOINT: &str = "wss://oneko.nosh.zip/";

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
pub struct CatRenderSnapshot {
    pub x: f32,
    pub y: f32,
    pub sprite_x: u8,
    pub sprite_y: u8,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct PeerSnapshot {
    pub id: String,
    pub display_name: String,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub locked: bool,
    pub cats: Vec<CatStyle>,
    pub renders: Vec<CatRenderSnapshot>,
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
        renders: Vec<CatRenderSnapshot>,
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
