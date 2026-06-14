use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::agent_memory;
use crate::claims;
use crate::config::{load_agents, resolve_lead_agent, sync_lead_coordination_rules, AgentSpec};
use crate::health::{self, HealthState};
use crate::confirm_panel::ConfirmPanel;
use crate::help_panel::HelpPanel;
use crate::inbox_ui::InboxPanel;
use crate::events_ui::EventsPanel;
use crate::lead_followup;
use crate::lead_watch::{self, LeadWatchState};
use crate::meta::{self, AgentMeta, CoordEvent};
use crate::pane::{spawn_demo_shell, AgentPane};
use crate::dead_letter;
use crate::mailbox_relay;
use crate::relay::{self, RelayConfig, RelayState};
use crate::report_watch::{self, ReportWatchState};
use crate::tasks_ui::TasksPanel;
use crate::terminal;
use crate::workflow_phase;
use crate::pr_lifecycle;
use crate::ops;

const MAX_PANES: usize = 8;
const SPAWN_STAGGER_MS: u64 = 1000;
const PTY_RESIZE_WARMUP: Duration = Duration::from_secs(5);
const HEALTH_INTERVAL: Duration = Duration::from_secs(2);
const META_INTERVAL: Duration = Duration::from_secs(5);
const PR_POLL_INTERVAL: Duration = Duration::from_secs(60);
const RESTART_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_RESTARTS: u32 = 3;

const MAX_SPAWN_RETRIES: u32 = 2;
const SPAWN_RETRY_DELAY: Duration = Duration::from_secs(3);

struct PendingSpawn {
    rx: Receiver<(usize, Result<AgentPane, String>)>,
    total: usize,
    done: usize,
}

pub struct App {
    pub project_dir: PathBuf,
    pub agents: Vec<AgentSpec>,
    pub panes: Vec<Option<AgentPane>>,
    pub focus: usize,
    pub status: String,
    pub workflow_badge: String,
    pub workflow_phase: workflow_phase::Phase,
    pub workflow_hint: String,
    pub should_quit: bool,
    pub dirty: bool,
    pub terminal_rows: u16,
    pub terminal_cols: u16,
    pub fullscreen: Option<usize>,
    pub lead_agent: String,
    pub inbox_line: String,
    pub health: Vec<HealthState>,
    pub agent_meta: Vec<AgentMeta>,
    pub pane_inner_sizes: Vec<(u16, u16)>,
    pub inbox: InboxPanel,
    pub tasks: TasksPanel,
    pub events: EventsPanel,
    pub help: HelpPanel,
    pub confirm: ConfirmPanel,
    event_ack_lines: Vec<usize>,
    notify_events: Vec<meta::NotifyEvent>,
    coord_events: Vec<CoordEvent>,
    relay: RelayState,
    relay_config: RelayConfig,
    lead_watch: LeadWatchState,
    report_watch: ReportWatchState,
    restart_counts: Vec<u32>,
    last_restart_at: Vec<Option<Instant>>,
    spawn_finished_at: Option<Instant>,
    last_health_check: Instant,
    last_meta_refresh: Instant,
    last_pr_poll: Instant,
    pending_spawn: Option<PendingSpawn>,
    spawn_setup_done: bool,
    spawn_retry_round: u32,
    spawn_retry_at: Option<(Instant, Vec<usize>)>,
}

impl App {
    pub fn new(project_dir: PathBuf) -> Result<Self> {
        let agents = load_agents(&project_dir).unwrap_or_default();
        let lead_agent = resolve_lead_agent(&agents);
        let pane_slots = agents.len().clamp(1, MAX_PANES);
        let wf = workflow_phase::evaluate(&project_dir);
        let lead_watch = LeadWatchState::new(&project_dir, &lead_agent);
        let mut app = Self {
            project_dir: project_dir.clone(),
            agents,
            lead_agent,
            panes: (0..pane_slots).map(|_| None).collect(),
            focus: 0,
            status: String::from("就绪"),
            workflow_badge: workflow_phase::short_label_from(&wf),
            workflow_phase: wf.phase,
            workflow_hint: workflow_phase::action_hint(&wf, 48),
            should_quit: false,
            dirty: true,
            terminal_rows: 24,
            terminal_cols: 80,
            fullscreen: None,
            inbox_line: meta::inbox_snippet(&project_dir, 2),
            health: Vec::new(),
            agent_meta: Vec::new(),
            pane_inner_sizes: Vec::new(),
            inbox: InboxPanel::closed(),
            tasks: TasksPanel::closed(),
            events: EventsPanel::closed(),
            help: HelpPanel::closed(),
            confirm: ConfirmPanel::closed(),
            event_ack_lines: Vec::new(),
            notify_events: Vec::new(),
            coord_events: Vec::new(),
            relay: RelayState::new(&project_dir, 0),
            relay_config: RelayConfig::from_env(),
            lead_watch,
            report_watch: ReportWatchState::new(&project_dir),
            restart_counts: Vec::new(),
            last_restart_at: Vec::new(),
            spawn_finished_at: None,
            last_health_check: Instant::now(),
            last_meta_refresh: Instant::now(),
            last_pr_poll: Instant::now(),
            pending_spawn: None,
            spawn_setup_done: false,
            spawn_retry_round: 0,
            spawn_retry_at: None,
        };
        app.init_tracking_vectors();
        Ok(app)
    }

    pub fn spawn_progress(&self) -> Option<(usize, usize)> {
        self.pending_spawn
            .as_ref()
            .map(|p| (p.done, p.total))
    }

    pub fn agents_alive_count(&self) -> usize {
        self.panes.iter().filter(|p| p.is_some()).count()
    }

    pub fn spawn_retry_countdown_secs(&self) -> Option<u64> {
        let (at, _) = self.spawn_retry_at.as_ref()?;
        let now = Instant::now();
        if now >= *at {
            return Some(0);
        }
        Some(at.duration_since(now).as_secs().saturating_add(1))
    }

    pub fn focused_pane_needs_restart(&self) -> bool {
        if self.panes.get(self.focus).and_then(|p| p.as_ref()).is_none() {
            return true;
        }
        matches!(self.health_state(self.focus), HealthState::Dead(_))
    }

    fn refresh_workflow_ui(&mut self) {
        let snap = workflow_phase::evaluate(&self.project_dir);
        self.workflow_phase = snap.phase;
        self.workflow_badge = workflow_phase::short_label_from(&snap);
        self.workflow_hint = workflow_phase::action_hint(&snap, 48);
    }

    pub fn init_tracking_vectors(&mut self) {
        let n = self.panes.len();
        self.health = vec![HealthState::Ok; n];
        self.event_ack_lines = vec![0; n];
        self.pane_inner_sizes = vec![(24, 80); n];
        self.restart_counts = vec![0; n];
        self.last_restart_at = vec![None; n];
        self.relay.resize(n);
        self.sync_agent_meta();
    }

    fn sync_agent_meta(&mut self) {
        self.coord_events = meta::load_coord_events(&self.project_dir);
        self.notify_events = meta::load_notify_events(&self.project_dir);
        let snapshot = claims::load_claims_snapshot(&self.project_dir);
        let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
        let unread = meta::compute_unread(&names, &self.notify_events, &self.event_ack_lines);
        self.agent_meta = self
            .agents
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let tasks = snapshot
                    .agent_tasks
                    .get(&a.name)
                    .cloned()
                    .unwrap_or_default();
                let conflict = claims::agent_has_conflict(&snapshot, &a.name);
                meta::load_agent_meta(
                    &a.worktree,
                    unread.get(i).copied().unwrap_or(0),
                    tasks,
                    conflict,
                )
            })
            .collect();
    }

    #[allow(dead_code)]
    fn agent_names(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.name.clone()).collect()
    }

    #[allow(dead_code)]
    fn dispatch_new_coord_events(&mut self) {
        let names = self.agent_names();
        self.coord_events = meta::load_coord_events(&self.project_dir);
        let _ = relay::dispatch(
            &self.relay_config,
            &mut self.relay,
            &names,
            &mut self.panes,
            &self.coord_events,
        );
        self.relay
            .persist_with_plan_inbox(self.lead_watch.plan_inbox_line());
        self.dirty = true;
    }

    fn ack_agent_events(&mut self, index: usize) {
        let total = self.notify_events.len();
        if let Some(slot) = self.event_ack_lines.get_mut(index) {
            *slot = total;
        }
        if let Some(m) = self.agent_meta.get_mut(index) {
            m.unread = 0;
        }
    }

    pub fn is_solo(&self) -> bool {
        self.fullscreen.is_some()
    }

    pub fn focus_agent_by_name(&mut self, name: &str) {
        if let Some(idx) = self.agents.iter().position(|a| a.name == name) {
            self.focus = idx;
            self.fullscreen = Some(idx);
            self.dirty = true;
        }
    }

    pub fn show_grid(&mut self) {
        self.fullscreen = None;
        self.dirty = true;
    }

    pub fn show_solo(&mut self) {
        self.fullscreen = Some(self.focus);
        self.dirty = true;
    }

    pub fn fullscreen_index(&self) -> Option<usize> {
        self.fullscreen
    }

    pub fn health_state(&self, index: usize) -> HealthState {
        self.health
            .get(index)
            .cloned()
            .unwrap_or(HealthState::Ok)
    }

    pub fn agent_meta(&self, index: usize) -> Option<&AgentMeta> {
        self.agent_meta.get(index)
    }

    /// Start agents in background threads so the event loop keeps running (avoids Windows console freeze/exit).
    pub fn queue_spawn_all(&mut self, rows: u16, cols: u16) {
        self.terminal_rows = rows;
        self.terminal_cols = cols;
        if self.agents.is_empty() {
            self.status = "agents.yaml 无已启用 Agent — 启动演示 shell".into();
            let (h, w) = estimate_pane_size(rows, cols, 1);
            if let Ok(pane) = spawn_demo_shell("demo", h, w, &self.project_dir, &self.lead_agent) {
                if !self.panes.is_empty() {
                    self.panes[0] = Some(pane);
                }
            }
            self.finish_spawn_setup();
            self.dirty = true;
            return;
        }

        let count = self.panes.len();
        let (per_rows, per_cols) = estimate_pane_size(rows, cols, count);
        let (tx, rx) = mpsc::channel();
        self.status = format!("正在启动 {count} 个 Agent…");
        self.pending_spawn = Some(PendingSpawn {
            rx,
            total: count,
            done: 0,
        });
        self.dirty = true;

        for (i, spec) in self.agents.iter().enumerate().take(count) {
            let tx = tx.clone();
            let spec = spec.clone();
            let project_dir = self.project_dir.clone();
            let lead = self.lead_agent.clone();
            let delay_ms = (i as u64).saturating_mul(SPAWN_STAGGER_MS);
            thread::spawn(move || {
                if delay_ms > 0 {
                    thread::sleep(Duration::from_millis(delay_ms));
                }
                let result = AgentPane::spawn(spec, per_rows, per_cols, &project_dir, &lead)
                    .map_err(|e| e.to_string());
                let _ = tx.send((i, result));
            });
        }
        drop(tx);
    }

    fn queue_spawn_indices(&mut self, indices: &[usize]) {
        if indices.is_empty() {
            return;
        }
        let count = self.panes.len();
        let (per_rows, per_cols) =
            estimate_pane_size(self.terminal_rows, self.terminal_cols, count);
        let (tx, rx) = mpsc::channel();
        let n = indices.len();
        self.status = format!("正在重试启动 {n} 个 Agent…");
        self.pending_spawn = Some(PendingSpawn {
            rx,
            total: n,
            done: 0,
        });
        self.dirty = true;

        for (attempt, &i) in indices.iter().enumerate() {
            let Some(spec) = self.agents.get(i).cloned() else {
                continue;
            };
            let tx = tx.clone();
            let project_dir = self.project_dir.clone();
            let lead = self.lead_agent.clone();
            let delay_ms = (attempt as u64).saturating_mul(SPAWN_STAGGER_MS);
            thread::spawn(move || {
                if delay_ms > 0 {
                    thread::sleep(Duration::from_millis(delay_ms));
                }
                let result = AgentPane::spawn(spec, per_rows, per_cols, &project_dir, &lead)
                    .map_err(|e| e.to_string());
                let _ = tx.send((i, result));
            });
        }
        drop(tx);
    }

    fn failed_spawn_indices(&self) -> Vec<usize> {
        self.panes
            .iter()
            .enumerate()
            .filter_map(|(i, p)| if p.is_none() { Some(i) } else { None })
            .collect()
    }

    fn spawn_failure_status(&self, failed: &[usize]) -> String {
        let details: Vec<String> = failed
            .iter()
            .filter_map(|i| self.agents.get(*i))
            .map(|a| format!("{}（cmd={}）", a.name, a.command))
            .collect();
        format!(
            "{} 个 Agent 未启动：{}。检查 agents.yaml / AGENT_BIN，Ctrl+I !doctor",
            failed.len(),
            details.join(", ")
        )
    }

    fn try_finish_spawn_batch(&mut self) {
        let failed = self.failed_spawn_indices();
        if !failed.is_empty() && self.spawn_retry_round < MAX_SPAWN_RETRIES {
            self.spawn_retry_round += 1;
            self.spawn_retry_at = Some((
                Instant::now() + SPAWN_RETRY_DELAY,
                failed,
            ));
            self.status = format!(
                "{} — 3秒后自动重试 ({}/{})",
                self.spawn_failure_status(&self.failed_spawn_indices()),
                self.spawn_retry_round,
                MAX_SPAWN_RETRIES
            );
            self.dirty = true;
            return;
        }
        let ok = self.panes.iter().filter(|p| p.is_some()).count();
        if failed.is_empty() {
            self.status = format!(
                "已启动 {ok} 个 Agent — Ctrl+I !任务 说需求  |  Ctrl+G 速查  |  F2 单人",
                ok = ok,
            );
        } else {
            self.status = self.spawn_failure_status(&failed);
        }
        self.finish_spawn_setup();
        self.dirty = true;
    }

    fn finish_spawn_setup(&mut self) {
        if self.spawn_setup_done {
            return;
        }
        self.spawn_setup_done = true;
        self.spawn_finished_at = Some(Instant::now());
        if crate::coord_dedupe::initial_briefing_already_sent(&self.project_dir, &self.lead_agent)
        {
            self.lead_watch.skip_persisted_initial_briefing();
        } else {
            self.lead_watch
                .arm_briefing(Instant::now() + lead_watch::BRIEFING_DELAY);
        }
        if let Some(spec) = self.agents.iter().find(|a| a.name == self.lead_agent) {
            if let Err(e) = sync_lead_coordination_rules(&self.project_dir, &spec.worktree) {
                terminal::log_message(
                    &self.project_dir,
                    "warn",
                    &format!("sync lead coordination rules: {e}"),
                );
            }
        }
        let agent_pairs: Vec<(String, String)> = self
            .agents
            .iter()
            .map(|a| (a.name.clone(), a.role.clone()))
            .collect();
        if let Err(e) =
            agent_memory::on_startup(&self.project_dir, &agent_pairs, &self.lead_agent)
        {
            terminal::log_message(
                &self.project_dir,
                "warn",
                &format!("agent memory init: {e}"),
            );
        }
        if let Err(e) = crate::memory_fts::maybe_reindex_on_startup(&self.project_dir) {
            terminal::log_message(
                &self.project_dir,
                "warn",
                &format!("memory FTS reindex: {e}"),
            );
        }
        self.coord_events = meta::load_coord_events(&self.project_dir);
        self.relay.cursor = self.coord_events.len();
        self.relay.mailbox_line =
            crate::mailbox::load_all_entries(&self.project_dir).len();
    }

    fn poll_pending_spawns(&mut self) {
        let Some(pending) = self.pending_spawn.as_mut() else {
            return;
        };
        while let Ok((i, result)) = pending.rx.try_recv() {
            pending.done += 1;
            match result {
                Ok(pane) => {
                    if i < self.panes.len() {
                        self.panes[i] = Some(pane);
                        self.health[i] = HealthState::Ok;
                    }
                }
                Err(e) => {
                    terminal::log_message(
                        &self.project_dir,
                        "error",
                        &format!("failed to spawn pane {i}: {e}"),
                    );
                    if i < self.health.len() {
                        self.health[i] = HealthState::Dead(format!("启动失败：{e}"));
                    }
                }
            }
        }
        if pending.done < pending.total {
            self.status = format!(
                "正在启动 Agent… {}/{}",
                pending.done, pending.total
            );
            self.dirty = true;
        }
        if pending.done >= pending.total {
            self.pending_spawn = None;
            self.try_finish_spawn_batch();
        }
    }

    #[allow(dead_code)]
    pub fn spawn_all(&mut self, rows: u16, cols: u16) {
        self.queue_spawn_all(rows, cols);
        while self.pending_spawn.is_some() {
            self.poll_pending_spawns();
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some((retry_at, indices)) = self.spawn_retry_at.take() {
            if now >= retry_at {
                self.queue_spawn_indices(&indices);
            } else {
                self.spawn_retry_at = Some((retry_at, indices));
            }
        }
        self.poll_pending_spawns();
        if relay::process_pending_wakes(&mut self.panes, &mut self.relay) > 0 {
            self.dirty = true;
        }

        let lead_index = self
            .agents
            .iter()
            .position(|a| a.name == self.lead_agent);
        if self.pending_spawn.is_none() {
            if let Some(idx) = lead_index {
                let lead_pane = self.panes.get(idx).and_then(|p| p.as_ref());
                if lead_watch::process_briefing_retries(
                    &self.project_dir,
                    &self.lead_agent,
                    &mut self.lead_watch,
                    lead_pane,
                ) > 0
                {
                    self.dirty = true;
                }
                if lead_watch::process_pending_rebrief(
                    &self.project_dir,
                    &self.lead_agent,
                    &mut self.lead_watch,
                    lead_pane,
                )
                .unwrap_or(false)
                {
                    self.dirty = true;
                }
            }
        }

            if now.duration_since(self.last_meta_refresh) >= META_INTERVAL {
            self.inbox_line = meta::inbox_snippet(&self.project_dir, 2);
            self.refresh_workflow_ui();
            if self.pending_spawn.is_none()
                && now.duration_since(self.last_pr_poll) >= PR_POLL_INTERVAL
            {
                self.last_pr_poll = now;
                let _ = pr_lifecycle::maybe_auto_merge(&self.project_dir, &self.lead_agent);
                let _ = pr_lifecycle::maybe_poll_merged(&self.project_dir, &self.lead_agent);
            }
            if self.inbox.open {
                self.inbox.refresh(&self.project_dir);
            }
            if self.events.open {
                self.events.refresh(&self.project_dir);
            }
            self.sync_agent_meta();
            // Skip relay/orchestration until agents finished spawning.
            if self.pending_spawn.is_none() {
            let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
            let delivered = relay::dispatch(
                &self.relay_config,
                &mut self.relay,
                &names,
                &mut self.panes,
                &self.coord_events,
            );
            if delivered > 0 {
                self.dirty = true;
            }
            let mb_delivered = mailbox_relay::dispatch_from_mailbox(
                &self.relay_config,
                &mut self.relay,
                &names,
                &mut self.panes,
                &self.project_dir,
            );
            if mb_delivered > 0 {
                self.dirty = true;
            }
            self.relay.persist_with_plan_inbox(self.lead_watch.plan_inbox_line());

            let retried = dead_letter::process_retry_queue(&self.project_dir, &self.lead_agent);
            if retried > 0 {
                self.coord_events = meta::load_coord_events(&self.project_dir);
                self.status = format!("自动重试已重新委派 {retried} 个任务");
                self.dirty = true;
            }

            let lead_index = self
                .agents
                .iter()
                .position(|a| a.name == self.lead_agent);
            if let Some(idx) = lead_index {
                let briefing_sent = {
                    let lead_pane = self.panes.get(idx).and_then(|p| p.as_ref());
                    lead_watch::maybe_send_briefing(
                        &self.project_dir,
                        &self.lead_agent,
                        &mut self.lead_watch,
                        lead_pane,
                    )
                    .unwrap_or(false)
                };
                if briefing_sent {
                    self.dirty = true;
                }
                let periodic = {
                    let lead_pane = self.panes.get(idx).and_then(|p| p.as_ref());
                    lead_watch::maybe_periodic_rebrief(
                        &self.project_dir,
                        &self.lead_agent,
                        &mut self.lead_watch,
                        lead_pane,
                    )
                    .unwrap_or(false)
                };
                if periodic {
                    self.dirty = true;
                }
                let dispatched = lead_watch::watch_lead_pane(
                    &self.project_dir,
                    &self.lead_agent,
                    idx,
                    &names,
                    &mut self.panes,
                    &mut self.lead_watch,
                );
                let from_inbox = lead_watch::watch_plan_inbox(
                    &self.project_dir,
                    &self.lead_agent,
                    &names,
                    &mut self.lead_watch,
                );
                let total = dispatched + from_inbox;
                if total > 0 {
                    self.coord_events = meta::load_coord_events(&self.project_dir);
                    self.status = format!("主 Agent 已按计划派发 {total} 个子任务");
                    self.dirty = true;
                }
                let lead_pane = self.panes.get(idx).and_then(|p| p.as_ref());
                let followup = lead_followup::watch_lead_followup(
                    &self.project_dir,
                    &self.lead_agent,
                    &names,
                    lead_pane,
                    &mut self.lead_watch,
                );
                if followup.dispatched > 0 {
                    self.coord_events = meta::load_coord_events(&self.project_dir);
                    self.status =
                        format!("续派 transcript 扫描派发 {} 个子任务", followup.dispatched);
                    self.dirty = true;
                } else if followup.nudged > 0 {
                    self.status = format!("已催促主 Agent 续派 agent-plan（{}）", followup.nudged);
                    self.dirty = true;
                }
            }

            if let Some(idx) = lead_index {
                let reported = report_watch::watch_worker_panes(
                    &self.project_dir,
                    &self.lead_agent,
                    idx,
                    &mut self.panes,
                    &mut self.report_watch,
                );
                if reported > 0 {
                    self.coord_events = meta::load_coord_events(&self.project_dir);
                    self.status = format!("已自动回传 {reported} 条工人回执给主 Agent");
                    self.dirty = true;
                    let rescanned = lead_watch::watch_lead_pane(
                        &self.project_dir,
                        &self.lead_agent,
                        idx,
                        &names,
                        &mut self.panes,
                        &mut self.lead_watch,
                    );
                    if rescanned > 0 {
                        self.coord_events = meta::load_coord_events(&self.project_dir);
                        self.status =
                            format!("回执后立即扫描 lead transcript，派发 {rescanned} 个子任务");
                        self.dirty = true;
                    }
                    let lead_pane = self.panes.get(idx).and_then(|p| p.as_ref());
                    let followup = lead_followup::watch_lead_followup(
                        &self.project_dir,
                        &self.lead_agent,
                        &names,
                        lead_pane,
                        &mut self.lead_watch,
                    );
                    if followup.dispatched > 0 {
                        self.coord_events = meta::load_coord_events(&self.project_dir);
                        self.status = format!(
                            "回执后续派 transcript 扫描，派发 {} 个子任务",
                            followup.dispatched
                        );
                        self.dirty = true;
                    }
                    let rebrief = {
                        let lead_pane = self.panes.get(idx).and_then(|p| p.as_ref());
                        lead_watch::maybe_rebrief_after_reports(
                            &self.project_dir,
                            &self.lead_agent,
                            &mut self.lead_watch,
                            lead_pane,
                            reported,
                        )
                        .unwrap_or(false)
                    };
                    if rebrief {
                        self.dirty = true;
                    }
                }
            }
            }

            for (i, spec) in self.agents.iter().enumerate() {
                if let Some(m) = self.agent_meta.get_mut(i) {
                    m.branch = meta::branch_for_worktree(&spec.worktree);
                }
            }
            self.last_meta_refresh = now;
            self.dirty = true;
        }

        if now.duration_since(self.last_health_check) >= HEALTH_INTERVAL {
            self.last_health_check = now;
            self.check_health_and_restart();
        }
    }

    fn check_health_and_restart(&mut self) {
        if self.pending_spawn.is_some() {
            return;
        }
        let mut restarted = false;
        for i in 0..self.panes.len() {
            if self.panes[i].is_none() {
                if self.can_restart(i)
                    && self.restart_pane(i) {
                        restarted = true;
                        if let Some(spec) = self.agents.get(i) {
                            self.status = format!("已重启 {}（进程缺失）", spec.name);
                        }
                    }
                continue;
            }

            let Some(pane) = self.panes.get(i).and_then(|p| p.as_ref()) else {
                continue;
            };
            let text = pane.visible_text();
            let state = health::scan_screen(&text);
            if let Some(slot) = self.health.get_mut(i) {
                *slot = state.clone();
            }

            if let HealthState::Dead(reason) = state {
                if self.can_restart(i)
                    && self.restart_pane(i) {
                        restarted = true;
                        if let Some(spec) = self.agents.get(i) {
                            self.status = format!("已重启 {}（{reason}）", spec.name);
                        }
                    }
            }
        }
        if restarted {
            self.dirty = true;
        }
    }

    fn can_restart(&self, index: usize) -> bool {
        let count = self.restart_counts.get(index).copied().unwrap_or(0);
        if count >= MAX_RESTARTS {
            return false;
        }
        match self.last_restart_at.get(index).copied().flatten() {
            None => true,
            Some(t) => t.elapsed() >= RESTART_COOLDOWN,
        }
    }

    fn restart_pane(&mut self, index: usize) -> bool {
        let Some(spec) = self.agents.get(index).cloned() else {
            return false;
        };
        let (rows, cols) = self
            .pane_inner_sizes
            .get(index)
            .copied()
            .unwrap_or_else(|| {
                estimate_pane_size(
                    self.terminal_rows,
                    self.terminal_cols,
                    self.panes.len().max(1),
                )
            });

        self.panes[index] = None;
        match AgentPane::spawn(spec.clone(), rows, cols, &self.project_dir, &self.lead_agent) {
            Ok(pane) => {
                self.panes[index] = Some(pane);
                self.health[index] = HealthState::Ok;
                if let Some(c) = self.restart_counts.get_mut(index) {
                    *c += 1;
                }
                if let Some(t) = self.last_restart_at.get_mut(index) {
                    *t = Some(Instant::now());
                }
                if spec.name == self.lead_agent {
                    let _ = sync_lead_coordination_rules(&self.project_dir, &spec.worktree);
                    lead_watch::on_lead_pane_restarted(&mut self.lead_watch);
                }
                true
            }
            Err(e) => {
                terminal::log_message(
                    &self.project_dir,
                    "error",
                    &format!("restart pane {index} failed: {e:#}"),
                );
                self.health[index] = HealthState::Dead(format!("重启失败：{e:#}"));
                false
            }
        }
    }

    pub fn can_sync_pty_size(&self) -> bool {
        self.spawn_finished_at
            .is_some_and(|t| t.elapsed() >= PTY_RESIZE_WARMUP)
    }

    pub fn resize_terminal(&mut self, rows: u16, cols: u16) {
        if self.terminal_rows == rows && self.terminal_cols == cols {
            return;
        }
        self.terminal_rows = rows;
        self.terminal_cols = cols;
        self.dirty = true;
    }

    pub fn sync_pane_pty_size(&mut self, index: usize, rows: u16, cols: u16) {
        if let Some(slot) = self.pane_inner_sizes.get_mut(index) {
            *slot = (rows, cols);
        }
        if !self.can_sync_pty_size() {
            return;
        }
        let Some(pane) = self.panes.get_mut(index).and_then(|p| p.as_mut()) else {
            return;
        };
        let (cur_rows, cur_cols) = pane.dimensions();
        if cur_rows == rows && cur_cols == cols {
            return;
        }
        pane.resize(rows, cols);
    }

    pub fn toggle_view(&mut self) {
        if self.is_solo() {
            self.show_grid();
        } else {
            self.show_solo();
        }
        self.dirty = true;
    }

    pub fn set_focus(&mut self, idx: usize) {
        if idx < self.panes.len() {
            self.focus = idx;
            self.ack_agent_events(idx);
            if self.fullscreen.is_some() {
                self.fullscreen = Some(idx);
            }
            if let Some(spec) = self.agents.get(idx) {
                self.status = format!("聚焦 {}", spec.name);
            } else {
                self.status = format!("面板 {}", idx + 1);
            }
            self.dirty = true;
        }
    }

    pub fn needs_redraw(&self) -> bool {
        if self.dirty {
            return true;
        }
        if self.panes.iter().flatten().any(|p| p.is_dirty()) {
            return true;
        }
        false
    }

    pub fn mark_rendered(&mut self) {
        self.dirty = false;
        for pane in self.panes.iter_mut().flatten() {
            pane.mark_clean();
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let agent_names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
        let focused_agent = self.agents.get(self.focus).map(|a| a.name.as_str());

        let confirm_outcome = self.confirm.handle_key(code, modifiers);
        if confirm_outcome.consumed {
            if let Some(verb) = confirm_outcome.confirmed {
                match ops::execute(&self.project_dir, &self.lead_agent, verb) {
                    Ok(status) => {
                        self.status = truncate_ops_status(&status);
                        self.refresh_workflow_ui();
                    }
                    Err(e) => {
                        self.status = format!("操作失败：{e:#}");
                    }
                }
            } else if let Some(s) = confirm_outcome.status.filter(|s| !s.is_empty()) {
                self.status = s;
            }
            self.dirty = true;
            return;
        }

        let help_outcome = self
            .help
            .handle_key(code, modifiers, &self.project_dir);
        if help_outcome.consumed {
            if self.help.open {
                self.inbox.close();
                self.tasks.close();
                self.events.close();
            }
            if let Some(s) = help_outcome.status.filter(|s| !s.is_empty()) {
                self.status = s;
            } else if !self.help.open {
                self.status = "就绪".into();
            }
            self.dirty = true;
            return;
        }

        let outcome = self.inbox.handle_key(
            code,
            modifiers,
            &self.project_dir,
            &agent_names,
            focused_agent,
            &self.lead_agent,
        );
        if outcome.consumed {
            let has_side_effect = outcome.side_effect.is_some();
            let has_pending_confirm = outcome.pending_confirm.is_some();
            if let Some(effect) = outcome.side_effect {
                match effect {
                    crate::ops::OpsVerb::ShowGuide => self.help.open_guide(&self.project_dir),
                    crate::ops::OpsVerb::ShowNext => self.help.open_next(&self.project_dir),
                    _ => {}
                }
            }
            if let Some(verb) = outcome.pending_confirm {
                self.confirm.open_for(verb);
            }
            if self.inbox.open {
                self.tasks.close();
                self.events.close();
                if !has_side_effect && !has_pending_confirm {
                    self.help.close();
                    self.confirm.close();
                }
            }
            if let Some(s) = outcome.status.filter(|s| !s.is_empty()) {
                self.status = s;
                self.inbox_line = meta::inbox_snippet(&self.project_dir, 2);
                self.sync_agent_meta();
                self.refresh_workflow_ui();
            } else if !self.inbox.open {
                self.status = "就绪".into();
            }
            self.dirty = true;
            return;
        }

        let tasks_outcome = self
            .tasks
            .handle_key(code, modifiers, &self.project_dir);
        if tasks_outcome.consumed {
            if self.tasks.open {
                self.inbox.close();
                self.events.close();
                self.help.close();
                self.confirm.close();
            }
            if let Some(s) = tasks_outcome.status.filter(|s| !s.is_empty()) {
                self.status = s;
            } else if !self.tasks.open {
                self.status = "就绪".into();
            }
            self.dirty = true;
            return;
        }

        let events_outcome = self
            .events
            .handle_key(code, modifiers, &self.project_dir);
        if events_outcome.consumed {
            if self.events.open {
                self.inbox.close();
                self.tasks.close();
                self.help.close();
                self.confirm.close();
            }
            if let Some(s) = events_outcome.status.filter(|s| !s.is_empty()) {
                self.status = s;
            } else if !self.events.open {
                self.status = "就绪".into();
            }
            self.dirty = true;
            return;
        }

        // Chrome shortcuts use Ctrl+* / F2 so Tab, digits, f, Esc stay with the agent.
        if modifiers.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('q' | 'Q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char(c @ '1'..='8') => {
                    let idx = (c as u8 - b'1') as usize;
                    if idx < self.panes.len() {
                        self.set_focus(idx);
                    }
                    return;
                }
                KeyCode::Tab => {
                    let next = if modifiers.contains(KeyModifiers::SHIFT) {
                        (self.focus + self.panes.len().saturating_sub(1)) % self.panes.len()
                    } else {
                        (self.focus + 1) % self.panes.len()
                    };
                    self.set_focus(next);
                    return;
                }
                _ => {}
            }
        }

        if code == KeyCode::F(2) {
            self.toggle_view();
            return;
        }

        if code == KeyCode::F(5) && self.focused_pane_needs_restart() {
            let name = self
                .agents
                .get(self.focus)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| format!("{}", self.focus + 1));
            if self.restart_pane(self.focus) {
                self.status = format!("F5 已重启 {name}");
            } else {
                self.status = format!("F5 重启 {name} 失败 — 看 agents.yaml / !doctor");
            }
            self.dirty = true;
            return;
        }

        let Some(pane) = self.panes.get(self.focus).and_then(|p| p.as_ref()) else {
            return;
        };

        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                pane.send_input(&[0x03]);
            }
            KeyCode::Char(c) => pane.send_input(c.to_string().as_bytes()),
            KeyCode::Backspace => pane.send_input(&[0x08]),
            KeyCode::Tab => pane.send_input(b"\t"),
            KeyCode::Esc => pane.send_input(&[0x1b]),
            KeyCode::Enter => {
                #[cfg(windows)]
                pane.send_input(b"\r");
                #[cfg(not(windows))]
                pane.send_input(b"\n");
            }
            KeyCode::Left => pane.send_input(&[27, 91, 68]),
            KeyCode::Right => pane.send_input(&[27, 91, 67]),
            KeyCode::Up => pane.send_input(&[27, 91, 65]),
            KeyCode::Down => pane.send_input(&[27, 91, 66]),
            _ => {}
        }
        self.dirty = true;
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.handle_key(key.code, key.modifiers);
            }
            Event::Resize(_, _) => self.dirty = true,
            _ => {}
        }
    }
}

fn truncate_ops_status(s: &str) -> String {
    const MAX: usize = 200;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX).collect();
    out.push('…');
    out
}

pub(crate) fn estimate_pane_size(rows: u16, cols: u16, count: usize) -> (u16, u16) {
    let usable_rows = rows.saturating_sub(3);
    let (pane_h, pane_w) = match count {
        0 | 1 => (usable_rows, cols),
        2 => (usable_rows, cols / 2),
        3 => (usable_rows, cols / 3),
        4 => (usable_rows / 2, cols / 2),
        _ => (usable_rows / 2, cols / 3),
    };
    (
        pane_h.saturating_sub(2).max(8),
        pane_w.saturating_sub(2).max(24),
    )
}

pub fn poll_event(timeout: Duration) -> Result<Option<Event>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    match event::read() {
        Ok(ev) => Ok(Some(ev)),
        Err(e) => Err(e.into()),
    }
}
