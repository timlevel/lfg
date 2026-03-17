use std::time::Instant;
use tracing::info;

use crate::gateway;
use crate::state::{AgentState, DisplayState};

pub fn handle_event(
    state: &mut DisplayState,
    event_name: &str,
    session_id: &str,
    tool_name: &str,
    client_key: &str,
    profile: Option<&str>,
    pixels: Option<Vec<u8>>,
) {
    if session_id.is_empty() {
        return;
    }

    if state.stats.unique_agents.len() < crate::state::MAX_UNIQUE_AGENTS {
        state.stats.unique_agents.insert(session_id.to_string());
    }

    if !state.session_map.contains_key(session_id) {
        let result = gateway::join_agent(state, session_id, client_key, profile, pixels);
        if result.is_none() {
            info!("Display full — ignoring session {:.8}", session_id);
            return;
        }
    }

    let info = match state.session_map.get(session_id) {
        Some(i) => i.clone(),
        None => return,
    };

    let col = match state.columns[info.col].as_mut() {
        Some(c) => c,
        None => return,
    };

    let agent = match col.slots[info.row].as_mut() {
        Some(a) => a,
        None => return,
    };

    agent.last_event = event_name.to_string();
    agent.last_seen = Instant::now();

    if event_name == "PreToolUse" {
        state.stats.tool_calls += 1;
        agent.last_tool_time = Instant::now();
    }

    match event_name {
        "SessionStart" => {
            agent.state = AgentState::Idle;
            agent.tool_name.clear();
        }
        "PreToolUse" => {
            // Don't override Requesting — PreToolUse fires alongside
            // PermissionRequest but the tool is still blocked on approval.
            if agent.state != AgentState::Requesting {
                agent.state = AgentState::Working;
            }
            agent.tool_name = tool_name.to_string();
        }
        "PostToolUse" => {
            // If coming from a permission request, clear the fire icon.
            // Otherwise keep working — tools fire rapidly in sequence.
            if agent.state == AgentState::Requesting {
                agent.state = AgentState::Working;
                agent.tool_name = tool_name.to_string();
            }
        }
        "PermissionRequest" => {
            agent.state = AgentState::Requesting;
        }
        "Stop" | "SessionEnd" => {
            agent.state = AgentState::Idle;
            agent.tool_name.clear();
        }
        _ => {}
    }

    info!(
        "Agent {} [{:.8}] {:<20} tool={:<16?} state={}",
        agent.agent_id,
        session_id,
        event_name,
        if tool_name.is_empty() { "" } else { tool_name },
        agent.state.as_str(),
    );
}

pub fn clear_agent_slot(state: &mut DisplayState, session_id: &str) {
    let info = match state.session_map.remove(session_id) {
        Some(i) => i,
        None => return,
    };

    let col_idx = info.col;
    let row = info.row;
    let host_id = &info.host_id;

    if let Some(col) = state.columns[col_idx].as_mut() {
        col.slots[row] = None;
    }

    // Check if column is now empty
    if let Some(col) = &state.columns[col_idx] {
        if col.slots[0].is_none() && col.slots[1].is_none() {
            if let Some(host) = state.hosts.get_mut(host_id.as_str()) {
                host.columns.retain(|&c| c != col_idx);
            }
            state.columns[col_idx] = None;

            let host_empty = state
                .hosts
                .get(host_id.as_str())
                .map(|h| h.columns.is_empty())
                .unwrap_or(false);

            if host_empty {
                state.hosts.remove(host_id.as_str());
                state
                    .host_by_client
                    .retain(|_, hid| hid != host_id.as_str());
            }

            compact_columns(state);
        }
    }

    info!("Cleared agent slot for session {:.8}", session_id);
}

fn compact_columns(state: &mut DisplayState) {
    let filled: Vec<(usize, _)> = state
        .columns
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.as_ref().map(|col| (i, col.clone())))
        .collect();

    for col in state.columns.iter_mut() {
        *col = None;
    }

    for (new_idx, (old_idx, col)) in filled.into_iter().enumerate() {
        state.columns[new_idx] = Some(col);
        if old_idx != new_idx {
            let host_id = state.columns[new_idx]
                .as_ref()
                .unwrap()
                .host_id
                .clone();
            if let Some(host) = state.hosts.get_mut(&host_id) {
                for c in host.columns.iter_mut() {
                    if *c == old_idx {
                        *c = new_idx;
                    }
                }
            }
            for info in state.session_map.values_mut() {
                if info.col == old_idx {
                    info.col = new_idx;
                }
            }
        }
    }
}

pub fn check_stale(state: &mut DisplayState) {
    let now = Instant::now();

    // Accumulate agent-minutes
    let elapsed_min = now.duration_since(state.stats.last_accum_time).as_secs_f64() / 60.0;
    let mut active_count = 0u64;
    for col in &state.columns {
        if let Some(c) = col {
            for slot in &c.slots {
                if slot.is_some() {
                    active_count += 1;
                }
            }
        }
    }
    state.stats.agent_minutes += active_count as f64 * elapsed_min;
    state.stats.last_accum_time = now;

    // Check idle timeouts (age out agents with no tool call for 5m)
    let mut aged_out = Vec::new();
    for col in state.columns.iter_mut() {
        if let Some(c) = col {
            for slot in c.slots.iter_mut() {
                if let Some(agent) = slot {
                    let secs_since_tool =
                        now.duration_since(agent.last_tool_time).as_secs_f64();
                    if secs_since_tool > crate::state::AGENT_IDLE_TIMEOUT_SECS {
                        aged_out.push(agent.session_id.clone());
                        info!("Agent {} aged out (no tool call for 5m)", agent.agent_id);
                    }
                }
            }
        }
    }

    for sid in aged_out {
        clear_agent_slot(state, &sid);
    }

    // Refresh stats display
    let display_elapsed = now
        .duration_since(state.stats_display.last_update)
        .as_secs_f64();
    if state.stats_display.needs_immediate
        || display_elapsed >= crate::state::STATS_DISPLAY_INTERVAL_SECS
    {
        state.stats_display.time_str = format!("{}", state.stats.agent_minutes as u64);
        state.stats_display.tool_str = format!("{}", state.stats.tool_calls);
        state.stats_display.agnt_str = format!("{}", state.stats.total_unique_agents());
        state.stats_display.last_update = now;
        state.stats_display.needs_immediate = false;

        // Persist stats to DB
        if let Some(ref db) = state.db_conn {
            if let Ok(conn) = db.lock() {
                crate::db::save_stats(
                    &conn,
                    state.stats.tool_calls,
                    state.stats.total_unique_agents(),
                    state.stats.agent_minutes,
                );
            }
        }
    }
}

pub fn reset_all(state: &mut DisplayState) {
    for col in state.columns.iter_mut() {
        *col = None;
    }
    state.hosts.clear();
    state.session_map.clear();
    state.host_by_client.clear();
    state.next_host_num = 1;
    state.stats = crate::state::Stats::default();
    state.stats_display = crate::state::StatsDisplay::default();

    // Clear persisted stats
    if let Some(ref db) = state.db_conn {
        if let Ok(conn) = db.lock() {
            crate::db::save_stats(&conn, 0, 0, 0.0);
        }
    }
}
