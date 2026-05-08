#![windows_subsystem = "windows"]

use std::{
    error::Error,
    fs,
    mem::size_of,
    path::PathBuf,
    ptr::null,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use image::ImageFormat;
use oneko_desktop::protocol::{CatStyle, ClientMessage, DEFAULT_ENDPOINT, LobbyConfig, PeerSnapshot, ServerMessage};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Response, Server, StatusCode};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HBRUSH,
            HDC, HGDIOBJ, ReleaseDC, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                ShellExecuteW,
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, HMENU, IDC_ARROW, IDI_APPLICATION,
                LoadCursorW, LoadIconW, MF_STRING, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage,
                RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE,
                SetCursorPos, SetForegroundWindow, SetWindowPos, ShowWindow, TPM_BOTTOMALIGN,
                TPM_LEFTALIGN, TrackPopupMenu, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
                WINDOW_EX_STYLE, WM_APP, WM_COMMAND, WM_DESTROY, WM_QUIT, WM_RBUTTONUP,
                WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_EX_TRANSPARENT, WS_OVERLAPPED, WS_POPUP, GetSystemMetrics,
                SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
            },
        },
    },
    core::{PCWSTR, w},
};

const CAT: PCWSTR = w!("OnekoCat");
const CTRL: PCWSTR = w!("OnekoCtrl");
const TITLE: PCWSTR = w!("oneko-desktop");
const PNG: &[u8] = include_bytes!("../assets/oneko.png");
const W: usize = 32;
const WI: i32 = 32;
const FRAME_MS: Duration = Duration::from_millis(100);
const LOOP_MS: Duration = Duration::from_millis(16);
const LOCKED_CURSOR_MS: Duration = Duration::from_millis(1500);
const CAT_SPEED: f32 = 10.0;
const FOLLOW_STOP: f32 = 48.0;
const SCRATCH_DIST: f32 = 18.0;
const SCRATCH_PUSH: i32 = 10;
const SCRATCH_FRAMES: u32 = 5;
const ORBIT_BASE: f32 = 28.0;
const ORBIT_STEP: f32 = 16.0;
const CATS_PER_RING: usize = 6;
const LAP_FRAMES: u32 = 48;
const CHASE_FRAMES: u32 = 42;
const IDLE_TRIGGER: u32 = 10;
const IDLE_CHANCE: u32 = 200;
const SLEEP_TIRED: u32 = 8;
const SLEEP_RESET: u32 = 192;
const SCRATCH_RESET: u32 = 9;
const TRAY_MSG: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;
const CMD_EXIT: usize = 1001;
const CMD_NEXT: usize = 1008;
const CMD_SETTINGS: usize = 1009;
const SETTINGS_HTML: &str = include_str!("../assets/settings.html");
const NET_MS: Duration = Duration::from_millis(33);

type Sprite = (usize, usize);

const IDLE: [Sprite; 1] = [(3, 3)];
const ALERT: [Sprite; 1] = [(7, 3)];
const SCRATCH_SELF: [Sprite; 3] = [(5, 0), (6, 0), (7, 0)];
const SCRATCH_N: [Sprite; 2] = [(0, 0), (0, 1)];
const SCRATCH_S: [Sprite; 2] = [(7, 1), (6, 2)];
const SCRATCH_E: [Sprite; 2] = [(2, 2), (2, 3)];
const SCRATCH_W: [Sprite; 2] = [(4, 0), (4, 1)];
const TIRED: [Sprite; 1] = [(3, 2)];
const SLEEP: [Sprite; 2] = [(2, 0), (2, 1)];
const N: [Sprite; 2] = [(1, 2), (1, 3)];
const NE: [Sprite; 2] = [(0, 2), (0, 3)];
const E: [Sprite; 2] = [(3, 0), (3, 1)];
const SE: [Sprite; 2] = [(5, 1), (5, 2)];
const S: [Sprite; 2] = [(6, 3), (7, 2)];
const SW: [Sprite; 2] = [(5, 3), (6, 1)];
const WDIR: [Sprite; 2] = [(4, 2), (4, 3)];
const NW: [Sprite; 2] = [(1, 0), (1, 1)];

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
static SETTINGS_URL: OnceLock<String> = OnceLock::new();
static NET_TX: OnceLock<mpsc::UnboundedSender<NetCommand>> = OnceLock::new();

fn main() -> Result<(), Box<dyn Error>> {
    let cfg = load_config();
    let _ = SHARED.set(Mutex::new(Shared { cfg, selected: 0, lobby: LobbyRuntime::default() }));
    let _ = NET_TX.set(start_network_thread());
    App::new()?.run()
}

struct App {
    instance: HINSTANCE,
    ctrl: HWND,
    _tray: Tray,
    asset: Asset,
    cats: Vec<CatWindow>,
    remotes: Vec<RemotePeerWindow>,
    cursor: CursorState,
    desktop: DesktopBounds,
    rng: u64,
}

impl App {
    fn new() -> Result<Self, Box<dyn Error>> {
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
        register_classes(instance)?;
        let ctrl = make_window(instance, CTRL, WS_OVERLAPPED, WINDOW_EX_STYLE::default(), true)?;
        let tray = Tray::new(ctrl)?;
        let _ = SETTINGS_URL.set(start_settings_server()?);
        let asset = Asset::load()?;
        let count = shared().lock().unwrap().cfg.cats.len();
        let mut app = Self {
            instance,
            ctrl,
            _tray: tray,
            asset,
            cats: Vec::new(),
            remotes: Vec::new(),
            cursor: CursorState::default(),
            desktop: desktop_bounds(),
            rng: seed_now() ^ 0x1234_5678_9abc_def0,
        };
        app.reconcile(count)?;
        Ok(app)
    }

    fn run(mut self) -> Result<(), Box<dyn Error>> {
        let mut last_frame = Instant::now();
        let mut last_net = Instant::now() - NET_MS;
        let mut msg = MSG::default();
        loop {
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.into() {
                if msg.message == WM_QUIT {
                    self.destroy_all();
                    return Ok(());
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let snapshot = snapshot_shared();
            let cfg = snapshot.config.clone();
            self.reconcile(cfg.cats.len())?;
            self.reconcile_remote(&snapshot.lobby.peers)?;
            self.cursor.update();
            if last_net.elapsed() >= NET_MS {
                send_net(NetCommand::Sync(LocalPresence {
                    lobby: cfg.lobby.clone(),
                    cursor_x: self.desktop.normalize_x(self.cursor.pos.0),
                    cursor_y: self.desktop.normalize_y(self.cursor.pos.1),
                    locked: self.cursor.locked,
                    cats: cfg.cats.clone(),
                }));
                last_net = Instant::now();
            }
            if last_frame.elapsed() >= FRAME_MS {
                self.tick(&cfg, &snapshot.lobby.peers);
                last_frame = Instant::now();
            }
            self.draw(&cfg, &snapshot.lobby.peers)?;
            thread::sleep(LOOP_MS);
        }
    }

    fn reconcile(&mut self, desired: usize) -> Result<(), Box<dyn Error>> {
        while self.cats.len() < desired {
            let hwnd = make_window(
                self.instance,
                CAT,
                WS_POPUP,
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                false,
            )?;
            let seed = self.roll_u64();
            self.cats.push(CatWindow { hwnd, renderer: Renderer::new(hwnd)?, state: CatState::new(seed) });
        }
        while self.cats.len() > desired {
            if let Some(cat) = self.cats.pop() {
                drop(cat.renderer);
                let _ = unsafe { DestroyWindow(cat.hwnd) };
            }
        }
        Ok(())
    }

    fn reconcile_remote(&mut self, peers: &[PeerSnapshot]) -> Result<(), Box<dyn Error>> {
        self.remotes.retain_mut(|peer| {
            if peers.iter().any(|p| p.id == peer.id) {
                true
            } else {
                for cat in peer.cats.drain(..) {
                    drop(cat.renderer);
                    let _ = unsafe { DestroyWindow(cat.hwnd) };
                }
                false
            }
        });
        for peer in peers {
            let Some(existing_idx) = self.remotes.iter().position(|p| p.id == peer.id) else {
                let mut cats = Vec::new();
                for i in 0..peer.cats.len() {
                    cats.push(RemoteCatWindow::new(self.instance, self.roll_u64(), i)?);
                }
                self.remotes.push(RemotePeerWindow { id: peer.id.clone(), cats });
                continue;
            };
            while self.remotes[existing_idx].cats.len() < peer.cats.len() {
                let idx = self.remotes[existing_idx].cats.len();
                let seed = self.roll_u64();
                let cat = RemoteCatWindow::new(self.instance, seed, idx)?;
                self.remotes[existing_idx].cats.push(cat);
            }
            while self.remotes[existing_idx].cats.len() > peer.cats.len() {
                if let Some(cat) = self.remotes[existing_idx].cats.pop() {
                    drop(cat.renderer);
                    let _ = unsafe { DestroyWindow(cat.hwnd) };
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self, cfg: &Config, peers: &[PeerSnapshot]) {
        let locked = self.cursor.locked;
        let mouse = self.cursor.pos;
        let positions: Vec<(f32, f32)> = self.cats.iter().map(|c| c.state.pos).collect();
        if locked {
            self.plan_play();
        } else {
            for cat in &mut self.cats {
                cat.state.mode = Mode::Orbit;
            }
        }
        for (i, cat) in self.cats.iter_mut().enumerate() {
            let sprite = cat.state.advance(i, mouse, &positions, cfg.scratch_push, locked);
            cat.state.sprite = sprite;
        }
        for remote in &mut self.remotes {
            if let Some(peer) = peers.iter().find(|p| p.id == remote.id) {
                let cursor = (
                    self.desktop.denormalize_x(peer.cursor_x),
                    self.desktop.denormalize_y(peer.cursor_y),
                );
                for (i, cat) in remote.cats.iter_mut().enumerate() {
                    cat.advance(cursor, i, peer.cats.len());
                }
            }
        }
    }

    fn plan_play(&mut self) {
        if self.cats.is_empty() || self.roll(30) != 0 {
            return;
        }
        if self.cats.len() >= 2 && self.roll(2) == 0 {
            let a = self.roll(self.cats.len() as u32) as usize;
            let mut b = self.roll(self.cats.len() as u32) as usize;
            if a == b {
                b = (b + 1) % self.cats.len();
            }
            self.cats[a].state.mode = Mode::Lap { angle: self.rand_angle(), speed: 0.45, ttl: CHASE_FRAMES };
            self.cats[b].state.mode = Mode::Chase { mate: a, ttl: CHASE_FRAMES };
        } else {
            let i = self.roll(self.cats.len() as u32) as usize;
            self.cats[i].state.mode = Mode::Lap { angle: self.rand_angle(), speed: 0.38, ttl: LAP_FRAMES };
        }
    }

    fn draw(&mut self, cfg: &Config, peers: &[PeerSnapshot]) -> Result<(), Box<dyn Error>> {
        for (cat, style) in self.cats.iter_mut().zip(cfg.cats.iter()) {
            cat.renderer.draw(&self.asset, cat.state.pos, cat.state.sprite, style)?;
        }
        for remote in &mut self.remotes {
            if let Some(peer) = peers.iter().find(|p| p.id == remote.id) {
                for (cat, style) in remote.cats.iter_mut().zip(peer.cats.iter()) {
                    cat.renderer.draw(&self.asset, cat.pos, cat.sprite, style)?;
                }
            }
        }
        Ok(())
    }

    fn roll(&mut self, n: u32) -> u32 {
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((self.rng >> 32) as u32) % n
    }

    fn roll_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.rng
    }

    fn rand_angle(&mut self) -> f32 {
        self.roll(360) as f32 / 180.0 * std::f32::consts::PI
    }

    fn destroy_all(&mut self) {
        while let Some(cat) = self.cats.pop() {
            drop(cat.renderer);
            let _ = unsafe { DestroyWindow(cat.hwnd) };
        }
        while let Some(mut peer) = self.remotes.pop() {
            for cat in peer.cats.drain(..) {
                drop(cat.renderer);
                let _ = unsafe { DestroyWindow(cat.hwnd) };
            }
        }
        let _ = unsafe { DestroyWindow(self.ctrl) };
    }
}

struct CatWindow {
    hwnd: HWND,
    renderer: Renderer,
    state: CatState,
}

struct RemotePeerWindow {
    id: String,
    cats: Vec<RemoteCatWindow>,
}

struct RemoteCatWindow {
    hwnd: HWND,
    renderer: Renderer,
    pos: (f32, f32),
    sprite: Sprite,
    orbit_angle: f32,
}

impl RemoteCatWindow {
    fn new(instance: HINSTANCE, seed: u64, index: usize) -> Result<Self, Box<dyn Error>> {
        let hwnd = make_window(
            instance,
            CAT,
            WS_POPUP,
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            false,
        )?;
        Ok(Self {
            hwnd,
            renderer: Renderer::new(hwnd)?,
            pos: (0.0, 0.0),
            sprite: IDLE[0],
            orbit_angle: ((seed >> 8) % 360) as f32 / 180.0 * std::f32::consts::PI + index as f32 * 0.35,
        })
    }

    fn advance(&mut self, cursor: (f32, f32), index: usize, total: usize) {
        let target = orbit_target(cursor, self.orbit_angle, index, total.max(1));
        if self.pos == (0.0, 0.0) {
            self.pos = target;
            self.sprite = IDLE[0];
            return;
        }
        let d = dist(self.pos, target);
        if d < FOLLOW_STOP {
            self.sprite = IDLE[0];
            return;
        }
        let (dx, dy, len) = delta(self.pos, target);
        self.pos.0 -= dx / len.max(1.0) * CAT_SPEED;
        self.pos.1 -= dy / len.max(1.0) * CAT_SPEED;
        self.sprite = pick(dir(delta(self.pos, target)), index as u32);
    }
}

#[derive(Default, Clone)]
struct DesktopBounds {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl DesktopBounds {
    fn normalize_x(&self, x: f32) -> f32 { ((x - self.left) / self.width.max(1.0)).clamp(0.0, 1.0) }
    fn normalize_y(&self, y: f32) -> f32 { ((y - self.top) / self.height.max(1.0)).clamp(0.0, 1.0) }
    fn denormalize_x(&self, x: f32) -> f32 { self.left + self.width * x.clamp(0.0, 1.0) }
    fn denormalize_y(&self, y: f32) -> f32 { self.top + self.height * y.clamp(0.0, 1.0) }
}

#[derive(Default, Clone, Serialize)]
struct LobbyView {
    endpoint: String,
    room_code: String,
    display_name: String,
    connected: bool,
    status: String,
    peers: Vec<PeerSnapshot>,
}

#[derive(Clone, Serialize)]
struct UiSnapshot {
    config: Config,
    selected: usize,
    lobby: LobbyView,
}

#[derive(Default)]
struct LobbyRuntime {
    connected: bool,
    status: String,
    client_id: String,
    peers: Vec<PeerSnapshot>,
}

struct Shared {
    cfg: Config,
    selected: usize,
    lobby: LobbyRuntime,
}

#[derive(Clone)]
struct LocalPresence {
    lobby: LobbyConfig,
    cursor_x: f32,
    cursor_y: f32,
    locked: bool,
    cats: Vec<CatStyle>,
}

enum NetCommand {
    Sync(LocalPresence),
    Create(LobbyConfig),
    Join(LobbyConfig),
    Leave,
}

#[derive(Default)]
struct CursorState {
    pos: (f32, f32),
    last_pos: (f32, f32),
    last_move: Option<Instant>,
    locked: bool,
}

impl CursorState {
    fn update(&mut self) {
        let Some(pos) = cursor_pos() else { return };
        if dist(self.pos, pos) > 0.5 {
            self.last_pos = self.pos;
            self.pos = pos;
            self.last_move = Some(Instant::now());
            self.locked = false;
        } else {
            self.pos = pos;
            self.locked = self.last_move.is_some_and(|t| t.elapsed() >= LOCKED_CURSOR_MS);
        }
    }
}

#[derive(Clone, Copy)]
enum IdleAnim { Sleep, Groom, N, S, E, W }

#[derive(Clone, Copy)]
enum Mode {
    Orbit,
    Lap { angle: f32, speed: f32, ttl: u32 },
    Chase { mate: usize, ttl: u32 },
}

struct CatState {
    pos: (f32, f32),
    frame: u32,
    idle: u32,
    idle_anim: Option<IdleAnim>,
    idle_frame: u32,
    push_frames: u32,
    sprite: Sprite,
    rng: u64,
    mode: Mode,
    orbit_angle: f32,
}

impl CatState {
    fn new(seed: u64) -> Self {
        let orbit_angle = ((seed >> 8) % 360) as f32 / 180.0 * std::f32::consts::PI;
        Self {
            pos: (0.0, 0.0),
            frame: 0,
            idle: 0,
            idle_anim: None,
            idle_frame: 0,
            push_frames: 0,
            sprite: IDLE[0],
            rng: seed | 1,
            mode: Mode::Orbit,
            orbit_angle,
        }
    }

    fn advance(
        &mut self,
        index: usize,
        mouse: (f32, f32),
        positions: &[(f32, f32)],
        scratch_push: bool,
        locked: bool,
    ) -> Sprite {
        self.frame = self.frame.wrapping_add(1);
        let target = self.target(index, mouse, positions, locked);
        if self.push_frames > 0 {
            self.push_frames -= 1;
            if scratch_push {
                self.bump_cursor(mouse);
            }
            return self.sprite;
        }

        let d_mouse = dist(self.pos, mouse);
        if scratch_push && d_mouse < SCRATCH_DIST && self.roll(18) == 0 {
            self.push_frames = SCRATCH_FRAMES;
            self.sprite = scratch_toward(self.pos, mouse, self.frame);
            return self.sprite;
        }

        let d_target = dist(self.pos, target);
        if matches!(self.mode, Mode::Orbit) && d_target < FOLLOW_STOP {
            if locked {
                return self.locked_idle(index, mouse, positions);
            }
            self.set_orbit_from_pos(mouse);
            self.idle = 0;
            self.idle_anim = None;
            self.idle_frame = 0;
            self.sprite = IDLE[0];
            return self.sprite;
        }

        self.idle_anim = None;
        self.idle_frame = 0;
        if self.idle > 1 {
            self.idle = self.idle.min(7) - 1;
            self.sprite = ALERT[0];
            return self.sprite;
        }

        self.step_toward(target);
        self.sprite = pick(dir(delta(self.pos, target)), self.frame);
        self.sprite
    }

    fn target(
        &mut self,
        index: usize,
        mouse: (f32, f32),
        positions: &[(f32, f32)],
        _locked: bool,
    ) -> (f32, f32) {
        match self.mode {
            Mode::Orbit => orbit_target(mouse, self.orbit_angle, index, positions.len().max(1)),
            Mode::Lap { angle, speed, ttl } => {
                let next = angle + speed;
                if ttl > 0 {
                    self.mode = Mode::Lap { angle: next, speed, ttl: ttl - 1 };
                } else {
                    self.set_orbit_from_pos(mouse);
                    self.mode = Mode::Orbit;
                }
                let radius = ORBIT_BASE + 20.0;
                (mouse.0 + radius * next.cos(), mouse.1 + radius * next.sin())
            }
            Mode::Chase { mate, ttl } => {
                if ttl == 0 || mate >= positions.len() {
                    self.set_orbit_from_pos(mouse);
                    self.mode = Mode::Orbit;
                    orbit_target(mouse, self.orbit_angle, index, positions.len().max(1))
                } else {
                    self.mode = Mode::Chase { mate, ttl: ttl - 1 };
                    let mate_pos = positions[mate];
                    let (dx, dy, len) = delta(mate_pos, mouse);
                    let tail = 18.0;
                    (mate_pos.0 + dx / len.max(1.0) * tail, mate_pos.1 + dy / len.max(1.0) * tail)
                }
            }
        }
    }

    fn locked_idle(&mut self, _index: usize, _mouse: (f32, f32), _positions: &[(f32, f32)]) -> Sprite {
        self.idle += 1;
        if self.idle > IDLE_TRIGGER && self.idle_anim.is_none() && self.roll(IDLE_CHANCE) == 0 {
            self.idle_anim = Some(match self.roll(6) { 0 => IdleAnim::Sleep, 1 => IdleAnim::Groom, 2 => IdleAnim::N, 3 => IdleAnim::S, 4 => IdleAnim::E, _ => IdleAnim::W });
        }
        let out = match self.idle_anim {
            Some(IdleAnim::Sleep) => {
                let s = if self.idle_frame < SLEEP_TIRED { TIRED[0] } else { pick(&SLEEP, self.idle_frame / 4) };
                if self.idle_frame > SLEEP_RESET {
                    self.reset_idle();
                }
                s
            }
            Some(IdleAnim::Groom) => self.step_idle(&SCRATCH_SELF),
            Some(IdleAnim::N) => self.step_idle(&SCRATCH_N),
            Some(IdleAnim::S) => self.step_idle(&SCRATCH_S),
            Some(IdleAnim::E) => self.step_idle(&SCRATCH_E),
            Some(IdleAnim::W) => self.step_idle(&SCRATCH_W),
            None => IDLE[0],
        };
        self.idle_frame += 1;
        self.sprite = out;
        out
    }

    fn step_idle(&mut self, set: &[Sprite]) -> Sprite {
        let out = pick(set, self.idle_frame);
        if self.idle_frame > SCRATCH_RESET {
            self.reset_idle();
        }
        out
    }

    fn reset_idle(&mut self) {
        self.idle_anim = None;
        self.idle_frame = 0;
    }

    fn set_orbit_from_pos(&mut self, mouse: (f32, f32)) {
        if self.pos != (0.0, 0.0) {
            self.orbit_angle = (self.pos.1 - mouse.1).atan2(self.pos.0 - mouse.0);
        }
    }

    fn step_toward(&mut self, target: (f32, f32)) {
        if self.pos == (0.0, 0.0) {
            self.pos = target;
            return;
        }
        let (dx, dy, d) = delta(self.pos, target);
        self.pos.0 -= dx / d.max(1.0) * CAT_SPEED;
        self.pos.1 -= dy / d.max(1.0) * CAT_SPEED;
    }

    fn bump_cursor(&self, mouse: (f32, f32)) {
        let (dx, dy, len) = delta(self.pos, mouse);
        let step = (dx / len.max(1.0) * SCRATCH_PUSH as f32, dy / len.max(1.0) * SCRATCH_PUSH as f32);
        unsafe {
            let _ = SetCursorPos((mouse.0 + step.0).round() as i32, (mouse.1 + step.1).round() as i32);
        }
    }

    fn roll(&mut self, n: u32) -> u32 {
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((self.rng >> 32) as u32) % n
    }
}

struct Asset {
    sheet: Vec<u8>,
    stride: usize,
}

impl Asset {
    fn load() -> Result<Self, Box<dyn Error>> {
        let img = image::load_from_memory_with_format(PNG, ImageFormat::Png)?.into_rgba8();
        Ok(Self { stride: img.width() as usize, sheet: img.into_raw() })
    }
}

struct Renderer {
    hwnd: HWND,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    frame: [u8; W * W * 4],
}

impl Renderer {
    fn new(hwnd: HWND) -> Result<Self, Box<dyn Error>> {
        let screen = unsafe { GetDC(None) };
        let mem = unsafe { CreateCompatibleDC(Some(screen)) };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: WI,
                biHeight: -WI,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = unsafe { CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) }?;
        let old = unsafe { SelectObject(mem, HGDIOBJ(bmp.0)) };
        unsafe { let _ = ReleaseDC(None, screen); }
        Ok(Self { hwnd, mem, bmp, old, bits: bits.cast(), frame: [0; W * W * 4] })
    }

    fn draw(&mut self, asset: &Asset, pos: (f32, f32), sprite: Sprite, style: &CatStyle) -> Result<(), Box<dyn Error>> {
        blit(asset, sprite, style, &mut self.frame);
        unsafe { std::ptr::copy_nonoverlapping(self.frame.as_ptr(), self.bits, self.frame.len()); }
        let x = pos.0.round() as i32 - WI / 2;
        let y = pos.1.round() as i32 - WI / 2;
        unsafe { SetWindowPos(self.hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE)?; }
        let screen = unsafe { GetDC(None) };
        let pt = POINT { x, y };
        let src = POINT { x: 0, y: 0 };
        let size = SIZE { cx: WI, cy: WI };
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let res = unsafe { UpdateLayeredWindow(self.hwnd, Some(screen), Some(&pt), Some(&size), Some(self.mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA) };
        unsafe { let _ = ReleaseDC(None, screen); }
        res.map_err(Into::into)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.mem, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.mem);
        }
    }
}

struct Tray(HWND);

impl Tray {
    fn new(hwnd: HWND) -> Result<Self, Box<dyn Error>> {
        let mut data = NOTIFYICONDATAW::default();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = TRAY_UID;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = TRAY_MSG;
        data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION)? };
        fill_tip(&mut data.szTip, "oneko desktop");
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return Err("tray icon failed".into());
        }
        Ok(Self(hwnd))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let mut data = NOTIFYICONDATAW::default();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.0;
        data.uID = TRAY_UID;
        unsafe { let _ = Shell_NotifyIconW(NIM_DELETE, &data); }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Config {
    scratch_push: bool,
    cats: Vec<CatStyle>,
    lobby: LobbyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scratch_push: true,
            cats: vec![CatStyle::default(), CatStyle { body: 0x00f4_d7f7, outline: 0x0024_2424, opacity: 224 }],
            lobby: LobbyConfig {
                endpoint: DEFAULT_ENDPOINT.to_string(),
                room_code: String::new(),
                display_name: default_name(),
            },
        }
    }
}

fn shared() -> &'static Mutex<Shared> {
    SHARED.get().expect("shared state not initialized")
}

fn snapshot_shared() -> UiSnapshot {
    let shared = shared().lock().unwrap();
    UiSnapshot {
        config: shared.cfg.clone(),
        selected: shared.selected.min(shared.cfg.cats.len().saturating_sub(1)),
        lobby: LobbyView {
            endpoint: shared.cfg.lobby.endpoint.clone(),
            room_code: shared.cfg.lobby.room_code.clone(),
            display_name: shared.cfg.lobby.display_name.clone(),
            connected: shared.lobby.connected,
            status: shared.lobby.status.clone(),
            peers: shared
                .lobby
                .peers
                .iter()
                .filter(|peer| peer.id != shared.lobby.client_id)
                .cloned()
                .collect(),
        },
    }
}

fn config_path() -> PathBuf {
    let root = std::env::var("APPDATA").ok().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    root.join("oneko-desktop").join("config.json")
}

fn load_config() -> Config {
    let path = config_path();
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .map(normalize_config)
        .unwrap_or_default()
}

fn save_config(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, serde_json::to_string_pretty(cfg).unwrap_or_default());
}

fn normalize_config(mut cfg: Config) -> Config {
    if cfg.cats.is_empty() {
        cfg.cats.push(CatStyle::default());
    }
    if cfg.lobby.endpoint.is_empty() {
        cfg.lobby.endpoint = DEFAULT_ENDPOINT.to_string();
    }
    if cfg.lobby.display_name.is_empty() {
        cfg.lobby.display_name = default_name();
    }
    cfg
}

fn register_classes(instance: HINSTANCE) -> Result<(), Box<dyn Error>> {
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW)? };
    let icon = unsafe { LoadIconW(None, IDI_APPLICATION)? };
    let cat_cls = WNDCLASSW {
        hCursor: cursor,
        hIcon: icon,
        hInstance: instance.into(),
        lpszClassName: CAT,
        lpfnWndProc: Some(cat_wndproc),
        hbrBackground: HBRUSH::default(),
        ..Default::default()
    };
    let ctrl_cls = WNDCLASSW {
        hCursor: cursor,
        hIcon: icon,
        hInstance: instance.into(),
        lpszClassName: CTRL,
        lpfnWndProc: Some(ctrl_wndproc),
        hbrBackground: HBRUSH::default(),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&cat_cls) } == 0 || unsafe { RegisterClassW(&ctrl_cls) } == 0 {
        return Err("register class failed".into());
    }
    Ok(())
}

fn make_window(
    instance: HINSTANCE,
    class: PCWSTR,
    style: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE,
    ex: WINDOW_EX_STYLE,
    hidden: bool,
) -> Result<HWND, Box<dyn Error>> {
    let hwnd = unsafe { CreateWindowExW(ex, class, TITLE, style, 0, 0, WI, WI, None, None, Some(instance), Some(null())) }?;
    unsafe { let _ = ShowWindow(hwnd, if hidden { SW_HIDE } else { SW_SHOWNOACTIVATE }); }
    Ok(hwnd)
}

fn blit(asset: &Asset, (sx, sy): Sprite, style: &CatStyle, out: &mut [u8; W * W * 4]) {
    let body = unpack(style.body);
    let outline = unpack(style.outline);
    for y in 0..W {
        for x in 0..W {
            let si = (((sy * W + y) * asset.stride) + sx * W + x) * 4;
            let di = (y * W + x) * 4;
            let alpha = (asset.sheet[si + 3] as u16 * style.opacity as u16 / 255) as u8;
            let mix_value = asset.sheet[si];
            let rgb = mix(outline, body, mix_value);
            out[di] = (rgb[2] as u16 * alpha as u16 / 255) as u8;
            out[di + 1] = (rgb[1] as u16 * alpha as u16 / 255) as u8;
            out[di + 2] = (rgb[0] as u16 * alpha as u16 / 255) as u8;
            out[di + 3] = alpha;
        }
    }
}

fn orbit_target(mouse: (f32, f32), angle: f32, index: usize, total: usize) -> (f32, f32) {
    let radius = ORBIT_BASE + (index / CATS_PER_RING) as f32 * ORBIT_STEP + (total.saturating_sub(1) as f32 * 2.0);
    (mouse.0 + radius * angle.cos(), mouse.1 + radius * angle.sin())
}

fn desktop_bounds() -> DesktopBounds {
    DesktopBounds {
        left: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) as f32 },
        top: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) as f32 },
        width: unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) as f32 },
        height: unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) as f32 },
    }
}

fn scratch_toward(from: (f32, f32), to: (f32, f32), frame: u32) -> Sprite {
    let dx = from.0 - to.0;
    let dy = from.1 - to.1;
    if dy.abs() > dx.abs() {
        pick(if dy > 0.0 { &SCRATCH_N } else { &SCRATCH_S }, frame)
    } else {
        pick(if dx > 0.0 { &SCRATCH_W } else { &SCRATCH_E }, frame)
    }
}

fn cursor_pos() -> Option<(f32, f32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.is_ok().then_some((p.x as f32, p.y as f32))
}

fn delta(from: (f32, f32), to: (f32, f32)) -> (f32, f32, f32) {
    let (dx, dy) = (from.0 - to.0, from.1 - to.1);
    (dx, dy, (dx * dx + dy * dy).sqrt())
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
    (dx * dx + dy * dy).sqrt()
}

fn pick(set: &[Sprite], frame: u32) -> Sprite {
    set[frame as usize % set.len()]
}

fn dir((dx, dy, d): (f32, f32, f32)) -> &'static [Sprite] {
    match (dy / d > 0.5, dy / d < -0.5, dx / d > 0.5, dx / d < -0.5) {
        (true, false, true, false) => &NW,
        (true, false, false, true) => &NE,
        (false, true, true, false) => &SW,
        (false, true, false, true) => &SE,
        (true, false, false, false) => &N,
        (false, true, false, false) => &S,
        (false, false, true, false) => &WDIR,
        _ => &E,
    }
}

fn fill_tip(buf: &mut [u16], s: &str) {
    let utf: Vec<u16> = s.encode_utf16().collect();
    let n = utf.len().min(buf.len().saturating_sub(1));
    buf[..n].copy_from_slice(&utf[..n]);
    buf[n] = 0;
}

fn menu_text(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn cmd(id: WPARAM) -> usize {
    (id.0 & 0xffff) as usize
}

fn unpack(rgb: u32) -> [u8; 3] {
    [((rgb >> 16) & 255) as u8, ((rgb >> 8) & 255) as u8, (rgb & 255) as u8]
}

fn mix(a: [u8; 3], b: [u8; 3], t: u8) -> [u8; 3] {
    let t = t as u16;
    [
        ((a[0] as u16 * (255 - t) + b[0] as u16 * t) / 255) as u8,
        ((a[1] as u16 * (255 - t) + b[1] as u16 * t) / 255) as u8,
        ((a[2] as u16 * (255 - t) + b[2] as u16 * t) / 255) as u8,
    ]
}

fn default_name() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "cat owner".to_string())
}

fn mutate_config<F: FnOnce(&mut Shared)>(f: F) {
    let mut shared = shared().lock().unwrap();
    f(&mut shared);
    shared.selected = shared.selected.min(shared.cfg.cats.len().saturating_sub(1));
    shared.cfg = normalize_config(shared.cfg.clone());
    save_config(&shared.cfg);
}

fn mutate_lobby<F: FnOnce(&mut LobbyRuntime)>(f: F) {
    let mut shared = shared().lock().unwrap();
    f(&mut shared.lobby);
}

fn start_network_thread() -> mpsc::UnboundedSender<NetCommand> {
    let (tx, mut rx) = mpsc::unbounded_channel::<NetCommand>();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
        runtime.block_on(async move {
            let mut socket = None;
            let mut client_id = String::new();
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    NetCommand::Create(lobby) => {
                        socket = connect_lobby(&lobby, true).await;
                        client_id.clear();
                        mutate_lobby(|l| {
                            l.status = if socket.is_some() { "Connecting".into() } else { "Connection failed".into() };
                            l.connected = false;
                            l.peers.clear();
                        });
                        if let Some(ws) = socket.as_mut() {
                            let _ = ws.send(json_msg(&ClientMessage::CreateRoom { display_name: lobby.display_name.clone(), cats: snapshot_shared().config.cats.clone() })).await;
                        }
                    }
                    NetCommand::Join(lobby) => {
                        socket = connect_lobby(&lobby, false).await;
                        client_id.clear();
                        mutate_lobby(|l| {
                            l.status = if socket.is_some() { "Connecting".into() } else { "Connection failed".into() };
                            l.connected = false;
                            l.peers.clear();
                        });
                        if let Some(ws) = socket.as_mut() {
                            let _ = ws.send(json_msg(&ClientMessage::JoinRoom {
                                room_code: lobby.room_code.clone(),
                                display_name: lobby.display_name.clone(),
                                cats: snapshot_shared().config.cats.clone(),
                            })).await;
                        }
                    }
                    NetCommand::Leave => {
                        if let Some(mut ws) = socket.take() {
                            let _ = ws.send(json_msg(&ClientMessage::Leave)).await;
                            let _ = ws.close(None).await;
                        }
                    }
                    NetCommand::Sync(local) => {
                        if let Some(ws) = socket.as_mut() {
                            while let Ok(Some(msg)) = tokio::time::timeout(Duration::from_millis(1), ws.next()).await {
                                handle_server_message(&mut client_id, msg);
                            }
                            let _ = ws.send(json_msg(&ClientMessage::Presence {
                                cursor_x: local.cursor_x,
                                cursor_y: local.cursor_y,
                                locked: local.locked,
                                cats: local.cats,
                            })).await;
                        } else if !local.lobby.room_code.is_empty() && !local.lobby.endpoint.is_empty() {
                            mutate_lobby(|l| {
                                if l.status.is_empty() {
                                    l.status = "Disconnected".into();
                                }
                            });
                        }
                    }
                }
            }
        });
    });
    tx
}

fn handle_server_message(client_id: &mut String, msg: Result<Message, tokio_tungstenite::tungstenite::Error>) {
    let Ok(Message::Text(text)) = msg else { return; };
    let Ok(message) = serde_json::from_str::<ServerMessage>(&text) else { return; };
    match message {
        ServerMessage::Joined { client_id: id, room_code, peers } => {
            *client_id = id.clone();
            mutate_config(|s| s.cfg.lobby.room_code = room_code.clone());
            mutate_lobby(|l| {
                l.client_id = id;
                l.connected = true;
                l.status = format!("Connected to {}", room_code);
                l.peers = peers;
            });
        }
        ServerMessage::Snapshot { room_code, peers } => {
            mutate_lobby(|l| {
                l.connected = true;
                l.status = format!("Connected to {}", room_code);
                l.peers = peers;
            });
        }
        ServerMessage::Error { message } => {
            mutate_lobby(|l| {
                l.connected = false;
                l.status = message;
                l.peers.clear();
            });
        }
    }
}

async fn connect_lobby(
    lobby: &LobbyConfig,
    _create: bool,
) -> Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    let endpoint = if lobby.endpoint.trim().is_empty() { DEFAULT_ENDPOINT } else { lobby.endpoint.trim() };
    let url = Url::parse(endpoint).ok()?;
    connect_async(url.as_str()).await.ok().map(|(ws, _)| ws)
}

fn json_msg(msg: &ClientMessage) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap_or_default().into())
}

fn start_settings_server() -> Result<String, Box<dyn Error>> {
    let server = Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let url = format!("http://{}", server.server_addr());
    thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let response = match (request.method(), request.url()) {
                (&Method::Get, "/") => html_response(SETTINGS_HTML, "text/html; charset=utf-8"),
                (&Method::Get, "/oneko.png") => bytes_response(PNG.to_vec(), "image/png"),
                (&Method::Get, "/api/state") => {
                    let state = snapshot_shared();
                    json_response(serde_json::to_vec(&state).unwrap_or_else(|_| b"{}".to_vec()))
                }
                (&Method::Post, "/api/config") => {
                    let mut body = Vec::new();
                    let _ = request.as_reader().read_to_end(&mut body);
                    match serde_json::from_slice::<Config>(&body).map(normalize_config) {
                        Ok(cfg) => {
                            mutate_config(|s| s.cfg = cfg);
                            json_response(br#"{"ok":true}"#.to_vec())
                        }
                        Err(_) => response_status(StatusCode(400), b"bad config".to_vec(), "text/plain"),
                    }
                }
                (&Method::Post, "/api/lobby/create") => {
                    let lobby = snapshot_shared().config.lobby;
                    send_net(NetCommand::Create(lobby));
                    json_response(br#"{"ok":true}"#.to_vec())
                }
                (&Method::Post, "/api/lobby/join") => {
                    let lobby = snapshot_shared().config.lobby;
                    send_net(NetCommand::Join(lobby));
                    json_response(br#"{"ok":true}"#.to_vec())
                }
                (&Method::Post, "/api/lobby/leave") => {
                    send_net(NetCommand::Leave);
                    mutate_lobby(|l| {
                        l.connected = false;
                        l.client_id.clear();
                        l.peers.clear();
                        l.status = "Disconnected".into();
                    });
                    json_response(br#"{"ok":true}"#.to_vec())
                }
                _ => response_status(StatusCode(404), b"not found".to_vec(), "text/plain"),
            };
            let _ = request.respond(response);
        }
    });
    Ok(url)
}

fn header(name: &str, value: &str) -> Option<Header> {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
}

fn response_status(status: StatusCode, body: Vec<u8>, content_type: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_data(body).with_status_code(status);
    if let Some(h) = header("Content-Type", content_type) {
        response = response.with_header(h);
    }
    response
}

fn html_response(body: &str, content_type: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    response_status(StatusCode(200), body.as_bytes().to_vec(), content_type)
}

fn bytes_response(body: Vec<u8>, content_type: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    response_status(StatusCode(200), body, content_type)
}

fn json_response(body: Vec<u8>) -> Response<std::io::Cursor<Vec<u8>>> {
    response_status(StatusCode(200), body, "application/json")
}

fn open_settings(hwnd: HWND) {
    if let Some(url) = SETTINGS_URL.get() {
        let wide: Vec<u16> = url.encode_utf16().chain([0]).collect();
        unsafe {
            let _ = ShellExecuteW(Some(hwnd), w!("open"), PCWSTR(wide.as_ptr()), None, None, windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD(1));
        }
    }
}

fn send_net(cmd: NetCommand) {
    if let Some(tx) = NET_TX.get() {
        let _ = tx.send(cmd);
    }
}

unsafe fn tray_menu(hwnd: HWND) {
    let state = snapshot_shared();
    let menu: HMENU = unsafe { CreatePopupMenu() }.expect("menu");
    let settings = menu_text("Open Settings");
    let cat_count = menu_text(&format!("Cats: {}", state.config.cats.len()));
    let selected_label = menu_text(&format!("Selected: {}", state.selected + 1));
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, PCWSTR(settings.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, CMD_NEXT, PCWSTR(selected_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, 0, PCWSTR(cat_count.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_BOTTOMALIGN, p.x, p.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

unsafe extern "system" fn ctrl_wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match cmd(w) {
                CMD_SETTINGS => open_settings(hwnd),
                CMD_NEXT => mutate_config(|s| s.selected = (s.selected + 1) % s.cfg.cats.len()),
                CMD_EXIT => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return LRESULT(0);
                }
                _ => {}
            }
            LRESULT(0)
        }
        TRAY_MSG if l.0 as u32 == WM_RBUTTONUP => {
            unsafe { tray_menu(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

unsafe extern "system" fn cat_wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

fn seed_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
}
