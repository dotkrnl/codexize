use super::*;

#[test]
fn cached_row_body_populates_each_index_once() {
    let _guard = FrameGuard::enter();
    let mut count = 0;
    let body_a1 = cached_row_body(0, || {
        count += 1;
        vec![PipelineLine {
            line: Line::from("a"),
            kind: PipelineLineKind::Other,
        }]
    });
    let body_a2 = cached_row_body(0, || {
        count += 1;
        unreachable!("row 0 already cached");
    });
    let body_b = cached_row_body(1, || {
        count += 1;
        vec![PipelineLine {
            line: Line::from("b"),
            kind: PipelineLineKind::Other,
        }]
    });
    assert_eq!(count, 2);
    assert!(Rc::ptr_eq(&body_a1, &body_a2));
    assert_eq!(body_a1.len(), 1);
    assert_eq!(body_b.len(), 1);
}

#[test]
fn filtered_drops_container_placeholders_for_suppressed_runs() {
    let _guard = FrameGuard::enter();
    let lines = vec![
        PipelineLine {
            line: Line::from(""),
            kind: PipelineLineKind::Other,
        },
        PipelineLine {
            line: Line::from(""),
            kind: PipelineLineKind::RunningContainerPlaceholder { run_id: 7 },
        },
        PipelineLine {
            line: Line::from(""),
            kind: PipelineLineKind::RunningLeafTail { run_id: 7 },
        },
        PipelineLine {
            line: Line::from(""),
            kind: PipelineLineKind::RunningContainerPlaceholder { run_id: 9 },
        },
    ];
    let _populated = cached_pipeline_lines(|| lines.clone());
    let suppressed: BTreeSet<u64> = [7].into_iter().collect();
    let filtered = cached_pipeline_lines_filtered(&suppressed, || unreachable!());
    assert_eq!(filtered.len(), 3, "container placeholder for run 7 dropped");
    assert!(
        filtered
            .iter()
            .all(|line| !matches!(line.kind, PipelineLineKind::RunningContainerPlaceholder { run_id } if run_id == 7))
    );
}
