//! App state and every pure decision function: sorting, key-to-action
//! mapping, sparkline text rendering, aggregation. Nothing here touches
//! a terminal or a socket -- that's `ui.rs`/`main.rs`'s job -- so all
//! of it is directly unit-testable.

use crossterm::event::KeyCode;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};

/// How many samples of history to keep per pipeline for the inline
/// sparkline column. With the default 1s poll interval that's 30s of
/// visible history -- enough to be useful without unbounded growth
/// over a long-running session.
pub const HISTORY_LEN: usize = 30;

#[derive(Deserialize, Debug, Clone)]
pub struct PipelineStat {
    pub name: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections_total: u64,
    pub connections_active: u64,
    pub errors_total: u64,
}

/// Mirrors the control socket's wire shape exactly (`src/control.rs`'s
/// `stats_response`) -- deliberately not a shared type with `wf-core`,
/// same decoupling the control socket itself already uses: the JSON
/// contract is the interface, not an internal Rust type.
#[derive(Deserialize, Debug, Default)]
pub struct StatsResponse {
    #[serde(default)]
    pub pipelines: Vec<PipelineStat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Name,
    Active,
    Total,
    BytesIn,
    BytesOut,
    Errors,
}

impl SortField {
    pub fn label(&self) -> &'static str {
        match self {
            SortField::Name => "Name",
            SortField::Active => "Active",
            SortField::Total => "Total",
            SortField::BytesIn => "In",
            SortField::BytesOut => "Out",
            SortField::Errors => "Errors",
        }
    }
}

/// Cycles through every sort field in a fixed order, wrapping back to
/// the start -- bound to `s` in Normal mode.
pub fn next_sort_field(current: SortField) -> SortField {
    match current {
        SortField::Name => SortField::Active,
        SortField::Active => SortField::Total,
        SortField::Total => SortField::BytesIn,
        SortField::BytesIn => SortField::BytesOut,
        SortField::BytesOut => SortField::Errors,
        SortField::Errors => SortField::Name,
    }
}

/// Sorts in place by `field`, ascending unless `desc`. Ties fall back
/// to name order, so the row order stays stable and predictable when
/// two pipelines have equal values (e.g. both idle).
pub fn sort_pipelines(pipelines: &mut [PipelineStat], field: SortField, desc: bool) {
    pipelines.sort_by(|a, b| {
        let ordering = match field {
            SortField::Name => a.name.cmp(&b.name),
            SortField::Active => a.connections_active.cmp(&b.connections_active),
            SortField::Total => a.connections_total.cmp(&b.connections_total),
            SortField::BytesIn => a.bytes_in.cmp(&b.bytes_in),
            SortField::BytesOut => a.bytes_out.cmp(&b.bytes_out),
            SortField::Errors => a.errors_total.cmp(&b.errors_total),
        };
        let ordering = if desc { ordering.reverse() } else { ordering };
        ordering.then_with(|| a.name.cmp(&b.name))
    });
}

/// Sums every field across all pipelines into one synthetic row named
/// "TOTAL" -- an at-a-glance aggregate, not a real pipeline.
pub fn aggregate(pipelines: &[PipelineStat]) -> PipelineStat {
    let mut total = PipelineStat {
        name: "TOTAL".to_string(),
        bytes_in: 0,
        bytes_out: 0,
        connections_total: 0,
        connections_active: 0,
        errors_total: 0,
    };
    for p in pipelines {
        total.bytes_in += p.bytes_in;
        total.bytes_out += p.bytes_out;
        total.connections_total += p.connections_total;
        total.connections_active += p.connections_active;
        total.errors_total += p.errors_total;
    }
    total
}

/// Renders up to the last `width` values as Unicode block-height
/// characters, low to high. Empty input or a flat all-zero series
/// renders as the lowest bar throughout rather than dividing by zero.
pub fn sparkline_chars(values: &[u64], width: usize) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() {
        return BLOCKS[0].to_string().repeat(width.max(1));
    }
    let start = values.len().saturating_sub(width);
    let slice = &values[start..];
    let max = slice.iter().copied().max().unwrap_or(0).max(1);
    slice
        .iter()
        .map(|&v| {
            let level = ((v as f64 / max as f64) * (BLOCKS.len() - 1) as f64).round() as usize;
            BLOCKS[level.min(BLOCKS.len() - 1)]
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Help,
    Detail,
}

/// Every real effect a keypress can have -- `main.rs`'s event loop
/// matches on this to mutate `App` or the terminal; `handle_key` itself
/// touches neither, so the whole mapping is testable without a
/// terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    MoveUp,
    MoveDown,
    CycleSort,
    ReverseSort,
    TogglePause,
    OpenHelp,
    OpenDetail,
    Close,
    None,
}

/// Pure key-to-action mapping, one mode at a time. `q` quits from
/// anywhere; every other binding is mode-specific.
pub fn handle_key(mode: Mode, key: KeyCode) -> Action {
    if matches!(key, KeyCode::Char('q')) {
        return Action::Quit;
    }

    match mode {
        Mode::Normal => match key {
            KeyCode::Esc => Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
            KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
            KeyCode::Char('s') => Action::CycleSort,
            KeyCode::Char('r') => Action::ReverseSort,
            KeyCode::Char('p') => Action::TogglePause,
            KeyCode::Char('?') => Action::OpenHelp,
            KeyCode::Enter => Action::OpenDetail,
            _ => Action::None,
        },
        Mode::Help | Mode::Detail => match key {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => Action::Close,
            _ => Action::None,
        },
    }
}

pub struct App {
    pub pipelines: Vec<PipelineStat>,
    pub history: HashMap<String, VecDeque<u64>>,
    pub selected: usize,
    pub sort_field: SortField,
    pub sort_desc: bool,
    pub paused: bool,
    pub mode: Mode,
    pub last_error: Option<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            pipelines: Vec::new(),
            history: HashMap::new(),
            selected: 0,
            sort_field: SortField::Name,
            sort_desc: false,
            paused: false,
            mode: Mode::Normal,
            last_error: None,
        }
    }

    /// Replaces the current pipeline set, re-sorts, pushes one new
    /// history sample per pipeline (capped at `HISTORY_LEN`), and
    /// clamps `selected` so it never points past the new row count.
    pub fn update(&mut self, pipelines: Vec<PipelineStat>) {
        self.pipelines = pipelines;
        sort_pipelines(&mut self.pipelines, self.sort_field, self.sort_desc);

        for p in &self.pipelines {
            let entry = self.history.entry(p.name.clone()).or_default();
            entry.push_back(p.bytes_in + p.bytes_out);
            while entry.len() > HISTORY_LEN {
                entry.pop_front();
            }
        }

        if self.pipelines.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.pipelines.len() {
            self.selected = self.pipelines.len() - 1;
        }
    }

    pub fn selected_pipeline(&self) -> Option<&PipelineStat> {
        self.pipelines.get(self.selected)
    }

    /// Owned copy (not a borrow) of the selected pipeline's history --
    /// simplest way to hand data to a `Sparkline` widget, which just
    /// needs an iterator of values, not a live view into `self`.
    pub fn selected_history(&self) -> Vec<u64> {
        self.selected_pipeline()
            .and_then(|p| self.history.get(&p.name))
            .map(|h| h.iter().copied().collect())
            .unwrap_or_default()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(name: &str, active: u64, total: u64, bin: u64, bout: u64, errors: u64) -> PipelineStat {
        PipelineStat {
            name: name.to_string(),
            bytes_in: bin,
            bytes_out: bout,
            connections_total: total,
            connections_active: active,
            errors_total: errors,
        }
    }

    #[test]
    fn next_sort_field_cycles_through_every_field_and_wraps() {
        let mut f = SortField::Name;
        let mut seen = vec![f];
        for _ in 0..5 {
            f = next_sort_field(f);
            seen.push(f);
        }
        assert_eq!(next_sort_field(f), SortField::Name);
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn sort_pipelines_by_name_ascending() {
        let mut p = vec![stat("b", 0, 0, 0, 0, 0), stat("a", 0, 0, 0, 0, 0)];
        sort_pipelines(&mut p, SortField::Name, false);
        assert_eq!(p[0].name, "a");
    }

    #[test]
    fn sort_pipelines_by_errors_descending() {
        let mut p = vec![stat("a", 0, 0, 0, 0, 1), stat("b", 0, 0, 0, 0, 5)];
        sort_pipelines(&mut p, SortField::Errors, true);
        assert_eq!(p[0].name, "b");
    }

    #[test]
    fn sort_pipelines_breaks_ties_by_name() {
        let mut p = vec![stat("z", 1, 0, 0, 0, 0), stat("a", 1, 0, 0, 0, 0)];
        sort_pipelines(&mut p, SortField::Active, false);
        assert_eq!(p[0].name, "a");
    }

    #[test]
    fn aggregate_sums_every_field() {
        let p = vec![stat("a", 1, 2, 3, 4, 5), stat("b", 10, 20, 30, 40, 50)];
        let total = aggregate(&p);
        assert_eq!(total.name, "TOTAL");
        assert_eq!(total.connections_active, 11);
        assert_eq!(total.connections_total, 22);
        assert_eq!(total.bytes_in, 33);
        assert_eq!(total.bytes_out, 44);
        assert_eq!(total.errors_total, 55);
    }

    #[test]
    fn aggregate_of_empty_is_all_zero() {
        let total = aggregate(&[]);
        assert_eq!(total.connections_active, 0);
    }

    #[test]
    fn sparkline_chars_empty_input_is_flat() {
        assert_eq!(sparkline_chars(&[], 4), "▁▁▁▁");
    }

    #[test]
    fn sparkline_chars_all_zero_is_flat_not_a_panic() {
        assert_eq!(sparkline_chars(&[0, 0, 0], 3), "▁▁▁");
    }

    #[test]
    fn sparkline_chars_shows_only_the_last_width_values() {
        let values: Vec<u64> = (1..=10).collect();
        let out = sparkline_chars(&values, 3);
        assert_eq!(out.chars().count(), 3);
    }

    #[test]
    fn sparkline_chars_highest_value_is_the_tallest_block() {
        let out = sparkline_chars(&[0, 5, 10], 3);
        assert_eq!(out.chars().last(), Some('█'));
    }

    #[test]
    fn handle_key_quits_from_every_mode() {
        for mode in [Mode::Normal, Mode::Help, Mode::Detail] {
            assert_eq!(handle_key(mode, KeyCode::Char('q')), Action::Quit);
        }
    }

    #[test]
    fn handle_key_normal_mode_bindings() {
        assert_eq!(handle_key(Mode::Normal, KeyCode::Esc), Action::Quit);
        assert_eq!(handle_key(Mode::Normal, KeyCode::Up), Action::MoveUp);
        assert_eq!(handle_key(Mode::Normal, KeyCode::Char('k')), Action::MoveUp);
        assert_eq!(handle_key(Mode::Normal, KeyCode::Down), Action::MoveDown);
        assert_eq!(
            handle_key(Mode::Normal, KeyCode::Char('j')),
            Action::MoveDown
        );
        assert_eq!(
            handle_key(Mode::Normal, KeyCode::Char('s')),
            Action::CycleSort
        );
        assert_eq!(
            handle_key(Mode::Normal, KeyCode::Char('r')),
            Action::ReverseSort
        );
        assert_eq!(
            handle_key(Mode::Normal, KeyCode::Char('p')),
            Action::TogglePause
        );
        assert_eq!(
            handle_key(Mode::Normal, KeyCode::Char('?')),
            Action::OpenHelp
        );
        assert_eq!(handle_key(Mode::Normal, KeyCode::Enter), Action::OpenDetail);
        assert_eq!(handle_key(Mode::Normal, KeyCode::Char('z')), Action::None);
    }

    #[test]
    fn handle_key_help_and_detail_close_on_esc_question_or_enter() {
        for mode in [Mode::Help, Mode::Detail] {
            assert_eq!(handle_key(mode, KeyCode::Esc), Action::Close);
            assert_eq!(handle_key(mode, KeyCode::Char('?')), Action::Close);
            assert_eq!(handle_key(mode, KeyCode::Enter), Action::Close);
            assert_eq!(handle_key(mode, KeyCode::Char('z')), Action::None);
        }
    }

    #[test]
    fn app_update_clamps_selection_when_pipelines_shrink() {
        let mut app = App::new();
        app.update(vec![
            stat("a", 0, 0, 0, 0, 0),
            stat("b", 0, 0, 0, 0, 0),
            stat("c", 0, 0, 0, 0, 0),
        ]);
        app.selected = 2;
        app.update(vec![stat("a", 0, 0, 0, 0, 0)]);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn app_update_builds_bounded_history() {
        let mut app = App::new();
        for i in 0..(HISTORY_LEN as u64 + 10) {
            app.update(vec![stat("a", 0, 0, i, 0, 0)]);
        }
        assert_eq!(app.history.get("a").unwrap().len(), HISTORY_LEN);
    }

    #[test]
    fn stats_response_deserializes_the_real_wire_shape() {
        let json = r#"{"pipelines":[{"name":"a","bytes_in":10,"bytes_out":20,"connections_total":1,"connections_active":1,"errors_total":0}]}"#;
        let resp: StatsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.pipelines.len(), 1);
        assert_eq!(resp.pipelines[0].name, "a");
    }

    #[test]
    fn stats_response_defaults_to_an_empty_list() {
        let resp: StatsResponse = serde_json::from_str("{}").unwrap();
        assert!(resp.pipelines.is_empty());
    }
}
