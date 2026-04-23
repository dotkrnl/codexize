use anyhow::{Result, bail};
use std::{fs, path::PathBuf};
use crate::state;

pub fn validate_phase_artifacts(run_id: &str, phase: state::Phase) -> Result<()> {
    let dir = state::run_dir(run_id).join("artifacts");
    
    match phase {
        state::Phase::IdeaInput => {
            validate_exists(&dir.join("idea.md"))?;
        }
        state::Phase::BrainstormRunning => {
            validate_exists(&dir.join("spec.md"))?;
        }
        state::Phase::SpecReviewRunning => {
            validate_exists(&dir.join("spec-review.md"))?;
        }
        state::Phase::PlanningRunning => {
            validate_exists(&dir.join("plan.md"))?;
        }
        state::Phase::ImplementationRound(r) => {
            let round_dir = state::run_dir(run_id).join("rounds").join(format!("{:03}", r));
            validate_exists(&round_dir.join("commit.txt"))?;
        }
        state::Phase::ReviewRound(r) => {
            let round_dir = state::run_dir(run_id).join("rounds").join(format!("{:03}", r));
            validate_exists(&round_dir.join("review.md"))?;
        }
        _ => {}
    }
    
    Ok(())
}

fn validate_exists(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        bail!("required artifact missing: {}", path.display());
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() == 0 {
        bail!("required artifact is empty: {}", path.display());
    }
    Ok(())
}
