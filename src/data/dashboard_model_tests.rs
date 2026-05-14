use super::*;

fn ipbr_score(name: &str, value: f64, order: usize) -> ScoreEntry {
    ScoreEntry {
        name: name.to_string(),
        display_order: order,
        ipbr_stage_scores: IpbrStageScores {
            idea: Some(value),
            planning: Some(value + 1.0),
            build: Some(value + 2.0),
            review: Some(value + 3.0),
        },
        score_source: ScoreSource::Ipbr,
    }
}

#[test]
fn models_surface_directly_from_ipbr_scores() {
    let result = models_from_scores(vec![
        ipbr_score("claude-opus-4.6", 91.0, 2),
        ipbr_score("gpt-5.4", 87.0, 1),
    ]);

    assert!(result.warnings.is_empty());
    assert_eq!(
        result
            .models
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-5.4", "claude-opus-4.6"]
    );
    assert_eq!(result.models[1].ipbr_stage_scores.build, Some(93.0));
}

#[test]
fn models_keep_canonical_punctuation_without_normalization() {
    let result = models_from_scores(vec![ipbr_score("claude-opus-4.6", 90.0, 0)]);

    assert_eq!(result.models.len(), 1);
    assert_eq!(result.models[0].name, "claude-opus-4.6");
}

#[test]
fn duplicate_ipbr_display_names_are_dropped_and_warned() {
    let result = models_from_scores(vec![
        ipbr_score("claude-opus-4.6", 90.0, 1),
        ipbr_score("claude-opus-4.6", 80.0, 2),
        ipbr_score("gpt-5.4", 70.0, 3),
    ]);

    assert_eq!(result.models.len(), 1);
    assert_eq!(result.models[0].name, "gpt-5.4");
    assert_eq!(result.warnings.len(), 1);
    assert!(
        result.warnings[0].contains("ipbr display_name 'claude-opus-4.6' collided"),
        "unexpected warning: {}",
        result.warnings[0]
    );
}

fn render_dashboard_models(models: &[DashboardModel]) -> String {
    let mut sorted: Vec<&DashboardModel> = models.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for model in sorted {
        out.push_str(&format!("- name: {}\n", model.name));
        out.push_str(&format!("  display_order: {}\n", model.display_order));
        out.push_str(&format!("  score_source: {:?}\n", model.score_source));
        out.push_str(&format!(
            "  ipbr_stage_scores: idea={:?} planning={:?} build={:?} review={:?}\n",
            model.ipbr_stage_scores.idea,
            model.ipbr_stage_scores.planning,
            model.ipbr_stage_scores.build,
            model.ipbr_stage_scores.review,
        ));
    }
    out
}

#[test]
fn dashboard_model_after_representative_merge_snapshot() {
    let result = models_from_scores(vec![
        ipbr_score("claude-opus-4.6", 91.0, 0),
        ipbr_score("gpt-5.4", 87.0, 1),
    ]);

    insta::assert_snapshot!(
        "dashboard_model_after_representative_merge",
        render_dashboard_models(&result.models)
    );
}
