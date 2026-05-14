use crate::data::dashboard_io::DashboardModel;
use crate::selection::{IpbrStageScores, ScoreSource};
use std::collections::{BTreeMap, BTreeSet};

/// Canonical score record produced by ipbr ingestion. `name` is the
/// canonical model name from ipbr `display_name`, lowercased only for
/// stable config matching. Provider entries must use this same `model`
/// value; CLI-specific names belong in `ProviderEntry.launch_name`.
#[derive(Debug, Clone)]
pub(crate) struct ScoreEntry {
    pub(crate) name: String,
    pub(crate) display_order: usize,
    pub(crate) ipbr_stage_scores: IpbrStageScores,
    pub(crate) score_source: ScoreSource,
}

#[derive(Debug, Clone)]
pub(crate) struct MergeResult {
    pub(crate) models: Vec<DashboardModel>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn models_from_scores(scores: Vec<ScoreEntry>) -> MergeResult {
    let collisions = duplicated_display_names(&scores);
    let mut models = scores
        .into_iter()
        .enumerate()
        .filter_map(|(index, score)| {
            (!collisions.values().any(|indexes| indexes.contains(&index)))
                .then(|| dashboard_model_from_score(score))
        })
        .collect::<Vec<_>>();
    models.sort_by_key(|m| m.display_order);

    let warnings = collisions
        .into_iter()
        .map(|(name, row_indexes)| {
            let rows = row_indexes
                .into_iter()
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("ipbr display_name '{name}' collided across rows: {rows}; rows ignored")
        })
        .collect();

    MergeResult { models, warnings }
}

fn dashboard_model_from_score(score: ScoreEntry) -> DashboardModel {
    DashboardModel {
        name: score.name,
        ipbr_stage_scores: score.ipbr_stage_scores,
        score_source: score.score_source,
        display_order: score.display_order,
    }
}

fn duplicated_display_names(scores: &[ScoreEntry]) -> BTreeMap<String, BTreeSet<usize>> {
    let mut owners = BTreeMap::<String, usize>::new();
    let mut collisions = BTreeMap::<String, BTreeSet<usize>>::new();
    for (index, score) in scores.iter().enumerate() {
        match owners.get(&score.name).copied() {
            Some(owner) if owner != index => {
                collisions
                    .entry(score.name.clone())
                    .or_default()
                    .extend([owner, index]);
            }
            Some(_) => {}
            None => {
                owners.insert(score.name.clone(), index);
            }
        }
    }
    collisions
}

#[cfg(test)]
#[path = "dashboard_model_tests.rs"]
mod tests;
