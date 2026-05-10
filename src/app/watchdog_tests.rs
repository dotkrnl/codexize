use super::*;
use tokio::time::{Instant, advance};

fn fresh(effort: EffortLevel) -> (Instant, WatchdogState) {
    let now = Instant::now();
    (now, WatchdogState::new(effort, now))
}

#[tokio::test(start_paused = true)]
async fn idle_elapsed_is_zero_at_launch() {
    let (now, state) = fresh(EffortLevel::Normal);
    advance(Duration::ZERO).await;
    assert_eq!(state.idle_elapsed(now), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn idle_elapsed_tracks_wall_clock() {
    let (_, state) = fresh(EffortLevel::Normal);
    advance(Duration::from_secs(7 * 60)).await;
    assert_eq!(
        state.idle_elapsed(Instant::now()),
        Duration::from_secs(7 * 60)
    );
}

#[tokio::test(start_paused = true)]
async fn live_summary_event_resets_idle_clock() {
    let (_, mut state) = fresh(EffortLevel::Normal);
    advance(Duration::from_secs(8 * 60)).await;
    state.on_live_summary_event(Instant::now());
    advance(Duration::from_secs(2 * 60)).await;
    assert_eq!(
        state.idle_elapsed(Instant::now()),
        Duration::from_secs(2 * 60)
    );
}

#[tokio::test(start_paused = true)]
async fn warn_threshold_emits_once_then_idle_until_kill() {
    let (_, mut state) = fresh(EffortLevel::Normal);
    advance(Duration::from_secs(9 * 60)).await;
    assert_eq!(state.evaluate(Instant::now()), WatchdogDecision::Idle);
    advance(Duration::from_secs(2 * 60)).await;
    assert_eq!(
        state.evaluate(Instant::now()),
        WatchdogDecision::EmitWarning
    );
    assert!(state.warned);
    advance(Duration::from_secs(4 * 60)).await;
    assert_eq!(state.evaluate(Instant::now()), WatchdogDecision::Idle);
}

#[tokio::test(start_paused = true)]
async fn direct_kill_without_prior_warning_when_starved() {
    let (_, mut state) = fresh(EffortLevel::Normal);
    advance(Duration::from_secs(21 * 60)).await;
    let decision = state.evaluate(Instant::now());
    assert_eq!(decision, WatchdogDecision::EmitKill);
    assert!(!state.warned);
}

#[tokio::test(start_paused = true)]
async fn warn_followed_by_kill_when_idle_continues() {
    let (_, mut state) = fresh(EffortLevel::Normal);
    advance(Duration::from_secs(11 * 60)).await;
    assert_eq!(
        state.evaluate(Instant::now()),
        WatchdogDecision::EmitWarning
    );
    advance(Duration::from_secs(10 * 60)).await;
    assert_eq!(state.evaluate(Instant::now()), WatchdogDecision::EmitKill);
}

#[tokio::test(start_paused = true)]
async fn tough_thresholds_are_15_30_minutes() {
    advance(Duration::ZERO).await;
    assert_eq!(warn_after(EffortLevel::Tough), Duration::from_secs(15 * 60));
    assert_eq!(kill_after(EffortLevel::Tough), Duration::from_secs(30 * 60));
    assert_eq!(
        warn_after(EffortLevel::Normal),
        Duration::from_secs(10 * 60)
    );
    assert_eq!(
        kill_after(EffortLevel::Normal),
        Duration::from_secs(20 * 60)
    );
    assert_eq!(warn_after(EffortLevel::Low), Duration::from_secs(10 * 60));
    assert_eq!(kill_after(EffortLevel::Low), Duration::from_secs(20 * 60));
}

#[tokio::test(start_paused = true)]
async fn tough_run_does_not_warn_at_normal_threshold() {
    let (_, mut state) = fresh(EffortLevel::Tough);
    advance(Duration::from_secs(11 * 60)).await;
    assert_eq!(state.evaluate(Instant::now()), WatchdogDecision::Idle);
    advance(Duration::from_secs(5 * 60)).await;
    assert_eq!(
        state.evaluate(Instant::now()),
        WatchdogDecision::EmitWarning
    );
    advance(Duration::from_secs(15 * 60)).await;
    assert_eq!(state.evaluate(Instant::now()), WatchdogDecision::EmitKill);
}

#[tokio::test(start_paused = true)]
async fn registry_register_get_remove_roundtrip() {
    let mut registry = WatchdogRegistry::new();
    assert!(registry.is_empty());

    let now = Instant::now();
    advance(Duration::ZERO).await;
    registry.register(
        42,
        EffortLevel::Tough,
        PathBuf::from("/tmp/prompts/coder.md"),
        now,
    );
    assert_eq!(registry.len(), 1);
    let stored = registry.get(42).expect("registered");
    assert_eq!(stored.prompt_path.to_str(), Some("/tmp/prompts/coder.md"));
    assert_eq!(stored.warn_threshold, Duration::from_secs(15 * 60));
    assert_eq!(stored.kill_threshold, Duration::from_secs(30 * 60));
    assert_eq!(stored.warning_remaining_minutes, 15);

    registry.remove(42).expect("was inserted");
    assert!(registry.is_empty());
    assert!(registry.remove(42).is_none());
}

#[tokio::test(start_paused = true)]
async fn registry_iter_mut_visits_all_states() {
    let mut registry = WatchdogRegistry::new();
    let now = Instant::now();
    registry.register(1, EffortLevel::Normal, PathBuf::from("/a"), now);
    registry.register(2, EffortLevel::Tough, PathBuf::from("/b"), now);

    advance(Duration::from_secs(60)).await;
    for state in registry.iter_mut() {
        state.on_live_summary_event(Instant::now());
    }
    for id in [1, 2] {
        assert_eq!(
            registry
                .get(id)
                .map(|s| s.last_live_summary_event - now)
                .unwrap_or_default(),
            Duration::from_secs(60)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn registry_uses_paused_tokio_time_for_thresholds() {
    let mut registry = WatchdogRegistry::new();
    let now = Instant::now();
    registry.register(1, EffortLevel::Normal, PathBuf::from("/p"), now);
    advance(WARN_AFTER_NORMAL - Duration::from_secs(1)).await;
    assert_eq!(
        registry.evaluate_all(Instant::now())[0].1,
        WatchdogDecision::Idle
    );
    advance(Duration::from_secs(1)).await;
    assert_eq!(
        registry.evaluate_all(Instant::now())[0].1,
        WatchdogDecision::EmitWarning
    );
}

#[tokio::test(start_paused = true)]
async fn registry_evaluate_all_reports_per_run_decisions() {
    let mut registry = WatchdogRegistry::new();
    let now = Instant::now();
    registry.register(1, EffortLevel::Normal, PathBuf::from("/a"), now);
    registry.register(2, EffortLevel::Tough, PathBuf::from("/b"), now);

    advance(Duration::from_secs(11 * 60)).await;
    let mut decisions = registry.evaluate_all(Instant::now());
    decisions.sort_by_key(|(id, _)| *id);
    // Run 1 (Normal) crosses warn at 10 min; run 2 (Tough) does not.
    assert_eq!(decisions[0].1, WatchdogDecision::EmitWarning);
    assert_eq!(decisions[1].1, WatchdogDecision::Idle);
}

#[tokio::test(start_paused = true)]
async fn warning_text_contains_prompt_verbatim_and_minute_counts() {
    advance(Duration::ZERO).await;
    let body = "Original instructions for the agent.";
    let text = warning_text(11, 9, body);
    assert!(text.contains("11 minutes"));
    assert!(text.contains("9 minutes"));
    assert!(text.contains(body));
    assert!(text.contains("ORIGINAL PROMPT"));
    assert!(text.contains("END ORIGINAL PROMPT"));
    // The exact prompt body — including any later lines — is sandwiched
    // between the markers (AC7).
    let start = text.find("ORIGINAL PROMPT").unwrap();
    let end = text.find("END ORIGINAL PROMPT").unwrap();
    assert!(text[start..end].contains(body));
}

#[tokio::test(start_paused = true)]
async fn idle_minutes_for_message_tracks_paused_tokio_time() {
    let mut registry = WatchdogRegistry::new();
    let now = Instant::now();
    registry.register(1, EffortLevel::Normal, PathBuf::from("/p"), now);
    advance(Duration::from_secs(11 * 60)).await;
    let state = registry.get_mut(1).expect("registered");
    assert_eq!(state.idle_minutes_for_message(Instant::now()), 11);
}
