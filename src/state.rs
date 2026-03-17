use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::RwLock;

pub const MAX_COLUMNS: usize = 5;
pub const AGENT_IDLE_TIMEOUT_SECS: f64 = 5.0 * 60.0;
pub const STATS_DISPLAY_INTERVAL_SECS: f64 = 15.0;
pub const MAX_UNIQUE_AGENTS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    Idle,
    Working,
    Requesting,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::Requesting => "requesting",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpriteOverride {
    pub theme: usize,
    pub index: usize,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub agent_id: String,
    pub session_id: String,
    pub state: AgentState,
    pub tool_name: String,
    pub last_event: String,
    pub last_seen: Instant,
    pub last_tool_time: Instant,
    pub theme_index: usize,
    pub sprite_index: usize,
    pub sprite_override: Option<SpriteOverride>,
    pub pixel_override: Option<Vec<u8>>,
}

impl Agent {
    pub fn new(
        agent_id: String,
        session_id: String,
        state: AgentState,
        tool_name: String,
        theme_index: usize,
        sprite_index: usize,
    ) -> Self {
        let now = Instant::now();
        Self {
            agent_id,
            session_id,
            state,
            tool_name,
            last_event: String::new(),
            last_seen: now,
            last_tool_time: now,
            theme_index,
            sprite_index,
            sprite_override: None,
            pixel_override: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Host {
    pub theme_index: usize,
    pub columns: Vec<usize>,
    pub agent_count: usize,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub host_id: String,
    pub slots: [Option<Agent>; 2],
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub agent_id: String,
    pub host_id: String,
    pub col: usize,
    pub row: usize,
}

#[derive(Debug, Clone)]
pub struct Stats {
    pub tool_calls: u64,
    pub unique_agents: HashSet<String>,
    pub unique_agents_count: u64,
    pub agent_minutes: f64,
    pub last_accum_time: Instant,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            tool_calls: 0,
            unique_agents: HashSet::new(),
            unique_agents_count: 0,
            agent_minutes: 0.0,
            last_accum_time: Instant::now(),
        }
    }
}

impl Stats {
    pub fn total_unique_agents(&self) -> u64 {
        self.unique_agents_count + self.unique_agents.len() as u64
    }
}

#[derive(Debug, Clone)]
pub struct StatsDisplay {
    pub time_str: String,
    pub tool_str: String,
    pub agnt_str: String,
    pub last_update: Instant,
    pub needs_immediate: bool,
}

impl Default for StatsDisplay {
    fn default() -> Self {
        Self {
            time_str: "0".into(),
            tool_str: "0".into(),
            agnt_str: "0".into(),
            last_update: Instant::now(),
            needs_immediate: true,
        }
    }
}

pub struct DisplayState {
    pub columns: [Option<Column>; MAX_COLUMNS],
    pub hosts: HashMap<String, Host>,
    pub session_map: HashMap<String, SessionInfo>,
    pub host_by_client: HashMap<String, String>,
    pub next_host_num: u32,
    pub stats: Stats,
    pub stats_display: StatsDisplay,
    pub db_conn: Option<Mutex<rusqlite::Connection>>,
    pub force_ble_reconnect: bool,
}

impl std::fmt::Debug for DisplayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayState")
            .field("columns", &self.columns)
            .field("hosts", &self.hosts)
            .field("session_map", &self.session_map)
            .field("next_host_num", &self.next_host_num)
            .field("stats", &self.stats)
            .field("stats_display", &self.stats_display)
            .field("db_conn", &self.db_conn.as_ref().map(|_| "<Connection>"))
            .finish()
    }
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            columns: std::array::from_fn(|_| None),
            hosts: HashMap::new(),
            session_map: HashMap::new(),
            host_by_client: HashMap::new(),
            next_host_num: 1,
            stats: Stats::default(),
            stats_display: StatsDisplay::default(),
            db_conn: None,
            force_ble_reconnect: false,
        }
    }
}

impl DisplayState {
    pub fn any_requesting(&self) -> bool {
        self.columns.iter().any(|col| {
            col.as_ref().is_some_and(|c| {
                c.slots
                    .iter()
                    .any(|s| s.as_ref().is_some_and(|a| a.state == AgentState::Requesting))
            })
        })
    }
}

pub type SharedState = Arc<RwLock<DisplayState>>;

pub fn new_shared_state() -> SharedState {
    Arc::new(RwLock::new(DisplayState::default()))
}
