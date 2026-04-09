use std::collections::HashMap;

/// Unique identifier for a scheduled task.
pub type TaskId = u64;

/// What a task returned after being invoked.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskOutcome {
    /// Task completed or returned nil/false -- remove it.
    Done,
    /// Task wants to sleep for `seconds` before next invocation.
    Sleep(f64),
    /// Task encountered an error -- remove it and report.
    Error(String),
}

/// A scheduled task entry.
#[derive(Debug)]
pub struct TaskEntry {
    pub wake_time: f64,
    pub has_initial_args: bool,
}

/// Native task scheduler that tracks wake times and frame budgets.
///
/// This manages the scheduling *decisions* -- which tasks should run and when.
/// The actual task execution (Lua coroutine resume or Rust callback) is handled
/// by the caller.
pub struct Scheduler {
    tasks: HashMap<TaskId, TaskEntry>,
    next_id: TaskId,
    default_sleep: f64,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            tasks: HashMap::new(),
            next_id: 1,
            default_sleep: 1.0 / 30.0,
        }
    }
}

impl Scheduler {
    /// Create a scheduler with a custom default sleep interval.
    pub fn with_default_sleep(default_sleep: f64) -> Self {
        Self {
            default_sleep,
            ..Default::default()
        }
    }

    /// Register a new task. Returns its ID.
    pub fn add_task(&mut self, wake_time: f64, has_initial_args: bool) -> TaskId {
        let id = self.next_id;
        self.next_id += 1;
        self.tasks.insert(
            id,
            TaskEntry {
                wake_time,
                has_initial_args,
            },
        );
        id
    }

    /// Register a task with a specific ID (for weak-ref keyed threads).
    /// Returns false if the ID is already in use.
    pub fn add_task_with_id(&mut self, id: TaskId, wake_time: f64, has_initial_args: bool) -> bool {
        if self.tasks.contains_key(&id) {
            return false;
        }
        self.tasks.insert(
            id,
            TaskEntry {
                wake_time,
                has_initial_args,
            },
        );
        true
    }

    /// Remove a task.
    pub fn remove_task(&mut self, id: TaskId) -> bool {
        self.tasks.remove(&id).is_some()
    }

    /// Collect task IDs that are ready to run (wake_time < now).
    pub fn ready_tasks(&self, now: f64) -> Vec<TaskId> {
        let mut ready: Vec<TaskId> = self
            .tasks
            .iter()
            .filter(|(_, entry)| entry.wake_time < now)
            .map(|(id, _)| *id)
            .collect();
        ready.sort();
        ready
    }

    /// Process the outcome of running a task.
    pub fn handle_outcome(&mut self, id: TaskId, now: f64, outcome: TaskOutcome) {
        match outcome {
            TaskOutcome::Done | TaskOutcome::Error(_) => {
                self.tasks.remove(&id);
            }
            TaskOutcome::Sleep(seconds) => {
                if let Some(entry) = self.tasks.get_mut(&id) {
                    entry.wake_time = now + seconds;
                    entry.has_initial_args = false;
                }
            }
        }
    }

    /// Interpret a tick function result: nil/false -> Done, number -> Sleep.
    pub fn interpret_tick_result(&self, result: Option<f64>) -> TaskOutcome {
        match result {
            None => TaskOutcome::Done,
            Some(seconds) => TaskOutcome::Sleep(seconds),
        }
    }

    /// Interpret a coroutine resume result.
    pub fn interpret_coroutine_result(
        &self,
        ok: bool,
        yielded_seconds: Option<f64>,
        is_dead: bool,
        error_msg: Option<String>,
    ) -> TaskOutcome {
        if !ok {
            return TaskOutcome::Error(
                error_msg.unwrap_or_else(|| "unknown thread error".to_string()),
            );
        }
        if is_dead {
            return TaskOutcome::Done;
        }
        TaskOutcome::Sleep(yielded_seconds.unwrap_or(self.default_sleep))
    }

    /// Calculate the minimum time until any task wants to wake.
    pub fn min_wake_time(&self, now: f64) -> f64 {
        self.tasks
            .values()
            .map(|entry| (entry.wake_time - now).max(0.0))
            .fold(f64::INFINITY, f64::min)
    }

    /// Check if all tasks have been processed (none are past-due).
    pub fn all_processed(&self, now: f64) -> bool {
        self.tasks.values().all(|entry| entry.wake_time >= now)
    }

    /// Returns true if a task has initial args that should be passed on first resume.
    pub fn has_initial_args(&self, id: TaskId) -> bool {
        self.tasks
            .get(&id)
            .map(|e| e.has_initial_args)
            .unwrap_or(false)
    }

    /// Clear initial args flag after first resume.
    pub fn clear_initial_args(&mut self, id: TaskId) {
        if let Some(entry) = self.tasks.get_mut(&id) {
            entry.has_initial_args = false;
        }
    }

    /// Number of active tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether the scheduler has no tasks.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Clear all tasks.
    pub fn clear(&mut self) {
        self.tasks.clear();
        self.tasks.shrink_to_fit();
    }

    /// Check if running more tasks would exceed the frame budget.
    pub fn frame_budget_exceeded(frame_start: f64, now: f64, fps: f64) -> bool {
        let max_time = 1.0 / fps - 0.004;
        now - frame_start > max_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_ready() {
        let mut sched = Scheduler::default();
        let id1 = sched.add_task(0.0, false);
        let id2 = sched.add_task(10.0, false);
        let ready = sched.ready_tasks(1.0);
        assert!(ready.contains(&id1));
        assert!(!ready.contains(&id2));
    }

    #[test]
    fn handle_done_removes() {
        let mut sched = Scheduler::default();
        let id = sched.add_task(0.0, false);
        sched.handle_outcome(id, 1.0, TaskOutcome::Done);
        assert!(sched.is_empty());
    }

    #[test]
    fn handle_sleep_updates_wake() {
        let mut sched = Scheduler::default();
        let id = sched.add_task(0.0, false);
        sched.handle_outcome(id, 1.0, TaskOutcome::Sleep(0.5));
        assert_eq!(sched.len(), 1);
        let ready = sched.ready_tasks(1.0);
        assert!(ready.is_empty()); // wake_time = 1.5, now = 1.0
        let ready = sched.ready_tasks(2.0);
        assert!(ready.contains(&id)); // wake_time = 1.5, now = 2.0
    }

    #[test]
    fn handle_error_removes() {
        let mut sched = Scheduler::default();
        let id = sched.add_task(0.0, false);
        sched.handle_outcome(id, 1.0, TaskOutcome::Error("oops".into()));
        assert!(sched.is_empty());
    }

    #[test]
    fn interpret_tick_none_is_done() {
        let sched = Scheduler::default();
        assert_eq!(sched.interpret_tick_result(None), TaskOutcome::Done);
    }

    #[test]
    fn interpret_tick_some_is_sleep() {
        let sched = Scheduler::default();
        assert_eq!(
            sched.interpret_tick_result(Some(0.1)),
            TaskOutcome::Sleep(0.1)
        );
    }

    #[test]
    fn interpret_coroutine_error() {
        let sched = Scheduler::default();
        let outcome = sched.interpret_coroutine_result(false, None, false, Some("bad".into()));
        assert_eq!(outcome, TaskOutcome::Error("bad".into()));
    }

    #[test]
    fn interpret_coroutine_dead() {
        let sched = Scheduler::default();
        let outcome = sched.interpret_coroutine_result(true, None, true, None);
        assert_eq!(outcome, TaskOutcome::Done);
    }

    #[test]
    fn interpret_coroutine_yield() {
        let sched = Scheduler::default();
        let outcome = sched.interpret_coroutine_result(true, Some(0.5), false, None);
        assert_eq!(outcome, TaskOutcome::Sleep(0.5));
    }

    #[test]
    fn interpret_coroutine_yield_default() {
        let sched = Scheduler::with_default_sleep(1.0 / 30.0);
        let outcome = sched.interpret_coroutine_result(true, None, false, None);
        if let TaskOutcome::Sleep(s) = outcome {
            assert!((s - 1.0 / 30.0).abs() < 1e-10);
        } else {
            panic!("expected Sleep");
        }
    }

    #[test]
    fn min_wake_time_respects_now() {
        let mut sched = Scheduler::default();
        sched.add_task(2.0, false);
        sched.add_task(3.0, false);
        assert!((sched.min_wake_time(1.0) - 1.0).abs() < 1e-10);
        assert!((sched.min_wake_time(2.5) - 0.0).abs() < 1e-10); // 2.0 < 2.5, clamped to 0
    }

    #[test]
    fn add_with_id_rejects_duplicate() {
        let mut sched = Scheduler::default();
        assert!(sched.add_task_with_id(42, 0.0, false));
        assert!(!sched.add_task_with_id(42, 0.0, false));
    }

    #[test]
    fn initial_args_tracking() {
        let mut sched = Scheduler::default();
        let id = sched.add_task(0.0, true);
        assert!(sched.has_initial_args(id));
        sched.clear_initial_args(id);
        assert!(!sched.has_initial_args(id));
    }

    #[test]
    fn frame_budget_check() {
        assert!(!Scheduler::frame_budget_exceeded(0.0, 0.005, 60.0));
        assert!(Scheduler::frame_budget_exceeded(0.0, 0.02, 60.0));
    }
}
