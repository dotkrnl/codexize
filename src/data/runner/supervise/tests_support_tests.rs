use super::*;

fn insert_fixture_run(
    supervisor: &Supervisor,
    run_id: RunId,
    window_name: &str,
    waiting: bool,
) -> mpsc::UnboundedReceiver<AcpInput> {
    let cancel = supervisor.child_cancel_signal();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
    let (_waiting_tx, waiting_rx) = watch::channel(waiting);
    let (_finished_tx, finished_rx) = watch::channel(false);
    supervisor.inner.runs.insert(
        run_id,
        RunHandle {
            window_name: window_name.to_string(),
            cancel,
            input_tx,
            waiting_for_input: waiting_rx,
            finished: finished_rx,
            join: None,
        },
    );
    input_rx
}

#[test]
fn supervisor_targets_input_by_run_id_when_labels_match() {
    let supervisor = Supervisor::new(Arc::new(
        crate::data::config::Config::baked_defaults(),
    ));
    let mut first_rx = insert_fixture_run(&supervisor, 10, "[Duplicate]", true);
    let mut second_rx = insert_fixture_run(&supervisor, 11, "[Duplicate]", true);

    assert!(supervisor.send_run_input(11, "second".to_string()));

    assert!(first_rx.try_recv().is_err());
    match second_rx.try_recv().expect("second run input") {
        AcpInput::Prompt(text) => assert_eq!(text, "second"),
        other => panic!("expected prompt input, got {other:?}"),
    }
}
